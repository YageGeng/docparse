//! Native Texo owners share a bounded ready-crop queue across pages and documents.
use super::{Generation, MAX_LENGTH, ModelSessions, StepOutput};
use crate::TexoArtifacts;
use docparse_common::timing::TimingContext;
use docparse_common::timing::{StageTimer, TimingStage, Timings};
use docparse_common::{
    SessionManager as SharedSessionManager, SessionRequest, run_cpu,
};
use docparse_formula::FormulaError;
use docparse_layout::{PageImage, wasm_compat::OnnxBackend};
use ort::{session::builder::SessionBuilder, value::Tensor};
use std::sync::Arc;
use tokio::sync::oneshot;

type BatchResult = Result<Vec<Result<String, FormulaError>>, FormulaError>;

/// One crop keeps its caller lifetime and attribution even when neighboring crops belong to other PDFs.
#[derive(typed_builder::TypedBuilder)]
struct Request {
    // Keep the render delivery occupied until actual inference and input cleanup finish.
    #[builder(default = docparse_common::PageLease::current())]
    _page_lease: Option<docparse_common::PageLease>,
    image: Arc<PageImage>,
    response: oneshot::Sender<Result<String, FormulaError>>,
    caller: Arc<oneshot::Sender<()>>,
    context: TimingContext,
    #[builder(default)]
    queued: Option<StageTimer>,
}

impl Request {
    /// Blocking response receivers outlive canceled async callers, so both lifetimes must be checked.
    fn cancelled(&self) -> bool {
        self.response.is_closed() || self.caller.is_closed()
    }

    /// Ends admission timing outside the queue mutex while retaining the original tracing context.
    fn end_queue(&mut self) {
        let queued = self.queued.take();
        self.context.in_scope(|| drop(queued));
    }

    /// Attributes shared execution time to each original crop while preserving its page and tracing context.
    fn measure<T>(
        requests: &[Self],
        stage: TimingStage,
        operation: impl FnOnce() -> T,
    ) -> T {
        let timings: docparse_common::timing::BatchTimings =
            requests.iter().map(|request| &request.context).collect();
        let _timers = timings.start(stage);
        operation()
    }

    /// Maps each ordered result to its original caller; only model-wide failures affect the entire batch.
    fn complete(requests: Vec<Self>, result: BatchResult) {
        let mut outputs = result.and_then(|outputs| {
            if outputs.len() != requests.len() {
                return Err(FormulaError::Invalid(
                    "Texo batch result count mismatch".into(),
                ));
            }
            Ok(outputs.into_iter())
        });
        for request in requests {
            let cancelled = request.cancelled();
            let result = match &mut outputs {
                Ok(outputs) => outputs.next().expect("validated result count"),
                Err(error) => Err(FormulaError::Invalid(format!(
                    "Texo batch failed: {error}"
                ))),
            };
            request.context.in_scope(|| {
                if !cancelled && let Err(error) = &result {
                    tracing::warn!("Texo crop failed: {}", error);
                }
                let _ = request.response.send(result);
            });
        }
    }
}

impl SessionRequest for Request {
    /// Shares cancellation filtering with every native model queue.
    fn cancelled(&self) -> bool {
        self.cancelled()
    }
    /// Keeps original request attribution when the shared queue releases admission.
    fn end_queue(&mut self) {
        self.end_queue();
    }
}

/// Texo-specific generation policy delegates queue and thread ownership to common.
pub(crate) struct SessionManager {
    sessions: Arc<SharedSessionManager<Request>>,
    packet_size: usize,
}

impl SessionManager {
    /// Reads pending pressure from the shared CPU/GPU queue.
    pub(crate) fn pressure(
        &self,
    ) -> Arc<docparse_common::queue::QueuePressure> {
        self.sessions.pressure()
    }

