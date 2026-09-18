//! Bounded per-crop admission and ready-only batching shared by formula engines.
use crate::FormulaError;
use docparse_layout::{
    PageImage,
    timing::{StageTimer, TimingStage, Timings},
};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use typed_builder::TypedBuilder;

/// One sender represents the same queue across every page and document using an engine.
#[derive(Clone)]
pub struct FormulaQueue {
    sender: mpsc::Sender<FormulaRequest>,
}

impl FormulaQueue {
    /// Creates a bounded crop queue; capacity must be positive, as with Tokio channels.
    pub fn new(capacity: usize) -> (Self, mpsc::Receiver<FormulaRequest>) {
        let (sender, receiver) = mpsc::channel(capacity);
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
                .timings(timings.clone())
                .span(tracing::Span::current())
                .dispatch(tracing::dispatcher::get_default(Clone::clone))
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
    pub image: Arc<PageImage>,
    pub timings: Timings,
    response: oneshot::Sender<Result<String, FormulaError>>,
    #[builder(default)]
    queued: Option<StageTimer>,
    span: tracing::Span,
    dispatch: tracing::Dispatch,
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
        tracing::dispatcher::with_default(&self.dispatch, || {
            self.span.in_scope(|| drop(timer))
        });
    }

    /// Drains only already-ready crops; canceled entries never use a model batch slot.
    pub fn ready(
        first: Self,
        receiver: &mut mpsc::Receiver<Self>,
        limit: usize,
    ) -> Vec<Self> {
        let mut batch = Vec::with_capacity(limit);
        let mut next = Some(first);
        while let Some(mut request) = next {
            if request.cancelled() {
                request.end_queue();
            } else {
                batch.push(request);
            }
            if batch.len() == limit {
                break;
            }
            next = receiver.try_recv().ok();
        }
        batch
    }

    /// Completes one crop without letting a canceled caller affect its neighbors.
    pub fn complete(mut self, result: Result<String, FormulaError>) {
        self.end_queue();
        tracing::dispatcher::with_default(&self.dispatch, || {
            self.span.in_scope(|| {
                if !self.response.is_closed()
                    && let Err(error) = &result
                {
                    tracing::warn!(
                        "queued formula recognition failed: {}",
                        error
                    );
                }
                let _ = self.response.send(result);
            })
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

/// Copies only attribution metadata into blocking model operations, never response ownership.
pub struct BatchTimings(Vec<(Timings, tracing::Span, tracing::Dispatch)>);

impl From<&[FormulaRequest]> for BatchTimings {
    /// Preserves each crop's page, observer, and trace when a model batch spans callers.
    fn from(requests: &[FormulaRequest]) -> Self {
        Self(
            requests
                .iter()
                .map(|request| {
                    (
                        request.timings.clone(),
                        request.span.clone(),
                        request.dispatch.clone(),
                    )
                })
                .collect(),
        )
    }
}

impl BatchTimings {
    /// Starts the same physical stage for every participating crop's observer.
    pub fn start(&self, stage: TimingStage) -> BatchTimer {
        BatchTimer(
            self.0
                .iter()
                .map(|(timings, span, dispatch)| {
                    (timings.start(stage), span.clone(), dispatch.clone())
                })
                .collect(),
        )
    }
}

/// Stage guards restore original tracing contexts before recording their elapsed intervals.
pub struct BatchTimer(Vec<(StageTimer, tracing::Span, tracing::Dispatch)>);

impl Drop for BatchTimer {
    /// Emits timing records even when a model fails or an operation is canceled.
    fn drop(&mut self) {
        for (timer, span, dispatch) in self.0.drain(..) {
            tracing::dispatcher::with_default(&dispatch, || {
                span.in_scope(|| drop(timer))
            });
        }
    }
}
