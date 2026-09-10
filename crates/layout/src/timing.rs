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
    TextFinish,
    TableStructure,
    /// Local table topology and source validation.
    TableRules,
    /// Queue and external structure-provider time.
    TableExternal,
    /// External topology binding and final source validation.
    TableFill,
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
