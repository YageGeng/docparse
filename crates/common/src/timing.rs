//! Optional stage observations, separate from deterministic document and detection data.
use serde::Serialize;
use tokio::sync::mpsc::{
    UnboundedReceiver, UnboundedSender, unbounded_channel,
};
use web_time::Instant;

/// Stable names for elapsed intervals; totals include their nested stages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TimingStage {
    FormulaQueue,
    FormulaPreprocess,
    FormulaInference,
    FormulaDecode,
    /// Waiting for an available PDFium process lease.
    PdfiumQueue,
    PdfOpen,
    TextExtract,
    DocumentContext,
    PdfRender,
    LayoutPreprocess,
    LayoutQueue,
    LayoutInference,
    LayoutReadback,
    LayoutPostprocess,
    TextPrepare,
    Ocr,
    /// PaddleOCR image tensor preparation and DB box decoding.
    OcrDetectionPreprocess,
    OcrDetectionInference,
    OcrDetectionPostprocess,
    /// Waiting for an OCR model session.
    OcrQueue,
    OcrOrientationInference,
    OcrRecognitionPreprocess,
    OcrRecognitionInference,
    /// Output synchronization/copy and CPU probability validation/argmax, separate from the runtime call.
    OcrReadback,
    OcrDecode,
    TextFinish,
    TableStructure,
    /// Local table topology and source validation.
    TableRules,
    /// Queue and external structure-provider time.
    TableExternal,
    /// External topology binding and final source validation.
    TableFill,
    /// SLANet_plus crop preparation, independent of layout preprocessing.
    TsrPreprocess,
    /// Waiting for the single owned TSR session.
    TsrQueue,
    /// Real ONNX table model execution.
    TsrInference,
    /// Table model token and location decoding.
    TsrPostprocess,
    /// Preparing the independent table cell detector image and scale tensors.
    TableCellPreprocess,
    /// Real RT-DETR table cell inference, excluding structure prediction.
    TableCellInference,
    /// Validating and filtering detections before core topology matching.
    TableCellPostprocess,
    LinkValidate,
    ParseTotal,
    ResultSerialize,
}

/// An attempted stage's wall time, including waits inside that stage, even on failure.
#[derive(Debug, Clone, Serialize)]
pub struct Timing {
    pub stage: TimingStage,
    pub page_number: Option<u32>,
    pub duration_ms: f64,
}

/// Clonable observation context; without a receiver, only debug logs are emitted.
#[derive(Debug, Clone, Default)]
pub struct Timings {
    sender: Option<UnboundedSender<Timing>>,
    page_number: Option<u32>,
}

impl Timings {
    /// Identifies one page observer so a shared physical batch is not counted once per crop.
    pub fn same_observer(&self, other: &Self) -> bool {
        self.page_number == other.page_number
            && match (&self.sender, &other.sender) {
                (Some(left), Some(right)) => left.same_channel(right),
                (None, None) => true,
                _ => false,
            }
    }

    /// Collects small records for serial delivery by the owning parse future.
    pub fn channel() -> (Self, UnboundedReceiver<Timing>) {
        let (sender, receiver) = unbounded_channel();
        (
            Self {
                sender: Some(sender),
                page_number: None,
            },
            receiver,
        )
    }

    /// Shares the sink while preserving attribution across concurrent page tasks.
    pub fn for_page(&self, page_number: u32) -> Self {
        Self {
            sender: self.sender.clone(),
            page_number: Some(page_number),
        }
    }

    /// Starts a monotonic interval on native threads or the browser Worker's performance clock.
    pub fn start(&self, stage: TimingStage) -> StageTimer {
        StageTimer {
            timings: self.clone(),
            stage,
            started: Instant::now(),
        }
    }
}

/// Records exactly once on scope exit; it never calls user code from a worker task.
#[must_use = "keep the timer alive until the measured stage ends"]
pub struct StageTimer {
    timings: Timings,
    stage: TimingStage,
    started: Instant,
}

impl Drop for StageTimer {
    /// Preserves observations on early returns and tolerates a cancelled receiver.
    fn drop(&mut self) {
        let timing = Timing {
            stage: self.stage,
            page_number: self.timings.page_number,
            duration_ms: self.started.elapsed().as_secs_f64() * 1000.0,
        };
        tracing::debug!(
            "stage {:?} for page {:?} elapsed {:.3} ms",
            timing.stage,
            timing.page_number,
            timing.duration_ms
        );
        if let Some(sender) = &self.timings.sender {
            let _ = sender.send(timing);
        }
    }
}

