//! Per-model ready queues with independent session owners and per-page result routing.
use crate::pp_doclayout_v3::{preprocess::ModelInputs, session::ModelOutputs};
use crate::{LayoutError, ModelArtifacts, ModelSchema};
use docparse_common::timing::TimingContext;
use docparse_common::timing::{StageTimer, TimingStage, Timings};
use docparse_config::ValidatedConfig;
use ort::session::{OutputSelector, RunOptions};
use ort::value::TensorRef;
use std::sync::Arc;
use tokio::sync::oneshot;

/// A page retains its result channel and original attribution across shared physical batches.
#[derive(typed_builder::TypedBuilder)]
struct Request {
    // Keep the render delivery occupied until actual inference and input cleanup finish.
    #[builder(default = docparse_common::PageLease::current())]
    _page_lease: Option<docparse_common::PageLease>,
    inputs: ModelInputs,
    response: oneshot::Sender<Result<ModelOutputs, LayoutError>>,
    context: TimingContext,
    #[builder(default)]
    queued: Option<StageTimer>,
    #[builder(default)]
    caller: Option<oneshot::Sender<()>>,
}

impl Request {
    /// Blocking replies need the original async lifetime in addition to receiver cancellation.
    fn cancelled(&self) -> bool {
        self.response.is_closed()
            || self.caller.as_ref().is_some_and(oneshot::Sender::is_closed)
    }
    /// Records admission under the original caller's tracing subscriber.
    fn end_queue(&mut self) {
        let queued = self.queued.take();
        self.context.in_scope(|| drop(queued));
    }
    /// Routes batch results back to individual pages without changing their order or geometry.
    fn complete(
        requests: Vec<Self>,
        result: Result<Vec<ModelOutputs>, LayoutError>,
        timers: Vec<StageTimer>,
    ) {
        let mut outputs = result.map(Vec::into_iter);
        for (request, timer) in requests.into_iter().zip(timers) {
            let output = match &mut outputs {
                Ok(outputs) => outputs.next().ok_or(LayoutError::Engine {
                    message: "layout batch result count mismatch".into(),
                }),
                Err(error) => Err(LayoutError::Engine {
                    message: error.to_string(),
                }),
            };
            request.context.in_scope(|| {
                drop(timer);
                let _ = request.response.send(output);
            });
        }
    }
}

impl LayoutSessionPool {
    /// Enqueues one prepared page; sessions choose batches from all waiting callers.
    pub(crate) async fn run(
        self: Arc<Self>,
        inputs: ModelInputs,
        timings: Timings,
    ) -> Result<ModelOutputs, LayoutError> {
        let (response, receiver) = oneshot::channel();
        let request = Request::builder()
            .inputs(inputs)
            .response(response)
            .queued(Some(timings.start(TimingStage::LayoutQueue)))
            .context(TimingContext::new(timings))
            .build();
        self.submit(request, receiver).await
    }
}

impl docparse_common::SessionRequest for Request {
    /// Excludes canceled pages from the shared ready batch.
    fn cancelled(&self) -> bool {
        self.cancelled()
    }
    /// Restores per-page queue attribution before inference.
    fn end_queue(&mut self) {
        self.end_queue();
    }
}

#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
mod platform {
    use super::*;
    use docparse_common::{SessionManager, run_cpu};
    use ort::session::{HasSelectedOutputs, Session};

    /// One mutable native ORT session held by a unique lease.
    struct LayoutSession {
        session: Session,
        options: RunOptions<HasSelectedOutputs>,
    }

    impl LayoutSession {
        /// Creates one native session from the same bytes that passed artifact verification.
        fn load(
            bytes: &[u8],
            backend: crate::wasm_compat::OnnxBackend,
        ) -> Result<Self, LayoutError> {
            let mut builder =
                ort::session::builder::SessionBuilder::try_from(backend)?;
            let session = builder.commit_from_memory(bytes)?;
            ModelSchema::from_session(&session)?.validate_pp_doclayout_v3()?;
            let options = RunOptions::new()?.with_outputs(
                OutputSelector::no_default()
                    .with("fetch_name_0")
                    .with("fetch_name_1"),
            );
            Ok(Self { session, options })
        }

