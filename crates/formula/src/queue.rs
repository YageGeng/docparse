//! Bounded per-crop admission and ready-only batching shared by formula engines.
use crate::FormulaError;
pub use docparse_common::timing::BatchTimings;
use docparse_common::timing::TimingContext;
use docparse_common::timing::{StageTimer, TimingStage, Timings};
use docparse_common::{Queue, SessionRequest};
use docparse_layout::PageImage;
use std::sync::Arc;
use tokio::sync::oneshot;
use typed_builder::TypedBuilder;

/// One sender represents the same queue across every page and document using an engine.
#[derive(Clone)]
pub struct FormulaQueue {
    sender: docparse_common::queue::QueueSender<FormulaRequest>,
}

impl FormulaQueue {
    /// Creates a bounded crop queue; capacity must be positive, as with Tokio channels.
    pub fn new(
        name: &'static str,
        capacity: usize,
    ) -> (Self, Queue<FormulaRequest>) {
        let (sender, receiver) = Queue::new(name, capacity);
        (Self { sender }, receiver)
    }

    /// Discards consumer identity only for the legacy text-only engine interface.
    pub async fn run(
        &self,
        images: Vec<Arc<PageImage>>,
        timings: Timings,
    ) -> Result<Vec<String>, FormulaError> {
        Ok(self
            .run_named(images, timings)
            .await?
            .into_iter()
            .map(|output| output.latex)
            .collect())
    }

    /// Submits independent crops with backpressure and reconstructs caller order after completion.
    pub async fn run_named(
        &self,
        images: Vec<Arc<PageImage>>,
        timings: Timings,
    ) -> Result<Vec<FormulaOutput>, FormulaError> {
        // Only producer handles expose submission; consumers use Queue<FormulaRequest> directly.
        let sender = &self.sender;
        if images
            .iter()
            .any(|image| image.width() == 0 || image.height() == 0)
        {
            tracing::warn!(
                "rejecting an empty formula crop before shared admission"
            );
            return Err(FormulaError::Invalid(
                "formula crop must not be empty".into(),
            ));
        }
        futures_util::future::try_join_all(images.into_iter().map(|image| {
            let (response, result) = oneshot::channel();
            let request = FormulaRequest::builder()
                .image(image)
                .response(response)
                .queued(Some(timings.start(TimingStage::FormulaQueue)))
                .context(TimingContext::new(timings.clone()))
                .build();
            async move {
                sender.send(request).await.map_err(|_closed| {
                    FormulaError::Invalid("formula queue stopped".into())
                })?;
                result.await.map_err(|_closed| {
                    FormulaError::Invalid("formula queue response lost".into())
                })?
            }
        }))
        .await
    }
}

/// Owned input, reply, and timing context for a single independently cancelable crop.
#[derive(TypedBuilder)]
pub struct FormulaRequest {
    pub image: Arc<PageImage>,
    pub context: TimingContext,
    response: oneshot::Sender<Result<FormulaOutput, FormulaError>>,
    /// Actual consumer assigned after dequeueing, retained in the page result.
    #[builder(default)]
    pub engine: String,
    #[builder(default)]
    queued: Option<StageTimer>,
    // Keep the render delivery occupied until actual inference and input cleanup finish.
    #[builder(default = docparse_common::PageLease::current())]
    _page_lease: Option<docparse_common::PageLease>,
    // Actual model requests retain resource admission after their async caller is cancelled.
    #[builder(default = image.resource_lease().or_else(docparse_common::ResourceLease::current))]
    _resources: Option<docparse_common::ResourceLease>,
}

impl FormulaRequest {
    /// Restores crop/document ownership when a consumer starts independently cancelable CPU work.
    pub fn resource_lease(&self) -> Option<docparse_common::ResourceLease> {
        self._resources.clone()
    }
    /// Reports whether the original caller has canceled or timed out.
    pub fn cancelled(&self) -> bool {
        self.response.is_closed()
    }

    /// Waits for cancellation while the actor retains ownership of the reply.
    pub async fn closed(&mut self) {
        self.response.closed().await;
    }

    /// Finishes admission timing under the original caller's tracing context.
    pub fn end_queue(&mut self) {
        let timer = self.queued.take();
        self.context.in_scope(|| drop(timer));
    }

    /// Completes one crop without letting a canceled caller affect its neighbors.
    pub fn complete(mut self, result: Result<String, FormulaError>) {
        self.end_queue();
        self.context.in_scope(|| {
            if !self.response.is_closed()
                && let Err(error) = &result
            {
                tracing::warn!("queued formula recognition failed: {}", error);
            }
            let _ = self.response.send(result.map(|latex| FormulaOutput {
                latex,
                engine: self.engine,
            }));
        });
    }

    /// Attributes a physical stage to all original crop observers without retaining pixels.
    pub fn measure<T>(
        requests: &[Self],
        stage: TimingStage,
        operation: impl FnOnce() -> T,
    ) -> T {
        let timings: BatchTimings =
            requests.iter().map(|request| &request.context).collect();
        let _timers = timings.start(stage);
        operation()
    }

