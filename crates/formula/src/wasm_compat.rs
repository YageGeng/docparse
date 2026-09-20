//! Platform ownership keeps inference buffers alive until native calls or browser promises finish.
use crate::{
    FormulaArtifacts, FormulaError, PpFormulaNetEngine,
    artifacts::ModelKind,
    model::FormulaDecoder,
    preprocess::FormulaInput,
    queue::{BatchTimings, FormulaQueue, FormulaRequest},
};
use docparse_common::timing::{TimingStage, Timings};
use docparse_layout::{PageImage, wasm_compat::OnnxBackend};
use ort::{session::builder::SessionBuilder, value::Tensor};
use std::sync::Arc;

#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
mod platform {
    use super::*;
    use docparse_layout::wasm_compat::SessionWorker;

    /// One thread owns model construction, all batches and destruction.
    pub(crate) struct SessionRunner {
        // Drop the sender before joining consumers so idle owners can exit.
        queue: FormulaQueue,
        workers: crate::queue::FormulaWorkers,
        /// Actual executor, including the native Apple compatibility path.
        pub(crate) provider: docparse_layout::ExecutionProvider,
    }

    impl SessionRunner {
        /// Returns the same pressure state for native and browser formula queues.
        pub(crate) fn pressure(
            &self,
        ) -> Arc<docparse_common::queue::QueuePressure> {
            self.queue.pressure()
        }

        /// Loads consumers over an explicitly sized queue, using the CPU compatibility executor on Apple.
        pub(crate) async fn load(
            artifacts: FormulaArtifacts,
            backend: OnnxBackend,
            kind: ModelKind,
            batch_size: usize,
            session_size: usize,
            queue_size: usize,
            shared: Option<FormulaQueue>,
        ) -> Result<Arc<Self>, FormulaError> {
            let coreml_incompatible = cfg!(target_os = "macos")
                && matches!(
                    backend.execution_provider(),
                    docparse_layout::ExecutionProvider::CoreMl
                        | docparse_layout::ExecutionProvider::Metal
                );
            if coreml_incompatible {
                tracing::warn!(
                    "{} uses the CPU compatibility executor on Apple; accelerated repeated inference is not enabled",
                    kind.as_str()
                );
            }
            let metrics = docparse_common::telemetry::ModelMetrics::new(
                "formula_pp",
                session_size,
                batch_size,
            );
            let queue = shared.unwrap_or_else(|| {
                FormulaQueue::new("formula_pp", queue_size).0
            });
            let receiver = queue.receiver();
            let mut runner = Self {
                queue,
                workers: crate::queue::FormulaWorkers::default(),
                provider: if coreml_incompatible {
                    docparse_layout::ExecutionProvider::Cpu
                } else {
                    backend.execution_provider()
                },
            };
            let provider = runner.provider;
            for _ in 0..session_size {
                let model = Arc::clone(&artifacts.model);
                let tokenizer = Arc::clone(&artifacts.tokenizer);
                let session = SessionWorker::new(move || {
                    // A stable CPU batch is required on Apple; register other compiled accelerators normally.
                    let mut builder = if coreml_incompatible {
                        // Preserve the configured optimization level while selecting the compatibility executor.
                        backend.cpu_builder()?
                    } else {
                        SessionBuilder::try_from(backend)?
                    };
                    let session = builder.commit_from_memory(&model)?;
                    Ok::<_, FormulaError>((
                        session,
                        FormulaDecoder::new(&tokenizer)?,
                    ))
                })
                .await?;
                let receiver = receiver.clone();
                let metrics = Arc::clone(&metrics);
                runner.workers.spawn(Box::pin(async move {
                let _alive = metrics.alive(1);
                while let Some(batch) = receiver.recv().await {
                    let mut requests = batch.take_ready(batch_size);
                    if requests.is_empty() {
                        continue;
                    }
                    for request in &mut requests { request.engine = format!("{}-onnx-{}", kind.as_str(), provider); }
                    let _batch = metrics.batch();
                    let images = requests
                        .iter()
                        .map(|request| Arc::clone(&request.image))
                        .collect::<Vec<_>>();
                    let timings: BatchTimings = requests.iter().map(|request| &request.context).collect();
                    tracing::debug!(
                        "PP formula session running {} ready crops",
                        images.len()
                    );
                    let options = match ort::session::RunOptions::new() {
                        Ok(options) => Arc::new(options),
                        Err(error) => {
                            FormulaRequest::complete_batch(
                                requests,
                                Err(error.into()),
                            );
                            continue;
                        }
                    };
                    let mut cancel = CancelRun(Some(Arc::clone(&options)));
                    let work = session.run(move |(session, decoder)| {
                        let batch = images.len();
                        let preprocessing =
                            timings.start(TimingStage::FormulaPreprocess);
                        let input = FormulaInput::try_from((images, kind))?;
                        drop(preprocessing);
                        let inference =
                            timings.start(TimingStage::FormulaInference);
                        let physical = docparse_common::telemetry::Inference::new("formula_pp", "model", batch);
                        let outputs = session.run_with_options(
                            ort::inputs!["x" => Tensor::from_array(input.0)?],
                            &options,
                        );
                        physical.finish(outputs.is_ok());
                        let outputs = outputs?;
                        drop(inference);
                        let _decoding =
                            timings.start(TimingStage::FormulaDecode);
                        decoder.decode(&outputs, batch)
                    });
                    tokio::pin!(work);
                    // A merged batch may terminate only after every original caller has canceled.
                    let result = tokio::select! {
                        result = &mut work => result,
                        _ = futures_util::future::join_all(requests.iter_mut().map(FormulaRequest::closed)) => {
                            drop(CancelRun(cancel.0.take()));
                            work.await
                        }
                    };
                    cancel.0.take();
                    FormulaRequest::complete_batch(
                        requests,
                        result
                            .map_err(FormulaError::from)
                            .and_then(|result| result),
                    );
                }
                tracing::debug!("closed native PP formula queue");
            }))?;
            }
            Ok(Arc::new(runner))
        }