    /// Builds all session pairs once; verified artifact bytes are shared only during initialization.
    pub(crate) async fn load(
        artifacts: TexoArtifacts,
        backend: OnnxBackend,
        config: &docparse_config::FormulaConfig,
    ) -> Result<Arc<Self>, FormulaError> {
        let docparse_config::FormulaEngineConfig::Texo(texo) = &config.engine
        else {
            return Err(FormulaError::Invalid(
                "Texo session manager requires the Texo engine".into(),
            ));
        };
        let (sessions, batch_size, queue_size) = (
            texo.cpu_session_size + texo.gpu_session_size,
            config.batch_size,
            config.queue_size,
        );
        let cpu_count = texo.cpu_session_size;
        let cpu_intra_threads = texo.cpu_intra_threads;
        run_cpu(move || {
            Self::start(sessions, batch_size, queue_size, move |index| {
                let backend = backend.formula_worker(index, cpu_count)?;
                let device = (backend.execution_provider()
                    == docparse_layout::ExecutionProvider::Cuda)
                    .then_some(ort::memory::AllocationDevice::CUDA);
                tracing::info!(
                    "initializing Texo consumer {} on {}",
                    index,
                    backend.execution_provider()
                );
                // CPU consumers use their configured operator pool; CUDA host work stays at one thread.
                let intra_threads = if backend.execution_provider()
                    == docparse_layout::ExecutionProvider::Cpu
                {
                    cpu_intra_threads
                } else {
                    1
                };
                // Both graphs inherit the global graph and memory settings without decoder overrides.
                let mut model = ModelSessions {
                    encoder: SessionBuilder::try_from(backend)?
                        .with_intra_threads(intra_threads)
                        .map_err(ort::Error::from)?
                        .commit_from_memory(&artifacts.encoder)?,
                    decoder: SessionBuilder::try_from(backend)?
                        .with_intra_threads(intra_threads)
                        .map_err(ort::Error::from)?
                        .commit_from_memory(&artifacts.decoder)?,
                    tokenizer: ModelSessions::tokenizer(&artifacts.tokenizer)?,
                };
                Ok(move |requests: &mut Vec<Request>| {
                    model.recognize(requests, device)
                })
            })
        })
        .await?
    }

    /// Initializes each owner on its execution thread and releases the queue mutex before model work.
    fn start<F, W>(
        sessions: usize,
        batch_size: usize,
        queue_size: usize,
        initialize: F,
    ) -> Result<Arc<Self>, FormulaError>
    where
        F: Fn(usize) -> Result<W, FormulaError> + Send + Sync + 'static,
        W: FnMut(&mut Vec<Request>) -> BatchResult + 'static,
    {
        let owners = SharedSessionManager::start_indexed(
            "formula_texo",
            sessions,
            batch_size,
            queue_size,
            move |index| {
                let mut model = initialize(index)?;
                Ok::<_, FormulaError>(move |mut requests: Vec<Request>| {
                    let result = model(&mut requests);
                    Request::complete(requests, result);
                })
            },
        )?;
        Ok(Arc::new(Self {
            sessions: owners,
            // Atomic producer packets must fit even when capacity is smaller than one inference batch.
            packet_size: batch_size.min(queue_size),
        }))
    }

    /// Publishes ready caller batches atomically and reconstructs crop order across independently completed model batches.
    pub(crate) async fn run(
        self: Arc<Self>,
        images: Vec<Arc<PageImage>>,
        timings: Timings,
    ) -> Result<Vec<String>, FormulaError> {
        let (caller, _lifetime) = oneshot::channel();
        let caller = Arc::new(caller);
        // Start every crop's queue interval before blocking-pool admission or bounded-channel backpressure.
        let requests: Vec<_> = images
            .into_iter()
            .map(|image| {
                let (response, receiver) = oneshot::channel();
                let request = Request::builder()
                    .image(image)
                    .response(response)
                    .caller(Arc::clone(&caller))
                    .queued(Some(timings.start(TimingStage::FormulaQueue)))
                    .context(TimingContext::new(timings.clone()))
                    .build();
                (request, receiver)
            })
            .collect();
        // Retain the last manager owner off the inference/async threads through finite native work, including cancellation.
        run_cpu(move || {
            let mut responses = Vec::with_capacity(requests.len());
            let mut packet = Vec::with_capacity(self.packet_size);
            for (request, receiver) in requests {
                if caller.is_closed() {
                    break;
                }
                packet.push(request);
                responses.push(receiver);
                if packet.len() == self.packet_size {
                    self.sessions.send_batch(std::mem::take(&mut packet))?;
                }
            }
            if !packet.is_empty() {
                self.sessions.send_batch(packet)?;
            }
            responses
                .into_iter()
                .map(|response| {
                    response.blocking_recv().map_err(|error| {
                        FormulaError::Invalid(format!(
                            "Texo response lost: {error}"
                        ))
                    })?
                })
                .collect()
        })
        .await?
    }
}

