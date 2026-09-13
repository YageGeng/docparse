//! One session per model, with owned inputs retained through actual native/JS execution.
use crate::{
    SlanetPlusEngine, TsrError, model::ModelOutputs, preprocess::SlanetInput,
};
use docparse_layout::wasm_compat::OnnxBackend;
use docparse_layout::{
    ModelArtifacts,
    timing::{TimingStage, Timings},
};
use ort::{session::builder::SessionBuilder, value::TensorRef};
use std::sync::Arc;

#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
mod platform {
    use super::*;
    use docparse_layout::wasm_compat::SessionWorker;
    use ort::session::Session;

    /// Serializes only the TSR session, leaving layout inference independent.
    pub(crate) struct SessionRunner {
        session: SessionWorker<Session>,
    }
    impl SessionRunner {
        /// Initializes the configured graph on its dedicated inference thread.
        pub(crate) async fn load(
            artifacts: ModelArtifacts,
            backend: OnnxBackend,
        ) -> Result<Arc<Self>, TsrError> {
            let session = SessionWorker::new(move || {
                // Preprocessing always produces [1, 3, 488, 488]. Specialize the pinned
                // model's free dimensions so accelerator shape inference sees that contract.
                Ok::<_, TsrError>(
                    SessionBuilder::try_from(backend)?
                        .with_dimension_override("DynamicDimension.0", 1)
                        .map_err(ort::Error::from)?
                        .with_dimension_override("DynamicDimension.1", 488)
                        .map_err(ort::Error::from)?
                        .with_dimension_override("DynamicDimension.2", 488)
                        .map_err(ort::Error::from)?
                        .with_intra_threads(1)
                        .map_err(ort::Error::from)?
                        .commit_from_memory(&artifacts.model)?,
                )
            })
            .await?;
            // ponytail: one session serializes TSR crops; add a pool only if measured throughput needs it.
            Ok(Arc::new(Self { session }))
        }

        /// Runs on the same thread for every crop while retaining tensors through actual completion.
        pub(crate) async fn run(
            self: Arc<Self>,
            input: SlanetInput,
            timings: Timings,
        ) -> Result<ModelOutputs, TsrError> {
            let queued = timings.start(TimingStage::TsrQueue);
            self.session.run(move |session| {
                drop(queued);
                let _timer = timings.start(TimingStage::TsrInference);
                let outputs = session.run(ort::inputs! { "x" => TensorRef::from_array_view(&input.0)? })?;
                ModelOutputs::try_from(&outputs)
            }).await?
        }
    }

    impl SlanetPlusEngine {
        /// Reads fixed native model artifacts from validated configuration paths.
        pub async fn from_config(
            config: Arc<docparse_config::ValidatedConfig>,
        ) -> Result<Self, TsrError> {
            let paths = Arc::clone(&config);
            let artifacts = docparse_layout::wasm_compat::run_cpu(move || {
                ModelArtifacts::from_paths(
                    &paths.tsr().model_path,
                    &paths.tsr().model_config_path,
                    &paths.tsr().model_manifest_path,
                )
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
        input: SlanetInput,
        response: oneshot::Sender<Result<ModelOutputs, TsrError>>,
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
                        let _timer = request.timings.start(TimingStage::TsrInference);
                        let mut outputs = session.run_async(ort::inputs! { "x" => TensorRef::from_array_view(&request.input.0)? }, &options).await?;
                        ort_web::sync_outputs(&mut outputs).await.map_err(|error| TsrError::Inference { message: format!("TSR output synchronization failed: {error}") })?;
                        ModelOutputs::try_from(&outputs)
                    }.await;
                    let _ = request.response.send(result);
                }
                tracing::debug!("closed browser TSR session");
            });
            Ok(Arc::new(Self { sender }))
        }

        /// Sends owned buffers so a timed-out caller cannot release input memory during a JS Promise.
        pub(crate) async fn run(
            self: Arc<Self>,
            input: SlanetInput,
            timings: Timings,
        ) -> Result<ModelOutputs, TsrError> {
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
