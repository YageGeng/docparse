//! Independent OCR session consumers batch compatible ready inputs across pages.
use crate::{
    OcrError,
    model::{ModelKind, ModelOutput},
    preprocess::ImageTensor,
};
use docparse_common::timing::TimingContext;
use docparse_common::timing::{StageTimer, TimingStage, Timings};
use docparse_layout::wasm_compat::OnnxBackend;
use ort::{session::builder::SessionBuilder, value::TensorRef};
use std::sync::Arc;
use tokio::sync::oneshot;

/// One input remains owned until execution/readback ends, even after its original caller cancels.
#[derive(typed_builder::TypedBuilder)]
struct Request {
    // Keep the render delivery occupied until actual inference and input cleanup finish.
    #[builder(default = docparse_common::PageLease::current())]
    _page_lease: Option<docparse_common::PageLease>,
    input: ImageTensor,
    response: oneshot::Sender<Result<ModelOutput, OcrError>>,
    context: TimingContext,
    #[builder(default)]
    queued: Option<StageTimer>,
    #[builder(default)]
    caller: Option<oneshot::Sender<()>>,
}

impl Request {
    /// Finite native reply waits must also observe the original async caller's lifetime.
    fn cancelled(&self) -> bool {
        self.response.is_closed()
            || self.caller.as_ref().is_some_and(oneshot::Sender::is_closed)
    }
    /// Finishes queue timing under the correct page and subscriber.
    fn end_queue(&mut self) {
        let timer = self.queued.take();
        self.context.in_scope(|| drop(timer));
    }
    /// Groups only the drained ready inputs, preserving each input's original tensor dimensions.
    fn groups(mut requests: Vec<Self>) -> Vec<Vec<Self>> {
        requests.sort_by_key(|request| request.input.0.dim());
        let mut groups: Vec<Vec<Self>> = Vec::new();
        for request in requests {
            if let Some(group) = groups.last_mut()
                && group.first().is_some_and(|first| {
                    first.input.0.dim() == request.input.0.dim()
                })
            {
                group.push(request);
            } else {
                groups.push(vec![request]);
            }
        }
        groups
    }
    /// Delivers independent page/line outputs and preserves model-wide failure causes.
    fn complete(
        requests: Vec<Self>,
        result: Result<Vec<ModelOutput>, OcrError>,
    ) {
        let result = result.and_then(|outputs| {
            if outputs.len() == requests.len() {
                Ok(outputs)
            } else {
                Err(OcrError::InvalidData(
                    "OCR batch result count mismatch".into(),
                ))
            }
        });
        match result {
            Ok(outputs) => {
                for (request, output) in requests.into_iter().zip(outputs) {
                    let _ = request.response.send(Ok(output));
                }
            }
            Err(error) => {
                let error = Arc::new(error);
                for request in requests {
                    let _ = request
                        .response
                        .send(Err(OcrError::Shared(Arc::clone(&error))));
                }
            }
        }
    }
}

impl docparse_common::SessionRequest for Request {
    /// Prevents canceled inputs from occupying model batch slots.
    fn cancelled(&self) -> bool {
        self.cancelled()
    }
    /// Ends attribution when a session takes the request.
    fn end_queue(&mut self) {
        self.end_queue();
    }
}

impl SessionRunner {
    /// Submits individual model inputs so callers do not establish artificial page batch boundaries.
    pub async fn run(
        self: Arc<Self>,
        input: ImageTensor,
        timings: Timings,
    ) -> Result<ModelOutput, OcrError> {
        if input.0.dim().0 != 1 {
            return Err(OcrError::InvalidData(
                "OCR queue expects one input per request".into(),
            ));
        }
        let (response, receiver) = oneshot::channel();
        let request = Request::builder()
            .input(input)
            .response(response)
            .queued(Some(timings.start(TimingStage::OcrQueue)))
            .context(TimingContext::new(timings))
            .build();
        self.submit(request, receiver).await
    }
}

#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
mod platform {
    use super::*;
    use docparse_common::{SessionManager, run_cpu};

    /// Each OCR stage owns independent sessions consuming one shared ready-input queue.
    pub(crate) struct SessionRunner {
        manager: Arc<SessionManager<Request>>,
    }

