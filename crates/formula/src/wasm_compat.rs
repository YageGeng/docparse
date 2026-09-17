//! Platform ownership keeps inference buffers alive until native calls or browser promises finish.
use crate::{
    FormulaArtifacts, FormulaError, PpFormulaNetEngine, artifacts::ModelKind,
    model::FormulaDecoder, preprocess::FormulaInput,
};
use docparse_layout::{
    PageImage,
    timing::{TimingStage, Timings},
    wasm_compat::OnnxBackend,
};
use ort::{session::builder::SessionBuilder, value::Tensor};
use std::sync::Arc;

#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
mod platform {
    use super::*;
    use docparse_layout::wasm_compat::SessionWorker;

    /// One thread owns model construction, all batches and destruction.
    pub(crate) struct SessionRunner {
        session: SessionWorker<(ort::session::Session, FormulaDecoder)>,
        kind: ModelKind,
    }

    impl SessionRunner {
        /// Loads the selected provider, using the validated CPU compatibility executor on Apple.
        pub(crate) async fn load(
            artifacts: FormulaArtifacts,
            backend: OnnxBackend,
            kind: ModelKind,
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
            let session = SessionWorker::new(move || {
                // A stable CPU batch is required on Apple; register other compiled accelerators normally.
                let mut builder = if coreml_incompatible {
                    ort::session::Session::builder()?
                } else {
                    SessionBuilder::try_from(backend)?
                }
                .with_intra_threads(1)
                .map_err(ort::Error::from)?;
                let session = builder.commit_from_memory(&artifacts.model)?;
                Ok::<_, FormulaError>((
                    session,
                    FormulaDecoder::new(&artifacts.tokenizer)?,
                ))
            })
            .await?;
            Ok(Arc::new(Self { session, kind }))
        }

        /// Keeps pixels and run options alive until execution finishes, terminating canceled autoregressive work.
        pub(crate) async fn run(
            self: Arc<Self>,
            images: Vec<Arc<PageImage>>,
            timings: Timings,
        ) -> Result<Vec<String>, FormulaError> {
            let queued = timings.start(TimingStage::FormulaQueue);
            let options = Arc::new(ort::session::RunOptions::new()?);
            let mut cancel = CancelRun(Some(Arc::clone(&options)));
            let kind = self.kind;
            let result = self
                .session
                .run(move |(session, decoder)| {
                    drop(queued);
                    let batch = images.len();
                    let preprocessing =
                        timings.start(TimingStage::FormulaPreprocess);
                    let input = FormulaInput::try_from((images, kind))?;
                    drop(preprocessing);
                    let inference =
                        timings.start(TimingStage::FormulaInference);
                    let outputs = session.run_with_options(
                        ort::inputs!["x" => Tensor::from_array(input.0)?],
                        &options,
                    )?;
                    drop(inference);
                    let _decoding = timings.start(TimingStage::FormulaDecode);
                    decoder.decode(&outputs, batch)
                })
                .await?;
            cancel.0.take();
            result
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
            let docparse_config::FormulaEngineConfig::Pp(paths) =
                config.formula().engine.clone()
            else {
                tracing::error!("PP loader requires formula.engine.type = pp");
                return Err(FormulaError::Invalid(
                    "PP loader requires formula.engine.type = pp".into(),
                ));
            };
            let artifacts = docparse_layout::wasm_compat::run_cpu(move || {
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
            Self::from_artifacts(config, artifacts).await
        }
    }
}

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
mod platform {
    use super::*;
    use tokio::sync::{mpsc, oneshot};

    /// Channel ownership outlives canceled callers until the JS operation has completed.
    type Request = (
        Vec<Arc<PageImage>>,
        Timings,
        docparse_layout::timing::StageTimer,
        oneshot::Sender<Result<Vec<String>, FormulaError>>,
    );
    pub(crate) struct SessionRunner {
        sender: mpsc::Sender<Request>,
    }

    impl SessionRunner {
        /// Builds one browser actor and retains the global inference/readback exclusion boundary.
        pub(crate) async fn load(
            artifacts: FormulaArtifacts,
            backend: OnnxBackend,
            kind: ModelKind,
        ) -> Result<Arc<Self>, FormulaError> {
            let mut session = SessionBuilder::try_from(backend)?
                .commit_from_memory(&artifacts.model)
                .await?;
            let decoder = FormulaDecoder::new(&artifacts.tokenizer)?;
            let options = ort::session::RunOptions::new()?;
            let (sender, mut receiver) = mpsc::channel::<Request>(1);
            wasm_bindgen_futures::spawn_local(async move {
                while let Some((images, timings, queued, response)) =
                    receiver.recv().await
                {
                    if response.is_closed() {
                        continue;
                    }
                    let _guard = OnnxBackend::inference_guard().await;
                    if response.is_closed() {
                        continue;
                    }
                    drop(queued);
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
                    let _ = response.send(result);
                }
                tracing::debug!("closed browser formula session");
            });
            Ok(Arc::new(Self { sender }))
        }

        /// Queues owned crop pixels and preserves the final partial batch.
        pub(crate) async fn run(
            self: Arc<Self>,
            images: Vec<Arc<PageImage>>,
            timings: Timings,
        ) -> Result<Vec<String>, FormulaError> {
            let queued = timings.start(TimingStage::FormulaQueue);
            let (response, receiver) = oneshot::channel();
            self.sender
                .send((images, timings, queued, response))
                .await
                .map_err(|error| {
                    FormulaError::Invalid(format!(
                        "formula actor stopped: {error}"
                    ))
                })?;
            receiver.await.map_err(|error| {
                FormulaError::Invalid(format!("formula response lost: {error}"))
            })?
        }
    }

    impl PpFormulaNetEngine {
        /// Browser sessions require explicit model and tokenizer bytes from the host.
        pub async fn from_config(
            _config: Arc<docparse_config::ValidatedConfig>,
        ) -> Result<Self, FormulaError> {
            Err(FormulaError::ArtifactsRequired)
        }
    }
}

pub(crate) use platform::SessionRunner;