/// Original observer and tracing subscriber retained when requests cross runtime or thread boundaries.
#[derive(Clone)]
pub struct TimingContext {
    pub timings: Timings,
    span: tracing::Span,
    dispatch: tracing::Dispatch,
}

impl TimingContext {
    /// Captures the submitting caller rather than the eventual shared consumer's tracing context.
    pub fn new(timings: Timings) -> Self {
        Self {
            timings,
            span: tracing::Span::current(),
            dispatch: tracing::dispatcher::get_default(Clone::clone),
        }
    }

    /// Restores attribution around completion, cancellation, or a queued timer's destruction.
    pub fn in_scope<T>(&self, operation: impl FnOnce() -> T) -> T {
        tracing::dispatcher::with_default(&self.dispatch, || {
            self.span.in_scope(operation)
        })
    }

    /// Starts a guard that restores this context even during error returns or unwinding.
    pub fn start(&self, stage: TimingStage) -> ContextTimer {
        ContextTimer {
            timer: Some(self.timings.start(stage)),
            context: self.clone(),
        }
    }
}

/// A stage guard owns its original attribution independently of request and model lifetimes.
#[must_use = "keep the timer alive until the measured stage ends"]
pub struct ContextTimer {
    timer: Option<StageTimer>,
    context: TimingContext,
}

impl Drop for ContextTimer {
    /// Records the interval under its original caller, including early model failures.
    fn drop(&mut self) {
        let timer = self.timer.take();
        self.context.in_scope(|| drop(timer));
    }
}

/// Batch observers contain attribution metadata only, never inputs or response ownership.
pub struct BatchTimings(Vec<TimingContext>);

impl<'a> FromIterator<&'a TimingContext> for BatchTimings {
    /// Copies each participating request's observer into a reusable physical-batch context.
    fn from_iter<T: IntoIterator<Item = &'a TimingContext>>(
        contexts: T,
    ) -> Self {
        Self(contexts.into_iter().cloned().collect())
    }
}

impl BatchTimings {
    /// Measures the same physical stage for every participating request.
    pub fn start(&self, stage: TimingStage) -> Vec<ContextTimer> {
        self.0.iter().map(|context| context.start(stage)).collect()
    }

    /// Counts one physical invocation per page observer, rather than once per line from that page.
    pub fn start_unique(&self, stage: TimingStage) -> Vec<ContextTimer> {
        // Batches contain at most 32 requests, so a bounded scan avoids an artificial observer-key type.
        self.0
            .iter()
            .enumerate()
            .filter(|(index, context)| {
                !self
                    .0
                    .iter()
                    .take(*index)
                    .any(|prior| prior.timings.same_observer(&context.timings))
            })
            .map(|(_, context)| context.start(stage))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Observer de-duplication must not merge equal page numbers from different documents.
    #[test]
    fn batch_timing_deduplicates_only_the_same_page_observer() {
        let (first, mut first_events) = Timings::channel();
        let (second, mut second_events) = Timings::channel();
        let contexts = [
            TimingContext::new(first.for_page(1)),
            TimingContext::new(first.for_page(2)),
            TimingContext::new(first.for_page(1)),
            TimingContext::new(second.for_page(1)),
        ];
        let batch: BatchTimings = contexts.iter().collect();
        drop(batch.start_unique(TimingStage::OcrRecognitionInference));
        assert_eq!(
            first_events.try_recv().expect("page one").page_number,
            Some(1)
        );
        assert_eq!(
            first_events.try_recv().expect("page two").page_number,
            Some(2)
        );
        // An empty live observer proves de-duplication; a disconnected channel would not.
        assert_eq!(
            first_events
                .try_recv()
                .expect_err("no duplicate page timer"),
            tokio::sync::mpsc::error::TryRecvError::Empty
        );
        assert_eq!(
            second_events
                .try_recv()
                .expect("other document")
                .page_number,
            Some(1)
        );
        assert_eq!(
            second_events
                .try_recv()
                .expect_err("no duplicate document timer"),
            tokio::sync::mpsc::error::TryRecvError::Empty
        );
    }
}