    /// Routes model results individually and retains a shared source for model-wide failures.
    pub fn complete_batch(
        requests: Vec<Self>,
        result: Result<Vec<Result<String, FormulaError>>, FormulaError>,
    ) {
        let result = result.and_then(|outputs| {
            if outputs.len() != requests.len() {
                return Err(FormulaError::Invalid(
                    "formula batch result count mismatch".into(),
                ));
            }
            Ok(outputs)
        });
        match result {
            Ok(outputs) => {
                for (request, output) in requests.into_iter().zip(outputs) {
                    request.complete(output);
                }
            }
            Err(error) => {
                let error = Arc::new(error);
                for request in requests {
                    request.complete(Err(FormulaError::Shared(Arc::clone(
                        &error,
                    ))));
                }
            }
        }
    }
}

impl SessionRequest for FormulaRequest {
    /// Uses original response ownership to cancel shared-queue work.
    fn cancelled(&self) -> bool {
        self.cancelled()
    }
    /// Restores original timing attribution after the common queue releases its lock.
    fn end_queue(&mut self) {
        self.end_queue();
    }
}

/// Recognition text and the consumer that actually processed the crop.
#[derive(Debug)]
pub struct FormulaOutput {
    pub latex: String,
    pub engine: String,
}

/// Aborts receiver loops before joining native owners, including partial initialization failures.
#[derive(Default)]
pub struct FormulaWorkers {
    name: String,
    aborts: Vec<futures_util::future::AbortHandle>,
    owners: Vec<docparse_common::ThreadManager>,
}

impl FormulaWorkers {
    /// Names an execution group without exposing a request-submission interface.
    pub fn new(name: String) -> Self {
        Self {
            name,
            aborts: Vec::new(),
            owners: Vec::new(),
        }
    }

    /// Reports the initialized consumer family for lifecycle logs.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Stops partial consumer groups and awaits their native cleanup without blocking the async caller.
    pub async fn shutdown(mut self) -> Result<(), FormulaError> {
        for abort in &self.aborts {
            abort.abort();
        }
        for owner in std::mem::take(&mut self.owners) {
            owner.shutdown().await?;
        }
        Ok(())
    }

    /// Starts one independently owned consumer that can be stopped without closing peer groups.
    pub fn spawn(
        &mut self,
        future: docparse_common::WasmBoxedFuture<'static, ()>,
    ) -> Result<(), FormulaError> {
        let (abort, registration) =
            futures_util::future::AbortHandle::new_pair();
        let owner = docparse_common::ThreadManager::spawn_async(Box::pin(
            async move {
                let _ =
                    futures_util::future::Abortable::new(future, registration)
                        .await;
            },
        ))?;
        self.aborts.push(abort);
        self.owners.push(owner);
        Ok(())
    }
}

impl Drop for FormulaWorkers {
    /// Releases idle receivers before native joins; running model owners retain their inputs until completion.
    fn drop(&mut self) {
        for abort in &self.aborts {
            abort.abort();
        }
    }
}

/// One producer queue is served by every configured local or HTTP consumer group.
#[derive(TypedBuilder)]
pub struct FormulaPool {
    queue: FormulaQueue,
    admission: Arc<tokio::sync::Semaphore>,
    receiver: Queue<FormulaRequest>,
    workers: Vec<FormulaWorkers>,
    timeout: std::time::Duration,
}

impl FormulaPool {
    /// Builds infallible shared admission from validated active and pending capacities.
    pub fn new(config: &docparse_config::ValidatedConfig) -> Self {
        let config = config.formula();
        let (queue, receiver) = FormulaQueue::new("formula", config.queue_size);
        Self::builder()
            .queue(queue)
            .admission(Arc::new(tokio::sync::Semaphore::new(
                config.active_capacity() + config.queue_size,
            )))
            .receiver(receiver)
            .workers(Vec::new())
            .timeout(std::time::Duration::from_millis(config.timeout_ms))
            .build()
    }

    /// Gives execution groups receiving access without granting request submission.
    pub fn receiver(&self) -> Queue<FormulaRequest> {
        self.receiver.clone()
    }

    /// Retains execution owners until the shared pool is shut down.
    pub fn add(&mut self, workers: FormulaWorkers) {
        self.workers.push(workers);
    }
}

impl crate::FormulaEngine for FormulaPool {
    /// Names the dispatcher; individual outputs retain their actual consumer identity.
    fn name(&self) -> &str {
        "formula-pool"
    }
    /// Bounds cropped pixels across every group and document using this pool.
    fn admission(&self) -> Option<Arc<tokio::sync::Semaphore>> {
        Some(Arc::clone(&self.admission))
    }
    /// Retains the text-only interface for callers that do not need consumer provenance.
    fn recognize(
        &self,
        images: Vec<Arc<PageImage>>,
        timings: Timings,
    ) -> docparse_common::WasmBoxedFuture<'_, Result<Vec<String>, FormulaError>>
    {
        Box::pin(async move {
            Ok(self
                .recognize_named(images, timings)
                .await?
                .into_iter()
                .map(|output| output.latex)
                .collect())
        })
    }
    /// Returns the engine identity recorded by the consumer that won each crop.
    fn recognize_named(
        &self,
        images: Vec<Arc<PageImage>>,
        timings: Timings,
    ) -> docparse_common::WasmBoxedFuture<
        '_,
        Result<Vec<FormulaOutput>, FormulaError>,
    > {
        Box::pin(async move {
            docparse_common::timeout(
                self.timeout,
                self.queue.run_named(images, timings),
            )
            .await
            .unwrap_or_else(|_| {
                Err(FormulaError::Invalid(format!(
                    "formula request timed out after {} ms",
                    self.timeout.as_millis()
                )))
            })
        })
    }
}
