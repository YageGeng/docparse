//! OCR sessions own their input until inference and readback actually finish, including cancellation.
use crate::{
    OcrError,
    model::{ModelKind, ModelOutput},
    preprocess::ImageTensor,
};
use docparse_config::ExecutionProviderConfig;
use docparse_layout::{
    timing::{TimingStage, Timings},
    wasm_compat::OnnxBackend,
};
use ort::{session::builder::SessionBuilder, value::TensorRef};
use std::sync::Arc;

#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
mod platform {
    use super::*;
    use ort::session::Session;
    use tokio::sync::Mutex;

    /// A single native OCR stage, serialized independently of other model sessions.
    pub(crate) struct SessionRunner {
        session: Arc<Mutex<Session>>,
        kind: ModelKind,
    }

    impl SessionRunner {
        /// Loads immutable model bytes on the blocking executor using the shared strict backend selector.
        pub async fn load(
            bytes: Arc<[u8]>,
            provider: ExecutionProviderConfig,
            kind: ModelKind,
        ) -> Result<Arc<Self>, OcrError> {
            let session = docparse_layout::wasm_compat::run_cpu(move || {
                let session = SessionBuilder::try_from(OnnxBackend(provider))?
                    .with_intra_threads(1)
                    .map_err(ort::Error::from)?
                    .commit_from_memory(&bytes)?;
                kind.validate_session(&session)?;
                Ok::<_, OcrError>(session)
            })
            .await??;
            Ok(Arc::new(Self {
                session: Arc::new(Mutex::new(session)),
                kind,
            }))
        }

        /// Moves the owned guard into the blocking call so cancellation cannot free an in-flight tensor.
        pub async fn run(
            self: Arc<Self>,
            input: ImageTensor,
            timings: Timings,
        ) -> Result<ModelOutput, OcrError> {
            let queued = timings.start(TimingStage::OcrQueue);
            let mut session = Arc::clone(&self.session).lock_owned().await;
            docparse_layout::wasm_compat::run_cpu(move || {
                drop(queued);
                let _timer = timings.start(self.kind.timing());
                let outputs = session.run(
                    ort::inputs! {"x"=>TensorRef::from_array_view(&input.0)?},
                )?;
                self.kind.read(&outputs)
            })
            .await?
        }
    }

    impl crate::OcrArtifacts {
        /// Reads the three configured artifact directories without introducing filesystem access into shared inference.
        pub async fn from_config(
            config: &docparse_config::OcrConfig,
        ) -> Result<Self, OcrError> {
            let config = config.clone();
            docparse_layout::wasm_compat::run_cpu(move || {
                let load = |path: &std::path::Path| {
                    docparse_layout::ModelArtifacts::from_paths(
                        &path.join("inference.onnx"),
                        &path.join("inference.yml"),
                        &path.join("model-manifest.json"),
                    )
                };
                Ok::<_, OcrError>(Self {
                    detection: load(&config.detection_model_dir)?,
                    recognition: load(&config.recognition_model_dir)?,
                    orientation: if config.classify_orientation {
                        Some(load(&config.orientation_model_dir)?)
                    } else {
                        None
                    },
                })
            })
            .await?
        }
    }
}

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
mod platform {
    use super::*;
    use ort::session::RunOptions;
    use tokio::sync::{mpsc, oneshot};

    /// One bounded request keeps its tensor alive through the JS promise and GPU readback.
    #[derive(typed_builder::TypedBuilder)]
    struct Request {
        input: ImageTensor,
        response: oneshot::Sender<Result<ModelOutput, OcrError>>,
        timings: Timings,
        queued: docparse_layout::timing::StageTimer,
    }
    pub(crate) struct SessionRunner {
        sender: mpsc::Sender<Request>,
    }

    impl SessionRunner {
        /// Starts a Worker-local actor on the already initialized ORT Web backend.
        pub async fn load(
            bytes: Arc<[u8]>,
            provider: ExecutionProviderConfig,
            kind: ModelKind,
        ) -> Result<Arc<Self>, OcrError> {
            let mut session = SessionBuilder::try_from(OnnxBackend(provider))?
                .commit_from_memory(&bytes)
                .await?;
            kind.validate_session(&session)?;
            let options = RunOptions::new()?;
            let (sender, mut receiver) = mpsc::channel::<Request>(1);
            wasm_bindgen_futures::spawn_local(async move {
                while let Some(request) = receiver.recv().await {
                    if request.response.is_closed() {
                        continue;
                    }
                    let _inference = OnnxBackend::inference_guard().await;
                    if request.response.is_closed() {
                        continue;
                    }
                    drop(request.queued);
                    let result = async {
                        let _timer = request.timings.start(kind.timing());
                        let inputs = ort::inputs! {
                            "x" => TensorRef::from_array_view(&request.input.0)?
                        };
                        let mut outputs =
                            session.run_async(inputs, &options).await?;
                        // The shared guard and owned input outlive GPU readback, even after caller cancellation.
                        ort_web::sync_outputs(&mut outputs).await.map_err(
                            |error| {
                                OcrError::InvalidData(format!(
                                    "OCR output synchronization failed: {error}"
                                ))
                            },
                        )?;
                        kind.read(&outputs)
                    }
                    .await;
                    let _ = request.response.send(result);
                }
                tracing::debug!("closed browser OCR {:?} session", kind);
            });
            Ok(Arc::new(Self { sender }))
        }

        /// Enqueues owned memory; dropped callers cannot release the buffers used by JavaScript.
        pub async fn run(
            self: Arc<Self>,
            input: ImageTensor,
            timings: Timings,
        ) -> Result<ModelOutput, OcrError> {
            let queued = timings.start(TimingStage::OcrQueue);
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
                .map_err(|_closed| {
                    OcrError::InvalidData("OCR worker stopped".into())
                })?;
            receiver.await.map_err(|_closed| {
                OcrError::InvalidData("OCR response lost".into())
            })?
        }
    }

    impl crate::OcrArtifacts {
        /// Browser callers must provide owned model artifacts explicitly.
        pub async fn from_config(
            _config: &docparse_config::OcrConfig,
        ) -> Result<Self, OcrError> {
            Err(OcrError::ModelArtifactsRequired)
        }
    }
}

pub(crate) use platform::SessionRunner;