    impl SessionRunner {
        /// Loads owners over an explicitly sized queue and merges only equal-shaped ready inputs.
        pub async fn load(
            bytes: Arc<[u8]>,
            backend: OnnxBackend,
            kind: ModelKind,
            session_size: usize,
            batch_size: usize,
            queue_size: usize,
        ) -> Result<Arc<Self>, OcrError> {
            let manager = SessionManager::load(kind.metric_name(), session_size, batch_size, queue_size, move || {
                // Inherit the global session policy instead of overriding memory patterns per model.
                let mut session = SessionBuilder::try_from(backend)?
                    .commit_from_memory(&bytes)?;
                kind.validate_session(&session)?;
                Ok::<_, OcrError>(move |requests: Vec<Request>| {
                    for requests in Request::groups(requests) {
                            let timings: docparse_common::timing::BatchTimings = requests.iter().map(|request| &request.context).collect();
                        let result = (|| {
                            let input = ImageTensor::try_from(requests.iter().map(|request| &request.input).collect::<Vec<_>>().as_slice())?;
                            let timers = timings.start_unique(kind.timing());
                            let physical = docparse_common::telemetry::Inference::new(kind.metric_name(), "model", requests.len());
                            let outputs = session.run(ort::inputs! { "x" => TensorRef::from_array_view(&input.0)? });
                            physical.finish(outputs.is_ok());
                            drop(timers);
                            let outputs = outputs?;
                            let timers = timings.start_unique(TimingStage::OcrReadback);
                            let result = kind.read(&outputs, requests.len());
                            drop(timers);
                            result
                        })();
                        Request::complete(requests, result);
                    }
                })
            }).await?;
            Ok(Arc::new(Self { manager }))
        }

        /// Keeps the last session owner outside its own execution thread through finite native work.
        pub(super) async fn submit(
            self: Arc<Self>,
            mut request: Request,
            receiver: oneshot::Receiver<Result<ModelOutput, OcrError>>,
        ) -> Result<ModelOutput, OcrError> {
            let (caller, _lifetime) = oneshot::channel();
            request.caller = Some(caller);
            run_cpu(move || {
                self.manager.send(request)?;
                receiver
                    .blocking_recv()
                    .map_err(|error| OcrError::InvalidData(error.to_string()))?
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
            docparse_common::run_cpu(move || {
                Ok::<_, OcrError>(Self {
                    detection: (&config.detection.files).try_into()?,
                    recognition: (&config.recognition.files).try_into()?,
                    orientation: if config.classify_orientation {
                        Some((&config.orientation.files).try_into()?)
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

    /// Browser sessions consume the same queue while the existing global guard protects ORT Web.
    pub(crate) struct SessionRunner {
        sender: docparse_common::queue::QueueSender<Request>,
    }
    impl SessionRunner {
        /// Loads independent browser sessions without imposing page-local inference batch boundaries.
        pub async fn load(
            bytes: Arc<[u8]>,
            backend: OnnxBackend,
            kind: ModelKind,
            session_size: usize,
            batch_size: usize,
            queue_size: usize,
        ) -> Result<Arc<Self>, OcrError> {
            let (sender, receiver) = docparse_common::Queue::<Request>::new(
                kind.metric_name(),
                queue_size,
            );
            for _ in 0..session_size {
                // All three OCR stages inherit the same native/browser session policy.
                let mut session = SessionBuilder::try_from(backend)?
                    .commit_from_memory(&bytes)
                    .await?;
                kind.validate_session(&session)?;
                let options = RunOptions::new()?;
                let receiver = receiver.clone();
                let _worker = docparse_common::ThreadManager::spawn_async(
                    Box::pin(async move {
                        while let Some(batch) = receiver.recv().await {
                            let _guard = OnnxBackend::inference_guard().await;
                            let requests = batch.take_ready(batch_size);
                            for requests in Request::groups(requests) {
                                let timings: docparse_common::timing::BatchTimings =
                                requests
                                    .iter()
                                    .map(|request| &request.context)
                                    .collect();
                                let result = async {
                                let input = ImageTensor::try_from(requests.iter().map(|request| &request.input).collect::<Vec<_>>().as_slice())?;
                                let timers = timings.start_unique(kind.timing());
                                let outputs = session.run_async(ort::inputs! { "x" => TensorRef::from_array_view(&input.0)? }, &options).await;
                                drop(timers);
                                let mut outputs = outputs?;
                                let timers = timings.start_unique(TimingStage::OcrReadback);
                                let result = async {
                                    ort_web::sync_outputs(&mut outputs).await.map_err(|error| OcrError::InvalidData(error.to_string()))?;
                                    kind.read(&outputs, requests.len())
                                }.await;
                                drop(timers);
                                result
                            }.await;
                                Request::complete(requests, result);
                            }
                        }
                    }),
                )?;
            }
            Ok(Arc::new(Self { sender }))
        }
        /// Retains each input through its JavaScript inference and output readback.
        pub(super) async fn submit(
            self: Arc<Self>,
            request: Request,
            receiver: oneshot::Receiver<Result<ModelOutput, OcrError>>,
        ) -> Result<ModelOutput, OcrError> {
            self.sender
                .send(request)
                .await
                .map_err(|error| OcrError::InvalidData(error.to_string()))?;
            receiver
                .await
                .map_err(|error| OcrError::InvalidData(error.to_string()))?
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