        /// Executes a ready page batch and separates outputs before releasing the session.
        fn run(
            &mut self,
            inputs: &ModelInputs,
            batch: usize,
        ) -> Result<Vec<ModelOutputs>, LayoutError> {
            let physical = docparse_common::telemetry::Inference::new(
                "layout", "model", batch,
            );
            let outputs = self.session.run_with_options(ort::inputs! {
                "im_shape" => TensorRef::from_array_view(&inputs.image_size)?,
                "image" => TensorRef::from_array_view(&inputs.image)?,
                "scale_factor" => TensorRef::from_array_view(&inputs.scale_factor)?,
            }, &self.options);
            physical.finish(outputs.is_ok());
            let outputs = outputs?;
            ModelOutputs::from_batch(&outputs, batch)
        }
    }

    /// Native owners hold separate sessions and consume a single shared queue.
    pub(crate) struct LayoutSessionPool {
        manager: Arc<SessionManager<Request>>,
    }

    impl LayoutSessionPool {
        /// Builds the configured consumers once, sharing verified model bytes during initialization.
        pub(crate) async fn load(
            artifacts: ModelArtifacts,
            config: Arc<ValidatedConfig>,
        ) -> Result<Arc<Self>, LayoutError> {
            let backend =
                crate::wasm_compat::OnnxBackend::from(config.as_ref());
            let manager = SessionManager::load(
                "layout",
                config.layout().session_size,
                config.layout().batch_size,
                config.layout().queue_size,
                move || {
                    let mut session =
                        LayoutSession::load(&artifacts.model, backend)?;
                    Ok::<_, LayoutError>(move |requests: Vec<Request>| {
                        let timers = requests
                            .iter()
                            .map(|request| {
                                request
                                    .context
                                    .timings
                                    .start(TimingStage::LayoutInference)
                            })
                            .collect();
                        let result = ModelInputs::try_from(
                            requests
                                .iter()
                                .map(|request| &request.inputs)
                                .collect::<Vec<_>>()
                                .as_slice(),
                        )
                        .and_then(|inputs| {
                            session.run(&inputs, requests.len())
                        });
                        Request::complete(requests, result, timers);
                    })
                },
            )
            .await?;
            Ok(Arc::new(Self { manager }))
        }
        /// Holds the manager off session threads until even canceled native inference releases its input.
        pub(super) async fn submit(
            self: Arc<Self>,
            mut request: Request,
            receiver: oneshot::Receiver<Result<ModelOutputs, LayoutError>>,
        ) -> Result<ModelOutputs, LayoutError> {
            let (caller, _lifetime) = oneshot::channel();
            request.caller = Some(caller);
            run_cpu(move || {
                self.manager
                    .send(request)
                    .map_err(|source| LayoutError::TaskJoin { source })?;
                receiver.blocking_recv().map_err(|error| {
                    LayoutError::Engine {
                        message: error.to_string(),
                    }
                })?
            })
            .await
            .map_err(|source| LayoutError::TaskJoin { source })?
        }
    }
    #[cfg(test)]
    mod tests {
        /// Real batched outputs must split into the same pages, and two consumers must survive runtime replacement.
        #[test]
        #[ignore = "requires downloaded PP-DocLayoutV3 artifacts"]
        fn real_batches_and_sessions_preserve_page_outputs() {
            use super::*;
            use crate::{
                AffineTransform, PageImage, PageImageInput, PageRotation,
                PageTransform, PageTransformInput, PixelFormat,
            };
            let root =
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
            let mut raw =
                docparse_config::ConfigLoader::new(root.join("docparse.toml"))
                    .load_raw()
                    .expect("config");
            raw.layout.session_size = 2;
            raw.layout.batch_size = 3;
            let config =
                Arc::new(ValidatedConfig::try_from(raw).expect("valid config"));
            let artifacts = ModelArtifacts::from_paths(
                &config.layout().model_path,
                &config.layout().model_config_path,
                &config.layout().model_manifest_path,
            )
            .expect("artifacts");
            let mut inputs = Vec::new();
            for name in ["portrait.png", "landscape.png", "blank.png"] {
                let image = image::open(
                    root.join("crates/layout/tests/fixtures/model").join(name),
                )
                .expect("fixture")
                .into_rgb8();
                let (width, height) = image.dimensions();
                let page = PageImage::try_from(
                    PageImageInput::builder()
                        .width(width)
                        .height(height)
                        .pixel_format(PixelFormat::Rgb8)
                        .data(Arc::from(image.into_raw()))
                        .build(),
                )
                .expect("page");
                let transform = PageTransform::try_from(
                    PageTransformInput::builder()
                        .page_to_viewport(AffineTransform::identity())
                        .viewport_width(f64::from(width))
                        .viewport_height(f64::from(height))
                        .render_width(width)
                        .render_height(height)
                        .model_width(800)
                        .model_height(800)
                        .rotation(PageRotation::Degrees0)
                        .build(),
                )
                .expect("transform");
                inputs.push(
                    crate::pp_doclayout_v3::preprocess::preprocess(
                        &page, &transform,
                    )
                    .expect("input"),
                );
            }
            let mut session = LayoutSession::load(
                &artifacts.model,
                crate::wasm_compat::OnnxBackend::from(config.as_ref()),
            )
            .expect("session");
            let expected: Vec<_> = inputs
                .iter()
                .map(|input| {
                    session
                        .run(input, 1)
                        .expect("singleton")
                        .pop()
                        .expect("page")
                })
                .collect();
            let merged = ModelInputs::try_from(
                inputs.iter().collect::<Vec<_>>().as_slice(),
            )
            .expect("merged input");
            let actual = session.run(&merged, 3).expect("physical batch");
            for (actual, expected) in actual.iter().zip(&expected) {
                assert_eq!(actual.count, expected.count);
                assert_eq!(actual.boxes.dim(), expected.boxes.dim());
                assert!(
                    actual
                        .boxes
                        .iter()
                        .zip(&expected.boxes)
                        .all(|(a, b)| (a - b).abs() < 0.01),
                    "batch changed page output"
                );
            }
            drop(session);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("runtime");
            let pool = runtime
                .block_on(LayoutSessionPool::load(artifacts, config))
                .expect("shared sessions");
            drop(runtime);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("replacement runtime");
            let mut tasks = tokio::task::JoinSet::new();
            runtime.block_on(async {
                for (index, input) in inputs.into_iter().enumerate() {
                    let pool = Arc::clone(&pool);
                    tasks.spawn(async move {
                        (index, pool.run(input, Timings::default()).await)
                    });
                }
                while let Some(result) = tasks.join_next().await {
                    let (index, actual) = result.expect("task");
                    let actual = actual.expect("queued page");
                    let expected = expected.get(index).expect("reference");
                    assert_eq!(actual.count, expected.count);
                    assert!(
                        actual
                            .boxes
                            .iter()
                            .zip(&expected.boxes)
                            .all(|(a, b)| (a - b).abs() < 0.01)
                    );
                }
            });
        }
        #[cfg(all(feature = "coreml", target_os = "macos"))]
        use std::path::{Path, PathBuf};

