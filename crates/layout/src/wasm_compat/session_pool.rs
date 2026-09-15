//! Native session leases and the browser inference actor behind one pool interface.
use crate::pp_doclayout_v3::{preprocess::ModelInputs, session::ModelOutputs};
use crate::timing::{TimingStage, Timings};
use crate::{LayoutError, ModelArtifacts, ModelSchema};
use docparse_config::ValidatedConfig;
use ort::session::{OutputSelector, RunOptions};
use ort::value::TensorRef;
use std::sync::Arc;

#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
mod platform {
    use super::*;
    use crate::wasm_compat::SessionWorker;
    use ort::session::{HasSelectedOutputs, Session};
    use std::sync::Mutex;
    use tokio::sync::{OwnedSemaphorePermit, Semaphore};

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

        /// Executes the three-input graph and copies only the two consumed outputs.
        fn run(
            &mut self,
            inputs: &ModelInputs,
            timings: &Timings,
        ) -> Result<ModelOutputs, LayoutError> {
            let inference = timings.start(TimingStage::LayoutInference);
            let outputs = self.session.run_with_options(ort::inputs! {
                "im_shape" => TensorRef::from_array_view(&inputs.image_size)?,
                "image" => TensorRef::from_array_view(&inputs.image)?,
                "scale_factor" => TensorRef::from_array_view(&inputs.scale_factor)?,
            }, &self.options)?;
            drop(inference);
            let _readback = timings.start(TimingStage::LayoutReadback);
            ModelOutputs::try_from(&outputs)
        }
    }

    /// Bounded native sessions with permits held until actual blocking work completes.
    pub(crate) struct LayoutSessionPool {
        sessions: Vec<Arc<SessionWorker<LayoutSession>>>,
        available: Mutex<Vec<usize>>,
        semaphore: Arc<Semaphore>,
    }

    impl LayoutSessionPool {
        /// Creates the configured native sessions outside the async executor.
        pub(crate) async fn load(
            artifacts: ModelArtifacts,
            config: Arc<ValidatedConfig>,
        ) -> Result<Arc<Self>, LayoutError> {
            let size = config.layout().session_pool_size;
            let backend =
                crate::wasm_compat::OnnxBackend::from(config.as_ref());
            let mut sessions = Vec::with_capacity(size);
            // Each pool slot owns a thread so cuBLAS/cuDNN thread-local resources cannot multiply across Tokio workers.
            for _ in 0..size {
                let model = Arc::clone(&artifacts.model);
                let session = SessionWorker::new(move || {
                    LayoutSession::load(&model, backend)
                })
                .await?;
                sessions.push(Arc::new(session));
            }
            Ok(Arc::new(Self {
                sessions,
                available: Mutex::new((0..size).rev().collect()),
                semaphore: Arc::new(Semaphore::new(size)),
            }))
        }

        /// Leases one session without holding a synchronous lock while waiting.
        pub(crate) async fn acquire(
            self: Arc<Self>,
        ) -> Result<LayoutSessionLease, LayoutError> {
            let permit = Arc::clone(&self.semaphore)
                .acquire_owned()
                .await
                .map_err(|error| LayoutError::SessionPool {
                    message: error.to_string(),
                })?;
            let index = self
                .available
                .lock()
                .map_err(|error| LayoutError::SessionPool {
                    message: error.to_string(),
                })?
                .pop()
                .ok_or(LayoutError::SessionPool {
                    message: "permit without an available session".into(),
                })?;
            Ok(LayoutSessionLease {
                pool: self,
                index,
                _permit: permit,
            })
        }

        /// Holds the lease inside the blocking closure even if its caller is cancelled.
        pub(crate) async fn run(
            self: Arc<Self>,
            inputs: ModelInputs,
            timings: Timings,
        ) -> Result<ModelOutputs, LayoutError> {
            let queued = timings.start(TimingStage::LayoutQueue);
            let lease = self.acquire().await?;
            let worker =
                Arc::clone(lease.pool.sessions.get(lease.index).ok_or_else(
                    || LayoutError::SessionPool {
                        message: "invalid session slot".into(),
                    },
                )?);
            worker
                .run(move |session| {
                    // Keep the pool permit on the owning session thread until actual inference/readback finishes.
                    let _lease = lease;
                    drop(queued);
                    session.run(&inputs, &timings)
                })
                .await
                .map_err(|source| LayoutError::TaskJoin { source })?
        }
    }

    /// Returns its native pool slot only when the executing closure releases ownership.
    pub(crate) struct LayoutSessionLease {
        pool: Arc<LayoutSessionPool>,
        index: usize,
        _permit: OwnedSemaphorePermit,
    }

    impl Drop for LayoutSessionLease {
        /// Restores availability before releasing the semaphore permit.
        fn drop(&mut self) {
            self.pool
                .available
                .lock()
                .unwrap_or_else(|poisoned| {
                    tracing::warn!(
                        "recovering poisoned layout session availability queue"
                    );
                    poisoned.into_inner()
                })
                .push(self.index);
        }
    }

    #[cfg(test)]
    mod tests {
        use std::path::{Path, PathBuf};
        use std::sync::Arc;
        use std::time::Duration;

        use super::LayoutSessionPool;

        /// Resolves a repository path from the layout crate directory.
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
            use ort::session::{OutputSelector, RunOptions, Session};
            use std::time::Instant;

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
                let mut builder = Session::builder()?
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
                        session
                            .run(input, &crate::timing::Timings::default())?;
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
                            .run(input, &crate::timing::Timings::default())?;
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

        /// Verifies a dropped lease returns its unique session slot and semaphore permit.
        #[tokio::test]
        #[ignore = "requires fixed PP-DocLayoutV3 model"]
        async fn lease_drop_returns_session_to_pool() {
            let config = docparse_config::ValidatedConfig::try_from(
                docparse_config::RawConfig::default(),
            )
            .expect("valid defaults");
            let artifacts = crate::ModelArtifacts::from_paths(
                &repository_path("models/pp-doclayout-v3/inference.onnx"),
                &repository_path("models/pp-doclayout-v3/inference.yml"),
                &repository_path("models/pp-doclayout-v3/model-manifest.json"),
            )
            .expect("model artifacts");
            let pool = LayoutSessionPool::load(artifacts, Arc::new(config))
                .await
                .expect("the real one-session pool must initialize");
            let first = Arc::clone(&pool)
                .acquire()
                .await
                .expect("the first lease must be available");

            let blocked = tokio::time::timeout(
                Duration::from_millis(20),
                Arc::clone(&pool).acquire(),
            )
            .await;
            let blocked_failure = match blocked {
                Err(_elapsed) => None,
                Ok(_lease) => {
                    Some("a second lease unexpectedly acquired a session")
                }
            };
            assert_eq!(blocked_failure, None);

            drop(first);
            let returned = tokio::time::timeout(
                Duration::from_secs(1),
                Arc::clone(&pool).acquire(),
            )
            .await;
            let returned_failure = match returned {
                Ok(Ok(_lease)) => None,
                Ok(Err(error)) => {
                    Some(format!("returned lease failed: {error}"))
                }
                Err(error) => {
                    Some(format!("returned lease timed out: {error}"))
                }
            };
            assert_eq!(returned_failure, None);
        }
    }
}

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
mod platform {
    use super::*;
    use tokio::sync::{mpsc, oneshot};

