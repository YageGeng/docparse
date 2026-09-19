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

/// Connects the formula policy to the actual queue before producer access begins.
pub fn configure_backpressure(
    pressure: &docparse_common::queue::QueuePressure,
    config: &docparse_config::FormulaConfig,
) -> Result<(), docparse_common::TaskError> {
    let policy = &config.backpressure;
    if policy.enabled && config.inline_enabled {
        pressure.configure(
            policy.high_watermark,
            policy.low_watermark,
            std::time::Duration::from_secs(policy.pause_after_secs),
            std::time::Duration::from_secs(policy.resume_after_secs),
        )?;
    }
    Ok(())
}

impl FormulaQueue {
    /// Shares pressure tracking across every caller of this engine.
    pub fn pressure(&self) -> Arc<docparse_common::queue::QueuePressure> {
        self.sender.pressure()
    }

    /// Creates a bounded crop queue; capacity must be positive, as with Tokio channels.
    pub fn new(
        name: &'static str,
        capacity: usize,
    ) -> (Self, Queue<FormulaRequest>) {
        let (sender, receiver) = Queue::new(name, capacity);
        (Self { sender }, receiver)
    }

    /// Submits independent crops with backpressure and reconstructs caller order after completion.
    pub async fn run(
        &self,
        images: Vec<Arc<PageImage>>,
        timings: Timings,
    ) -> Result<Vec<String>, FormulaError> {
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
                self.sender.send(request).await.map_err(|_closed| {
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
    // Keep the render delivery occupied until actual inference and input cleanup finish.
    #[builder(default = docparse_common::PageLease::current())]
    _page_lease: Option<docparse_common::PageLease>,
    pub image: Arc<PageImage>,
    pub context: TimingContext,
    response: oneshot::Sender<Result<String, FormulaError>>,
    #[builder(default)]
    queued: Option<StageTimer>,
}

impl FormulaRequest {
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
            let _ = self.response.send(result);
        });
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