impl ModelSessions {
    /// Keeps each batch's KV cache local to its owner; canceled peers cannot terminate another page's generation.
    fn recognize(
        &mut self,
        requests: &mut Vec<Request>,
        device: Option<ort::memory::AllocationDevice>,
    ) -> BatchResult {
        let mut values = Vec::new();
        // Validate and prepare each crop separately so malformed input from one PDF cannot fail its batch peers.
        for request in std::mem::take(requests) {
            if request.cancelled() {
                continue;
            }
            let input = Request::measure(
                std::slice::from_ref(&request),
                TimingStage::FormulaPreprocess,
                || crate::preprocess::preprocess(&request.image),
            );
            match input {
                Ok(input) => {
                    values.extend(input);
                    requests.push(request);
                }
                Err(error) => {
                    Request::complete(vec![request], Ok(vec![Err(error)]))
                }
            }
        }
        if requests.is_empty() {
            return Ok(Vec::new());
        }
        if requests.iter().all(Request::cancelled) {
            return Err(FormulaError::Invalid("Texo batch canceled".into()));
        }
        let input = crate::preprocess::FormulaInput(
            ndarray::Array4::from_shape_vec(
                (
                    requests.len(),
                    3,
                    crate::preprocess::IMAGE_SIZE,
                    crate::preprocess::IMAGE_SIZE,
                ),
                values,
            )
            .map_err(|error| FormulaError::Invalid(error.to_string()))?,
        );
        let options = ort::session::RunOptions::new()?;
        let generation =
            Request::measure(requests, TimingStage::FormulaInference, || {
                let memory = device
                    .map(|device| {
                        ort::memory::MemoryInfo::new(
                            device,
                            0,
                            ort::memory::AllocatorType::Device,
                            ort::memory::MemoryType::Default,
                        )
                    })
                    .transpose()?;
                let pixels = Tensor::from_array(input.0)?;
                let hidden = if let Some(memory) = &memory {
                    let mut binding = self.encoder.create_binding()?;
                    binding.bind_input("pixel_values", &pixels)?;
                    binding
                        .bind_output_to_device("last_hidden_state", memory)?;
                    let physical = docparse_common::telemetry::Inference::new(
                        "formula_texo",
                        "encoder",
                        requests.len(),
                    );
                    let outputs = self
                        .encoder
                        .run_binding_with_options(&binding, &options);
                    physical.finish(outputs.is_ok());
                    let mut outputs = outputs?;
                    binding.synchronize_outputs()?;
                    outputs.remove("last_hidden_state")
                } else {
                    let physical = docparse_common::telemetry::Inference::new(
                        "formula_texo",
                        "encoder",
                        requests.len(),
                    );
                    let outputs = self.encoder.run_with_options(
                        ort::inputs!["pixel_values" => pixels],
                        &options,
                    );
                    physical.finish(outputs.is_ok());
                    let mut outputs = outputs?;
                    outputs.remove("last_hidden_state")
                }
                .ok_or_else(|| {
                    FormulaError::Invalid("missing Texo image features".into())
                })?;
                let mut generation = Generation::new(hidden, requests.len())?;
                for _ in 1..MAX_LENGTH {
                    if requests.iter().all(Request::cancelled) {
                        return Err(FormulaError::Invalid(
                            "Texo batch canceled".into(),
                        ));
                    }
                    if generation
                        .cancel(requests.iter().map(Request::cancelled))
                    {
                        break;
                    }
                    let output = if let Some(memory) = &memory {
                        // Growing caches need fresh output bindings; inputs must not be overwritten by the same decode step.
                        let mut binding = self.decoder.create_binding()?;
                        for (name, value) in generation.inputs()? {
                            binding.bind_input(name, &*value)?;
                        }
                        for name in crate::model::PRESENT_NAMES {
                            binding.bind_output_to_device(name, memory)?;
                        }
                        let cpu = ort::memory::MemoryInfo::new(
                            ort::memory::AllocationDevice::CPU,
                            0,
                            ort::memory::AllocatorType::Device,
                            ort::memory::MemoryType::Default,
                        )?;
                        binding.bind_output_to_device("logits", &cpu)?;
                        let physical =
                            docparse_common::telemetry::Inference::new(
                                "formula_texo",
                                "decoder",
                                requests.len(),
                            );
                        let outputs = self
                            .decoder
                            .run_binding_with_options(&binding, &options);
                        physical.finish(outputs.is_ok());
                        let outputs = outputs?;
                        binding.synchronize_outputs()?;
                        StepOutput::try_from(outputs)?
                    } else {
                        let inputs = generation.inputs()?;
                        let physical =
                            docparse_common::telemetry::Inference::new(
                                "formula_texo",
                                "decoder",
                                requests.len(),
                            );
                        let outputs =
                            self.decoder.run_with_options(inputs, &options);
                        physical.finish(outputs.is_ok());
                        StepOutput::try_from(outputs?)?
                    };
                    if generation.advance(output)? {
                        break;
                    }
                    if let Some(device) = device {
                        generation.verify_device(device)?;
                    }
                }
                Ok::<_, FormulaError>(generation)
            })?;
        Ok(Request::measure(
            requests,
            TimingStage::FormulaDecode,
            || generation.decode(&self.tokenizer),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use docparse_common::queue::BlockingQueue as BatchQueue;
    use std::sync::Mutex;

    /// A failed owner must wake callers blocked on either bounded admission or an unprocessed reply.
    #[tokio::test]
    #[allow(clippy::panic)] // Deliberately exercises the model owner's unwind cleanup.
    async fn panicked_owner_closes_pending_admission() {
        let manager = run_cpu(|| {
            SessionManager::start(1, 2, 2, |_| {
                Ok(|_: &mut Vec<Request>| -> BatchResult {
                    panic!("simulated model failure")
                })
            })
        })
        .await
        .expect("task")
        .expect("manager");
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            Arc::clone(&manager)
                .run(vec![request(1).0.image; 8], Timings::default()),
        )
        .await;
        // Always unblock the producer before asserting, including when testing a broken owner-exit path.
        manager.sessions.close();
        result
            .expect("owner failure must release queued callers")
            .expect_err("failed worker");
        run_cpu(move || drop(manager)).await.expect("shutdown");
    }

    /// Ready crops fill the limit across caller packet boundaries without escaping capacity accounting.
    #[test]
    fn full_batches_and_tail_spills_preserve_queue_accounting() {
        let queue = BatchQueue::new("test", 19);
        let mut lifetimes = Vec::new();
        let mut replies = Vec::new();
        let mut value = 0;
        for size in [2, 8, 3, 6] {
            let mut packet = Vec::new();
            for _ in 0..size {
                let (request, reply, lifetime) = request(value);
                value += 1;
                packet.push(request);
                replies.push(reply);
                lifetimes.push(lifetime);
            }
            queue.push_batch(packet).expect("capacity");
        }
        for size in [8, 8, 3] {
            let requests = queue.pop(8).expect("ready batch");
            assert_eq!(requests.len(), size);
            let output = requests
                .iter()
                .map(|request| {
                    Ok(request.image.data().first().expect("pixel").to_string())
                })
                .collect();
            Request::complete(requests, Ok(output));
        }
        queue.close();
        assert!(queue.pop(8).is_none());
        for (value, reply) in replies.into_iter().enumerate() {
            assert_eq!(
                reply.blocking_recv().expect("reply").expect("result"),
                value.to_string()
            );
        }
    }

    /// one already-ready caller batch should not need additional model invocations.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ready_call_batches_remain_whole() {
        let records = Arc::new(Mutex::new(Vec::new()));
        let counts = Arc::clone(&records);
        let manager = run_cpu(move || {
            SessionManager::start(2, 8, 16, move |_| {
                let counts = Arc::clone(&counts);
                Ok(move |requests: &mut Vec<Request>| {
                    counts.lock().expect("records").push(requests.len());
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    Ok(requests.iter().map(|_| Ok("x".to_owned())).collect())
                })
            })
        })
        .await
        .expect("task")
        .expect("manager");
        let image = request(1).0.image;
        let mut split = 0;
        let mut examples = Vec::new();
        for _ in 0..300 {
            records.lock().expect("records").clear();
            let result = Arc::clone(&manager)
                .run(vec![Arc::clone(&image); 8], Timings::default())
                .await
                .expect("result");
            assert_eq!(result.len(), 8);
            let sizes = records.lock().expect("records").clone();
            if sizes.len() > 1 {
                split += 1;
                if examples.len() < 10 {
                    examples.push(sizes);
                }
            }
        }
        run_cpu(move || drop(manager)).await.expect("shutdown");
        assert_eq!(
            split, 0,
            "already-ready caller batches were fragmented: {examples:?}"
        );
    }

    /// Merged work keeps separate document timing sinks and rejects truncated result arrays for every caller.
    #[test]
    fn batch_timings_and_result_count_preserve_caller_boundaries() {
        let mut requests = Vec::new();
        let mut observations = Vec::new();
        let mut replies = Vec::new();
        let mut lifetimes = Vec::new();
        for page in [3, 7] {
            let (mut request, reply, lifetime) = request(page);
            let (timings, receiver) = Timings::channel();
            request.context.timings = timings.for_page(u32::from(page));
            requests.push(request);
            observations.push(receiver);
            replies.push(reply);
            lifetimes.push(lifetime);
        }
        Request::measure(&requests, TimingStage::FormulaInference, || ());
        for (mut events, page) in observations.into_iter().zip([3, 7]) {
            let event = events.try_recv().expect("original timing sink");
            assert_eq!(event.page_number, Some(page));
            assert_eq!(event.stage, TimingStage::FormulaInference);
            events
                .try_recv()
                .expect_err("exactly one timing observation");
        }
        Request::complete(requests, Ok(vec![Ok("missing peer".into())]));
        for reply in replies {
            let error = reply
                .blocking_recv()
                .expect("response")
                .expect_err("count mismatch");
            assert!(error.to_string().contains("count mismatch"));
        }
    }

    /// Constructs distinguishable crops with independent page attribution and caller lifetimes.
    fn request(
        value: u8,
    ) -> (
        Request,
        oneshot::Receiver<Result<String, FormulaError>>,
        oneshot::Receiver<()>,
    ) {
        let image = Arc::new(
            PageImage::try_from(
                docparse_layout::PageImageInput::builder()
                    .width(1)
                    .height(1)
                    .pixel_format(docparse_layout::PixelFormat::Rgb8)
                    .data(Arc::from(vec![value; 3]))
                    .build(),
            )
            .expect("image"),
        );
        let (response, reply) = oneshot::channel();
        let (caller, lifetime) = oneshot::channel();
        (
            Request::builder()
                .image(image)
                .response(response)
                .caller(Arc::new(caller))
                .context(TimingContext::new(
                    Timings::default().for_page(u32::from(value)),
                ))
                .build(),
            reply,
            lifetime,
        )
    }

    /// Cross-page batches skip canceled crops, flush short tails, and isolate sequence failures.
    #[test]
    fn ready_crops_preserve_order_and_isolate_errors() {
        let queue = BatchQueue::new("test", 5);
        let mut replies = Vec::new();
        let mut lifetimes = Vec::new();
        for value in 0..5 {
            let (request, reply, lifetime) = request(value);
            queue.push_batch(vec![request]).expect("capacity");
            replies.push(reply);
            if value != 1 {
                lifetimes.push(lifetime);
            }
        }
        let requests = queue.pop(3).expect("first");
        assert_eq!(
            requests
                .iter()
                .map(|request| *request.image.data().first().expect("pixel"))
                .collect::<Vec<_>>(),
            vec![0, 2, 3]
        );
        Request::complete(
            requests,
            Ok(vec![
                Ok("zero".into()),
                Err(FormulaError::Invalid("unfinished peer".into())),
                Ok("three".into()),
            ]),
        );
        assert_eq!(
            replies
                .remove(0)
                .blocking_recv()
                .expect("reply")
                .expect("success"),
            "zero"
        );
        assert!(
            replies.remove(0).blocking_recv().is_err(),
            "canceled work is discarded"
        );
        replies
            .remove(0)
            .blocking_recv()
            .expect("reply")
            .expect_err("isolated failure");
        assert_eq!(
            replies
                .remove(0)
                .blocking_recv()
                .expect("reply")
                .expect("result"),
            "three"
        );
        let requests = queue.pop(3).expect("tail");
        assert_eq!(requests.len(), 1);
        Request::complete(requests, Ok(vec![Ok("four".into())]));
        for (reply, expected) in replies.into_iter().zip(["four"]) {
            assert_eq!(
                reply.blocking_recv().expect("reply").expect("success"),
                expected
            );
        }
        queue.close();
        assert!(queue.pop(3).is_none());
    }

    /// Two owners execute concurrently, bounded admission backpressures, and canceling one caller preserves its peer.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn session_owners_share_capacity_without_sharing_cancellation() {
        let (started, mut events) = std::sync::mpsc::channel();
        let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        struct Release(Arc<(Mutex<bool>, std::sync::Condvar)>);
        impl Drop for Release {
            /// Releases test workers during unwinding so a failed assertion cannot hang runtime shutdown.
            fn drop(&mut self) {
                *self.0.0.lock().expect("gate") = true;
                self.0.1.notify_all();
            }
        }
        let _release = Release(Arc::clone(&gate));
        let worker_gate = Arc::clone(&gate);
        let manager = run_cpu(move || {
            SessionManager::start(2, 2, 3, move |_| {
                let gate = Arc::clone(&worker_gate);
                let started = started.clone();
                Ok(move |requests: &mut Vec<Request>| {
                    started
                        .send((std::thread::current().id(), requests.len()))
                        .expect("observation");
                    let (mutex, wake) = &*gate;
                    let mut released = mutex.lock().expect("gate");
                    while !*released {
                        released = wake.wait(released).expect("release");
                    }
                    Ok(requests
                        .iter()
                        .map(|request| {
                            Ok(request
                                .image
                                .data()
                                .first()
                                .expect("pixel")
                                .to_string())
                        })
                        .collect())
                })
            })
        })
        .await
        .expect("task")
        .expect("manager");
        let mut tasks = Vec::new();
        let mut owners = Vec::new();
        for value in [10, 11] {
            let image = request(value).0.image;
            let owner = Arc::clone(&manager);
            tasks.push(tokio::spawn(async move {
                owner.run(vec![image], Timings::default()).await
            }));
            // Wait for one owner to block before submitting its peer, otherwise the two requests may correctly coalesce.
            let (receiver, owner) = run_cpu(move || {
                let event = events
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .expect("owner running");
                (events, event.0)
            })
            .await
            .expect("observation");
            events = receiver;
            owners.push(owner);
        }
        assert_ne!(
            owners.first(),
            owners.get(1),
            "independent execution threads"
        );
        let mut queued = Vec::new();
        let mut lifetimes = Vec::new();
        for value in 20..23 {
            let (request, reply, lifetime) = request(value);
            manager
                .sessions
                .send_batch(vec![request])
                .expect("bounded capacity");
            queued.push(reply);
            lifetimes.push(lifetime);
        }
        let mut packet = Vec::new();
        for value in 23..24 {
            let (request, reply, lifetime) = request(value);
            packet.push(request);
            queued.push(reply);
            lifetimes.push(lifetime);
        }
        let queue = Arc::clone(&manager.sessions);
        let (entered, waiting) = oneshot::channel();
        let mut blocked = tokio::task::spawn_blocking(move || {
            let _ = entered.send(());
            queue.send_batch(packet)
        });
        waiting.await.expect("producer started");
        // Three configured slots must block a fourth crop even though two sessions times two batch slots equals four.
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(50),
                &mut blocked
            )
            .await
            .is_err(),
            "a full queue must backpressure the complete packet"
        );
        let canceled = tasks.remove(0);
        canceled.abort();
        assert!(canceled.await.expect_err("cancelled caller").is_cancelled());
        *gate.0.lock().expect("gate") = true;
        gate.1.notify_all();
        assert_eq!(
            tasks.remove(0).await.expect("task").expect("peer result"),
            vec!["11"]
        );
        blocked
            .await
            .expect("producer")
            .expect("admitted after capacity release");
        for (reply, expected) in queued.into_iter().zip(20..24) {
            assert_eq!(
                reply.await.expect("reply").expect("crop"),
                expected.to_string()
            );
        }
        assert!(
            events.try_iter().any(|(_, size)| size == 2),
            "ready crops from different pages are merged"
        );
        let output = Arc::clone(&manager)
            .run(vec![request(30).0.image], Timings::default())
            .await
            .expect("reuse after cancellation");
        assert_eq!(output, vec!["30"]);
        run_cpu(move || drop(manager)).await.expect("shutdown");
    }

    /// Producer packets split at queue capacity without losing order when one inference batch is larger.
    #[tokio::test]
    async fn queue_smaller_than_batch_accepts_every_crop() {
        let manager = run_cpu(|| {
            SessionManager::start(1, 8, 1, |_| {
                Ok(|requests: &mut Vec<Request>| {
                    Ok(requests
                        .iter()
                        .map(|request| {
                            Ok(request
                                .image
                                .data()
                                .first()
                                .expect("pixel")
                                .to_string())
                        })
                        .collect())
                })
            })
        })
        .await
        .expect("startup task")
        .expect("manager");
        let output = Arc::clone(&manager)
            .run(
                (0..12).map(|value| request(value).0.image).collect(),
                Timings::default(),
            )
            .await
            .expect("all crops");
        assert_eq!(
            output,
            (0..12).map(|value| value.to_string()).collect::<Vec<_>>()
        );
        run_cpu(move || drop(manager)).await.expect("shutdown");
    }

    /// A partially initialized pool closes admission and drops earlier owners when a later model fails to load.
    #[test]
    fn failed_initialization_releases_existing_sessions() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Owner(Arc<AtomicUsize>);
        impl Drop for Owner {
            /// Records destruction on the owning worker even when it never receives a request.
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let created = AtomicUsize::new(0);
        let destroyed = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&destroyed);
        let result = SessionManager::start(2, 2, 4, move |_| {
            if created.fetch_add(1, Ordering::SeqCst) == 1 {
                return Err(FormulaError::Invalid("load failure".into()));
            }
            let owner = Owner(Arc::clone(&observed));
            Ok(move |_: &mut Vec<Request>| {
                let _ = &owner;
                Ok(Vec::new())
            })
        });
        assert!(result.is_err());
        assert_eq!(destroyed.load(Ordering::SeqCst), 1);
    }

    /// Exercise the same growing-cache I/O-binding path on CPU when CUDA hardware is absent.
    #[tokio::test]
    #[ignore = "requires models/texo"]
    async fn bound_outputs_preserve_batched_and_repeated_results() {
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../models/texo");
        let paths = docparse_config::TexoFormulaConfig::builder()
            .encoder_path(directory.join("encoder_model.onnx"))
            .decoder_path(directory.join("decoder_model_merged.onnx"))
            .tokenizer_path(directory.join("tokenizer.json"))
            .build();
        let artifacts = TexoArtifacts::try_from(&paths).expect("artifacts");
        artifacts.verify().expect("identity");
        let runner = run_cpu(move || {
            SessionManager::start(2, 4, 8, move |_| {
                let backend = OnnxBackend::compiled();
                // Match production memory policy while exercising the I/O-binding path.
                let mut model = ModelSessions {
                    encoder: SessionBuilder::try_from(backend)?
                        .with_intra_threads(1)
                        .map_err(ort::Error::from)?
                        .commit_from_memory(&artifacts.encoder)?,
                    decoder: SessionBuilder::try_from(backend)?
                        .with_intra_threads(1)
                        .map_err(ort::Error::from)?
                        .commit_from_memory(&artifacts.decoder)?,
                    tokenizer: ModelSessions::tokenizer(&artifacts.tokenizer)?,
                };
                Ok(move |requests: &mut Vec<Request>| {
                    model.recognize(
                        requests,
                        Some(ort::memory::AllocationDevice::CPU),
                    )
                })
            })
        })
        .await
        .expect("task")
        .expect("sessions");
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/reference.json"
        ))
        .expect("reference");
        let mut images = Vec::new();
        let mut expected = Vec::new();
        for case in reference
            .get("cases")
            .expect("cases")
            .as_array()
            .expect("array")
        {
            let image = image::open(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures")
                    .join(
                        case.get("image")
                            .expect("image")
                            .as_str()
                            .expect("string"),
                    ),
            )
            .expect("PNG")
            .to_rgb8();
            images.push(Arc::new(
                PageImage::try_from(
                    docparse_layout::PageImageInput::builder()
                        .width(image.width())
                        .height(image.height())
                        .pixel_format(docparse_layout::PixelFormat::Rgb8)
                        .data(Arc::from(image.into_raw()))
                        .build(),
                )
                .expect("pixels"),
            ));
            expected.push(
                case.get("latex")
                    .expect("latex")
                    .as_str()
                    .expect("string")
                    .to_owned(),
            );
        }
        for _ in 0..2 {
            assert_eq!(
                Arc::clone(&runner)
                    .run(images.clone(), Timings::default())
                    .await
                    .expect("bound batch"),
                expected
            );
        }
    }
}