    /// Owned inference request whose buffers survive cancellation of the waiting caller.
    #[derive(typed_builder::TypedBuilder)]
    struct InferenceRequest {
        inputs: ModelInputs,
        response: oneshot::Sender<Result<ModelOutputs, LayoutError>>,
        timings: Timings,
        queued: crate::timing::StageTimer,
    }

    /// The sender for one browser-local actor that owns its ORT session across awaits.
    pub(crate) struct LayoutSessionPool {
        sender: mpsc::Sender<InferenceRequest>,
    }

    impl LayoutSessionPool {
        /// Creates a browser session after the embedding host initializes ort-web once.
        pub(crate) async fn load(
            artifacts: ModelArtifacts,
            config: Arc<ValidatedConfig>,
        ) -> Result<Arc<Self>, LayoutError> {
            let mut builder = ort::session::builder::SessionBuilder::try_from(
                crate::wasm_compat::OnnxBackend::from(config.as_ref()),
            )?;
            let mut session =
                builder.commit_from_memory(&artifacts.model).await?;
            ModelSchema::from_session(&session)?.validate_pp_doclayout_v3()?;
            let options = RunOptions::new()?.with_outputs(
                OutputSelector::no_default()
                    .with("fetch_name_0")
                    .with("fetch_name_1"),
            );
            let (sender, mut receiver) = mpsc::channel::<InferenceRequest>(1);
            wasm_bindgen_futures::spawn_local(async move {
                while let Some(request) = receiver.recv().await {
                    // The actor keeps inputs and the session alive until the JS Promise completes.
                    let _inference =
                        crate::wasm_compat::OnnxBackend::inference_guard()
                            .await;
                    drop(request.queued);
                    let result = async {
                        let inference = request.timings.start(TimingStage::LayoutInference);
                        let mut outputs = session.run_async(ort::inputs! {
                            "im_shape" => TensorRef::from_array_view(&request.inputs.image_size)?,
                            "image" => TensorRef::from_array_view(&request.inputs.image)?,
                            "scale_factor" => TensorRef::from_array_view(&request.inputs.scale_factor)?,
                        }, &options).await?;
                        drop(inference);
                        // The Promise may finish before GPU outputs are CPU-readable.
                        let _readback = request.timings.start(TimingStage::LayoutReadback);
                        ort_web::sync_outputs(&mut outputs).await.map_err(|error| LayoutError::Engine { message: format!("Web tensor synchronization failed: {error}") })?;
                        ModelOutputs::try_from(&outputs)
                    }.await;
                    if let Err(error) = &result {
                        tracing::error!(
                            "browser model inference failed: {}",
                            error
                        );
                    }
                    let _ = request.response.send(result);
                }
                tracing::debug!("closed browser layout session");
            });
            Ok(Arc::new(Self { sender }))
        }

        /// Queues one owned input without borrowing session state across an await.
        pub(crate) async fn run(
            self: Arc<Self>,
            inputs: ModelInputs,
            timings: Timings,
        ) -> Result<ModelOutputs, LayoutError> {
            let queued = timings.start(TimingStage::LayoutQueue);
            let (response, receiver) = oneshot::channel();
            self.sender
                .send(
                    InferenceRequest::builder()
                        .inputs(inputs)
                        .response(response)
                        .timings(timings)
                        .queued(queued)
                        .build(),
                )
                .await
                .map_err(|_error| LayoutError::SessionPool {
                    message: "browser session stopped".into(),
                })?;
            receiver.await.map_err(|_error| LayoutError::SessionPool {
                message: "browser session response lost".into(),
            })?
        }
    }
}

pub(crate) use platform::LayoutSessionPool;