        /// Resolves a repository path from the layout crate directory.
        #[cfg(all(feature = "coreml", target_os = "macos"))]
        fn repository_path(relative: &str) -> PathBuf {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(relative)
        }

        /// Compares CoreML options on identical production-preprocessed images before changing defaults.
        #[cfg(all(feature = "coreml", target_os = "macos"))]
        #[test]
        #[ignore = "requires macOS CoreML, fixed model, DOCPARSE_COREML_PROBE_CASE and DOCPARSE_COREML_PROBE_OUTPUT"]
        fn coreml_configuration_probe() -> Result<(), Box<dyn std::error::Error>>
        {
            use crate::pp_doclayout_v3::{
                postprocess::postprocess_page, preprocess::preprocess,
            };
            use crate::{
                AffineTransform, PageImage, PageImageInput, PageRotation,
                PageTransform, PageTransformInput, PixelFormat,
            };
            use ort::ep::coreml::{
                ComputeUnits, ModelFormat, SpecializationStrategy,
            };
            use ort::session::{OutputSelector, RunOptions};
            use std::{sync::Arc, time::Instant};

            let oracle: serde_json::Value =
                serde_json::from_slice(&std::fs::read(repository_path(
                    "crates/layout/tests/fixtures/model/python_outputs.json",
                ))?)?;
            let mut samples = Vec::new();
            for sample in oracle
                .get("samples")
                .and_then(serde_json::Value::as_array)
                .ok_or("missing samples")?
            {
                let name = sample
                    .get("input")
                    .and_then(|input| input.get("basename"))
                    .and_then(serde_json::Value::as_str)
                    .ok_or("missing basename")?;
                let image = image::open(repository_path(&format!(
                    "crates/layout/tests/fixtures/model/{name}"
                )))?
                .into_rgb8();
                let (width, height) = image.dimensions();
                let page = PageImage::try_from(
                    PageImageInput::builder()
                        .width(width)
                        .height(height)
                        .pixel_format(PixelFormat::Rgb8)
                        .data(Arc::from(image.into_raw()))
                        .build(),
                )?;
                let transform = PageTransform::try_from(
                    PageTransformInput::builder()
                        .page_to_viewport(AffineTransform::identity())
                        .viewport_width(f64::from(width))
                        .viewport_height(f64::from(height))
                        .render_width(width)
                        .render_height(height)
                        .model_width(800)
                        .model_height(800)
                        .rotation(PageRotation::Degrees0)
                        .build(),
                )?;
                samples.push((
                    name.to_owned(),
                    preprocess(&page, &transform)?,
                    transform,
                ));
            }
            let model = std::fs::read(repository_path(
                "models/pp-doclayout-v3/inference.onnx",
            ))?;
            let output =
                PathBuf::from(std::env::var("DOCPARSE_COREML_PROBE_OUTPUT")?);
            let selected = std::env::var("DOCPARSE_COREML_PROBE_CASE")?;
            let mut reports = Vec::new();
            let cases = [
                "legacy",
                "mlprogram",
                "legacy-static",
                "legacy-fast",
                "legacy-static-fast",
                "legacy-fp16",
                "legacy-gpu",
                "legacy-ane",
                "legacy-threads1",
                "legacy-threads2",
                "legacy-threads4",
                "legacy-fast-threads1",
                "legacy-no-spin",
                "legacy-pruned",
                "legacy-fast-pruned",
            ];
            if !cases.contains(&selected.as_str()) {
                return Err(
                    format!("unknown CoreML probe case {selected}").into()
                );
            }
            for name in cases {
                if selected != name {
                    continue;
                }
                eprintln!("loading CoreML probe {name}");
                let loading = Instant::now();
                let mut provider = ort::ep::CoreML::default()
                    .with_compute_units(if name.ends_with("gpu") {
                        ComputeUnits::CPUAndGPU
                    } else if name.ends_with("ane") {
                        ComputeUnits::CPUAndNeuralEngine
                    } else {
                        ComputeUnits::All
                    });
                if !name.starts_with("legacy") {
                    provider =
                        provider.with_model_format(ModelFormat::MLProgram);
                }
                if name.contains("fast") {
                    provider = provider.with_specialization_strategy(
                        SpecializationStrategy::FastPrediction,
                    );
                }
                if name.ends_with("fp16") {
                    provider =
                        provider.with_low_precision_accumulation_on_gpu(true);
                }
                // The probe varies provider options while retaining the shared default session policy.
                let mut builder = OnnxBackend::compiled()
                    .cpu_builder()?
                    .with_execution_providers([provider
                        .build()
                        .error_on_failure()])?;
                if let Some((_, threads)) = name.split_once("threads") {
                    builder = builder.with_intra_threads(threads.parse()?)?;
                }
                if name == "legacy-no-spin" {
                    builder = builder.with_config_entry(
                        "session.intra_op.allow_spinning",
                        "0",
                    )?;
                }
                if name.contains("static") {
                    for symbol in [
                        "DynamicDimension.0",
                        "DynamicDimension.1",
                        "DynamicDimension.2",
                    ] {
                        builder = builder.with_dimension_override(symbol, 1)?;
                    }
                }
                let native = if name.ends_with("pruned") {
                    use ort::editor::{Graph, Model, ONNX_DOMAIN, Opset};
                    let mut editable = builder.edit_from_memory(&model)?;
                    crate::ModelSchema::from_session(&editable)?
                        .validate_pp_doclayout_v3()?;
                    let mut graph = Graph::new()?;
                    graph.set_outputs(
                        editable.outputs().iter().take(2).map(|outlet| {
                            ort::value::Outlet::new(
                                outlet.name(),
                                outlet.dtype().clone(),
                            )
                        }),
                    )?;
                    let mut update = Model::new([Opset::new(
                        ONNX_DOMAIN,
                        editable
                            .opset_for_domain(ONNX_DOMAIN)
                            .ok_or("missing ONNX opset")?,
                    )?])?;
                    update.add_graph(graph)?;
                    editable.apply_model(&update)?;
                    editable.into_session()?
                } else {
                    builder.commit_from_memory(&model)?
                };
                let mut session = super::LayoutSession {
                    session: native,
                    options: RunOptions::new()?.with_outputs(
                        OutputSelector::no_default()
                            .with("fetch_name_0")
                            .with("fetch_name_1"),
                    ),
                };
                let schema =
                    crate::ModelSchema::from_session(&session.session)?;
                let loading_seconds = loading.elapsed().as_secs_f64();
                // Exercise every image twice before collecting repeated runtime intervals.
                let warming = Instant::now();
                for _ in 0..2 {
                    for (_, input, _) in &samples {
                        session.run(input, 1)?;
                    }
                }
                let warmup_seconds = warming.elapsed().as_secs_f64();
                let mut records = Vec::<serde_json::Value>::new();
                for repeat in 0..3 {
                    for (sample_index, (sample, input, transform)) in
                        samples.iter().enumerate()
                    {
                        let started = Instant::now();
                        let raw = session
                            .run(input, 1)?
                            .pop()
                            .ok_or("missing layout result")?;
                        let seconds = started.elapsed().as_secs_f64();
                        let detections = postprocess_page(
                            raw.boxes.view(),
                            raw.count,
                            0.5,
                            transform,
                        )?;
                        if repeat > 0 {
                            let expected = records
                                .get(sample_index)
                                .and_then(|record| record.get("detections"))
                                .ok_or("missing first-repeat detections")?;
                            if &serde_json::to_value(&detections)? != expected {
                                return Err(format!("CoreML {name} changed {sample} between identical repeated inputs").into());
                            }
                        }
                        records.push(serde_json::json!({"sample":sample,"repeat":repeat,"seconds":seconds,"detections":detections}));
                    }
                }
                eprintln!(
                    "CoreML probe {name}: load {loading_seconds:.3}s, warmup {warmup_seconds:.3}s"
                );
                reports.push(serde_json::json!({"case":name,"loading_seconds":loading_seconds,"warmup_seconds":warmup_seconds,"schema":schema,"records":records}));
                std::fs::write(&output, serde_json::to_vec_pretty(&reports)?)?;
            }
            Ok(())
        }
    }
}

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
mod platform {
    use super::*;

