//! OCR sessions own their input until inference and readback actually finish, including cancellation.
use crate::{
    OcrError,
    model::{ModelKind, ModelOutput},
    preprocess::ImageTensor,
};
use docparse_layout::{
    timing::{TimingStage, Timings},
    wasm_compat::OnnxBackend,
};
use ort::{session::builder::SessionBuilder, value::TensorRef};
use std::sync::Arc;

// Native stages can overlap across pages; browser tensor execution remains single-page bounded.
#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
pub(crate) const MAX_PAGE_CONCURRENCY: usize = 32;
#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
pub(crate) const MAX_PAGE_CONCURRENCY: usize = 1;

#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
mod platform {
    use super::*;
    use docparse_layout::wasm_compat::SessionWorker;
    use ort::session::Session;

    /// A single native OCR stage, serialized independently of other model sessions.
    pub(crate) struct SessionRunner {
        session: SessionWorker<Session>,
        kind: ModelKind,
    }

    impl SessionRunner {
        /// Loads immutable model bytes on the same dedicated thread that will run inference.
        pub async fn load(
            bytes: Arc<[u8]>,
            backend: OnnxBackend,
            kind: ModelKind,
        ) -> Result<Arc<Self>, OcrError> {
            let session = SessionWorker::new(move || {
                let session = SessionBuilder::try_from(backend)?
                    .with_intra_threads(1)
                    .map_err(ort::Error::from)?
                    .commit_from_memory(&bytes)?;
                kind.validate_session(&session)?;
                Ok::<_, OcrError>(session)
            })
            .await?;
            Ok(Arc::new(Self { session, kind }))
        }

        /// Keeps owned tensors on the session thread through inference, including after caller cancellation.
        pub async fn run(
            self: Arc<Self>,
            input: ImageTensor,
            timings: Timings,
        ) -> Result<ModelOutput, OcrError> {
            let queued = timings.start(TimingStage::OcrQueue);
            let kind = self.kind;
            self.session
                .run(move |session| {
                    drop(queued);
                    let _timer = timings.start(kind.timing());
                    let outputs = session.run(
                    ort::inputs! {"x"=>TensorRef::from_array_view(&input.0)?},
                )?;
                    kind.read(&outputs)
                })
                .await?
        }
    }

    impl crate::OcrArtifacts {
        /// Reads each configured model file set without introducing filesystem access into shared inference.
        pub async fn from_config(
            config: &docparse_config::OcrConfig,
        ) -> Result<Self, OcrError> {
            let config = config.clone();
            docparse_layout::wasm_compat::run_cpu(move || {
                Ok::<_, OcrError>(Self {
                    detection: (&config.detection).try_into()?,
                    recognition: (&config.recognition).try_into()?,
                    orientation: if config.classify_orientation {
                        Some((&config.orientation).try_into()?)
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
            backend: OnnxBackend,
            kind: ModelKind,
        ) -> Result<Arc<Self>, OcrError> {
            let mut session = SessionBuilder::try_from(backend)?
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