        /// Enqueues independent crops into the shared session queue.
        pub(crate) async fn run(
            self: Arc<Self>,
            images: Vec<Arc<PageImage>>,
            timings: Timings,
        ) -> Result<Vec<String>, FormulaError> {
            self.queue.run(images, timings).await
        }
    }

    /// Cancellation signals ORT without releasing input buffers while a native call still owns them.
    struct CancelRun(Option<Arc<ort::session::RunOptions>>);
    impl Drop for CancelRun {
        /// Stops an in-flight autoregressive loop at the next ORT cancellation boundary.
        fn drop(&mut self) {
            if let Some(options) = &self.0
                && let Err(error) = options.terminate()
            {
                tracing::warn!(
                    "failed to terminate canceled formula inference: {}",
                    error
                );
            }
        }
    }

    impl PpFormulaNetEngine {
        /// Loads the configured files away from the async executor, then verifies their identity.
        pub async fn from_config(
            config: Arc<docparse_config::ValidatedConfig>,
        ) -> Result<Self, FormulaError> {
            Self::from_config_on_queue(config, None).await
        }

        /// Loads this group's files while registering consumers on the supplied queue.
        pub async fn from_config_on_queue(
            config: Arc<docparse_config::ValidatedConfig>,
            queue: Option<FormulaQueue>,
        ) -> Result<Self, FormulaError> {
            let docparse_config::FormulaEngineConfig::Pp(paths) =
                config.formula().single_engine()?.clone()
            else {
                tracing::error!("PP loader requires formula.engine.type = pp");
                return Err(FormulaError::Invalid(
                    "PP loader requires formula.engine.type = pp".into(),
                ));
            };
            let artifacts = docparse_common::run_cpu(move || {
                let read = |path: &std::path::Path| {
                    std::fs::read(path).map(Arc::from).map_err(|error| {
                        FormulaError::Artifacts(format!(
                            "{}: {error}",
                            path.display()
                        ))
                    })
                };
                Ok::<_, FormulaError>(FormulaArtifacts {
                    model: read(&paths.model_path)?,
                    tokenizer: read(&paths.tokenizer_path)?,
                    manifest: read(&paths.model_manifest_path)?,
                })
            })
            .await??;
            Self::from_artifacts_on_queue(config, artifacts, queue).await
        }
    }
}

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
mod platform {
    use super::*;
    pub(crate) struct SessionRunner {
        // Drop the sender before joining consumers so idle owners can exit.
        queue: FormulaQueue,
        workers: crate::queue::FormulaWorkers,
        /// Actual executor, including the native Apple compatibility path.
        pub(crate) provider: docparse_layout::ExecutionProvider,
    }

