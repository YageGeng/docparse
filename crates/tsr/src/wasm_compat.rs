//! One session per model, with owned inputs retained through actual native/JS execution.
use crate::{
    SlanetPlusEngine, TsrError, artifacts::ModelKind, model::ModelResult,
    preprocess::ModelInput,
};
use docparse_layout::wasm_compat::OnnxBackend;
use docparse_layout::{
    ModelArtifacts,
    timing::{TimingStage, Timings},
};
use ort::session::builder::SessionBuilder;
use std::sync::Arc;

#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
mod platform {
    use super::*;
    use crate::TsrArtifacts;
    use docparse_layout::wasm_compat::SessionWorker;
    use ort::session::Session;

    /// Serializes only the TSR session, leaving layout inference independent.
    pub(crate) struct SessionRunner {
        session: SessionWorker<Session>,
        kind: ModelKind,
    }
    impl SessionRunner {
        /// Initializes the configured graph on its dedicated inference thread.
        pub(crate) async fn load(
            artifacts: ModelArtifacts,
            backend: OnnxBackend,
            kind: ModelKind,
        ) -> Result<Arc<Self>, TsrError> {
            let session = SessionWorker::new(move || {
                // Only input dimensions are specialized; similarly named output symbols have other meanings.
                let mut builder = SessionBuilder::try_from(backend)?
                    .with_intra_threads(1)
                    .map_err(ort::Error::from)?;
                let batch_dimension = if matches!(kind, ModelKind::Cells(_)) {
                    "DynamicDimension.2"
                } else {
                    "DynamicDimension.0"
                };
                builder = builder
                    .with_dimension_override(batch_dimension, 1)
                    .map_err(ort::Error::from)?;
                if matches!(
                    kind,
                    ModelKind::Structure(docparse_config::TsrModel::SlanetPlus)
                ) {
                    builder = builder
                        .with_dimension_override("DynamicDimension.1", 488)
                        .map_err(ort::Error::from)?
                        .with_dimension_override("DynamicDimension.2", 488)
                        .map_err(ort::Error::from)?;
                }
                Ok::<_, TsrError>(builder.commit_from_memory(&artifacts.model)?)
            })
            .await?;
            // ponytail: one session serializes TSR crops; add a pool only if measured throughput needs it.
            Ok(Arc::new(Self { session, kind }))
        }

        /// Runs on the same thread for every crop while retaining tensors through actual completion.
        pub(crate) async fn run(
            self: Arc<Self>,
            input: ModelInput,
            timings: Timings,
        ) -> Result<ModelResult, TsrError> {
            let queued = timings.start(TimingStage::TsrQueue);
            let kind = self.kind;
            self.session
                .run(move |session| {
                    drop(queued);
                    let _timer = timings.start(kind.timing());
                    let outputs = session.run(input.values()?)?;
                    ModelResult::try_from((kind, &outputs))
                })
                .await?
        }
    }

    impl SlanetPlusEngine {
        /// Reads fixed native model artifacts from validated configuration paths.
        pub async fn from_config(
            config: Arc<docparse_config::ValidatedConfig>,
        ) -> Result<Self, TsrError> {
            let paths = Arc::clone(&config);
            let artifacts = docparse_layout::wasm_compat::run_cpu(move || {
                let structure = ModelArtifacts::from_paths(
                    &paths.tsr().model_path,
                    &paths.tsr().model_config_path,
                    &paths.tsr().model_manifest_path,
                )?;
                let cell_detection = paths
                    .tsr()
                    .cell_detection
                    .as_ref()
                    .filter(|cells| cells.enabled)
                    .map(|cells| {
                        ModelArtifacts::from_paths(
                            &cells.files.model_path,
                            &cells.files.model_config_path,
                            &cells.files.model_manifest_path,
                        )
                    })
                    .transpose()?;
                Ok::<_, TsrError>(TsrArtifacts {
                    structure,
                    cell_detection,
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
    use ort::session::RunOptions;
    use tokio::sync::{mpsc, oneshot};

    /// A queued crop owns its tensor until the actor finishes or skips canceled work.
    #[derive(typed_builder::TypedBuilder)]
    struct Request {
        input: ModelInput,
        response: oneshot::Sender<Result<ModelResult, TsrError>>,
        timings: Timings,
        queued: docparse_layout::timing::StageTimer,
    }
    pub(crate) struct SessionRunner {
        sender: mpsc::Sender<Request>,
    }
    impl SessionRunner {
        /// Creates the selected browser session after the host initializes ort-web.
        pub(crate) async fn load(
            artifacts: ModelArtifacts,
            backend: OnnxBackend,
            kind: ModelKind,
        ) -> Result<Arc<Self>, TsrError> {
            let mut session = SessionBuilder::try_from(backend)?
                .commit_from_memory(&artifacts.model)
                .await?;
            let options = RunOptions::new()?;
            let (sender, mut receiver) = mpsc::channel::<Request>(1);
            wasm_bindgen_futures::spawn_local(async move {
                while let Some(request) = receiver.recv().await {
                    if request.response.is_closed() {
                        continue;
                    }
                    let _inference = OnnxBackend::inference_guard().await;
                    // A deadline may expire while another model owns the browser runtime.
                    if request.response.is_closed() {
                        continue;
                    }
                    drop(request.queued);
                    let result = async {
                        let _timer = request.timings.start(kind.timing());
                        let mut outputs = session
                            .run_async(request.input.values()?, &options)
                            .await?;
                        ort_web::sync_outputs(&mut outputs).await.map_err(
                            |error| TsrError::Inference {
                                message: format!(
                                    "TSR output synchronization failed: {error}"
                                ),
                            },
                        )?;
                        ModelResult::try_from((kind, &outputs))
                    }
                    .await;
                    let _ = request.response.send(result);
                }
                tracing::debug!("closed browser TSR session");
            });
            Ok(Arc::new(Self { sender }))
        }

        /// Sends owned buffers so a timed-out caller cannot release input memory during a JS Promise.
        pub(crate) async fn run(
            self: Arc<Self>,
            input: ModelInput,
            timings: Timings,
        ) -> Result<ModelResult, TsrError> {
            let queued = timings.start(TimingStage::TsrQueue);
            let (response, receiver) = oneshot::channel();
            self.sender
                .send(
                    Request::builder()
                        .input(input)
                        .response(response)
                        .timings(timings)
                        .queued(queued)
                        .build(),
                )
                .await
                .map_err(|_closed| TsrError::Inference {
                    message: "TSR worker stopped".to_owned(),
                })?;
            receiver.await.map_err(|_closed| TsrError::Inference {
                message: "TSR response lost".to_owned(),
            })?
        }
    }

    impl SlanetPlusEngine {
        /// Browser hosts must provide bytes rather than relying on filesystem configuration.
        pub async fn from_config(
            _config: Arc<docparse_config::ValidatedConfig>,
        ) -> Result<Self, TsrError> {
            tracing::error!(
                "browser TSR initialization requires explicit artifacts"
            );
            Err(TsrError::ModelArtifactsRequired)
        }
    }
}

pub(crate) use platform::SessionRunner;