    /// Browser consumers share one input queue while ORT execution remains guarded globally.
    pub(crate) struct LayoutSessionPool {
        sender: docparse_common::queue::QueueSender<Request>,
    }

    impl LayoutSessionPool {
        /// Creates separate sessions after the embedding host initializes ORT Web.
        pub(crate) async fn load(
            artifacts: ModelArtifacts,
            config: Arc<ValidatedConfig>,
        ) -> Result<Arc<Self>, LayoutError> {
            let batch_size = config.layout().batch_size;
            let (sender, receiver) = docparse_common::Queue::<Request>::new(
                "layout",
                config.layout().queue_size,
            );
            for _ in 0..config.layout().session_size {
                let mut session =
                    ort::session::builder::SessionBuilder::try_from(
                        crate::wasm_compat::OnnxBackend::from(config.as_ref()),
                    )?
                    .commit_from_memory(&artifacts.model)
                    .await?;
                ModelSchema::from_session(&session)?
                    .validate_pp_doclayout_v3()?;
                let options = RunOptions::new()?.with_outputs(
                    OutputSelector::no_default()
                        .with("fetch_name_0")
                        .with("fetch_name_1"),
                );
                let receiver = receiver.clone();
                let _worker = docparse_common::ThreadManager::spawn_async(
                    Box::pin(async move {
                        while let Some(batch) = receiver.recv().await {
                            let _guard =
                            crate::wasm_compat::OnnxBackend::inference_guard()
                                .await;
                            let requests = batch.take_ready(batch_size);
                            if requests.is_empty() {
                                continue;
                            }
                            let timers = requests
                                .iter()
                                .map(|request| {
                                    request
                                        .context
                                        .timings
                                        .start(TimingStage::LayoutInference)
                                })
                                .collect();
                            let result = async {
                            let inputs = ModelInputs::try_from(requests.iter().map(|request| &request.inputs).collect::<Vec<_>>().as_slice())?;
                            let mut outputs = session.run_async(ort::inputs! {
                                "im_shape" => TensorRef::from_array_view(&inputs.image_size)?,
                                "image" => TensorRef::from_array_view(&inputs.image)?,
                                "scale_factor" => TensorRef::from_array_view(&inputs.scale_factor)?,
                            }, &options).await?;
                            ort_web::sync_outputs(&mut outputs).await.map_err(|error| LayoutError::Engine { message: error.to_string() })?;
                            ModelOutputs::from_batch(&outputs, requests.len())
                        }.await;
                            Request::complete(requests, result, timers);
                        }
                    }),
                )?;
            }
            Ok(Arc::new(Self { sender }))
        }
        /// Retains input ownership through the JavaScript promise after the original caller cancels.
        pub(super) async fn submit(
            self: Arc<Self>,
            request: Request,
            receiver: oneshot::Receiver<Result<ModelOutputs, LayoutError>>,
        ) -> Result<ModelOutputs, LayoutError> {
            self.sender.send(request).await.map_err(|error| {
                LayoutError::Engine {
                    message: error.to_string(),
                }
            })?;
            receiver.await.map_err(|error| LayoutError::Engine {
                message: error.to_string(),
            })?
        }
    }
}

pub(crate) use platform::LayoutSessionPool;