    impl SessionRunner {
        /// Returns the same pressure state for native and browser formula queues.
        pub(crate) fn pressure(
            &self,
        ) -> Arc<docparse_common::queue::QueuePressure> {
            self.queue.pressure()
        }

        /// Builds one browser actor and retains the global inference/readback exclusion boundary.
        pub(crate) async fn load(
            artifacts: FormulaArtifacts,
            backend: OnnxBackend,
            kind: ModelKind,
            batch_size: usize,
            session_size: usize,
            queue_size: usize,
            shared: Option<FormulaQueue>,
        ) -> Result<Arc<Self>, FormulaError> {
            let metrics = docparse_common::telemetry::ModelMetrics::new(
                "formula_pp",
                session_size,
                batch_size,
            );
            let queue = shared.unwrap_or_else(|| {
                FormulaQueue::new("formula_pp", queue_size).0
            });
            let receiver = queue.receiver();
            let mut runner = Self {
                queue,
                workers: crate::queue::FormulaWorkers::default(),
                provider: backend.execution_provider(),
            };
            let provider = runner.provider;
            for _ in 0..session_size {
                // Browser formula sessions inherit the global graph and memory settings.
                let mut session = SessionBuilder::try_from(backend)?
                    .commit_from_memory(&artifacts.model)
                    .await?;
                let decoder = FormulaDecoder::new(&artifacts.tokenizer)?;
                let options = ort::session::RunOptions::new()?;
                let receiver = receiver.clone();
                let metrics = Arc::clone(&metrics);
                runner.workers.spawn(Box::pin(async move {
                let _alive = metrics.alive(1);
                while let Some(batch) = receiver.recv().await {
                    let mut requests = batch.take_ready(batch_size);
                    if requests.is_empty() {
                        continue;
                    }
                    for request in &mut requests { request.engine = format!("{}-onnx-{}", kind.as_str(), provider); }
                    let _batch = metrics.batch();
                    let images = requests
                        .iter()
                        .map(|request| Arc::clone(&request.image))
                        .collect::<Vec<_>>();
                    let timings: BatchTimings = requests.iter().map(|request| &request.context).collect();
                    // Release the shared formula receiver before waiting for ORT so HTTP consumers can keep draining it.
                    let queued = timings.start(TimingStage::FormulaQueue);
                    let _guard = OnnxBackend::inference_guard().await;
                    drop(queued);
                    tracing::debug!(
                        "browser PP formula session running {} ready crops",
                        images.len()
                    );
                    let result = async {
                        let batch = images.len();
                        let preprocessing = timings.start(TimingStage::FormulaPreprocess);
                        let input = FormulaInput::try_from((images, kind))?;
                        drop(preprocessing);
                        let inference = timings.start(TimingStage::FormulaInference);
                        let mut outputs = session.run_async(ort::inputs!["x" => Tensor::from_array(input.0)?], &options).await?;
                        ort_web::sync_outputs(&mut outputs).await.map_err(|error| FormulaError::Invalid(format!("formula readback failed: {error}")))?;
                        drop(inference);
                        let _decoding = timings.start(TimingStage::FormulaDecode);
                        decoder.decode(&outputs, batch)
                    }.await;
                    FormulaRequest::complete_batch(requests, result);
                }
                tracing::debug!("closed browser PP formula queue");
            }))?;
            }
            Ok(Arc::new(runner))
        }

        /// Enqueues owned pixels and preserves each caller's cancellation and result order.
        pub(crate) async fn run(
            self: Arc<Self>,
            images: Vec<Arc<PageImage>>,
            timings: Timings,
        ) -> Result<Vec<String>, FormulaError> {
            self.queue.run(images, timings).await
        }
    }

    impl PpFormulaNetEngine {
        /// Browser sessions require explicit model and tokenizer bytes from the host.
        pub async fn from_config(
            _config: Arc<docparse_config::ValidatedConfig>,
        ) -> Result<Self, FormulaError> {
            Err(FormulaError::ArtifactsRequired)
        }
        /// Browser consumers require explicit artifacts for every local engine.
        pub async fn from_config_on_queue(
            config: Arc<docparse_config::ValidatedConfig>,
            _queue: Option<FormulaQueue>,
        ) -> Result<Self, FormulaError> {
            Self::from_config(config).await
        }
    }
}

pub(crate) use platform::SessionRunner;
