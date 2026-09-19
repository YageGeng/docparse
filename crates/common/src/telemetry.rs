//! Small recorder-independent lifecycle guards; without a recorder all measurements are no-ops.
use std::sync::Arc;
use web_time::Instant;

/// A scope-owned gauge contribution, including work that outlives its original caller.
#[derive(Debug)]
pub struct Activity {
    metric: &'static str,
    label: (&'static str, &'static str),
    amount: f64,
}
impl Activity {
    /// Adds one owner's contribution and removes precisely that contribution on final drop.
    pub fn new(
        metric: &'static str,
        label: (&'static str, &'static str),
        amount: f64,
    ) -> Self {
        metrics::gauge!(metric, label.0 => label.1).increment(amount);
        Self {
            metric,
            label,
            amount,
        }
    }
}
impl Drop for Activity {
    /// Releases occupancy on success, error, cancellation, and unwinding.
    fn drop(&mut self) {
        metrics::gauge!(self.metric, self.label.0 => self.label.1)
            .decrement(self.amount);
    }
}

/// A histogram observation belongs to the actual operation scope, not its observers.
pub struct Timer {
    histogram: metrics::Histogram,
    started: Instant,
}
impl Timer {
    /// Starts an operation using a fixed metric and bounded label value.
    pub fn new(
        metric: &'static str,
        key: &'static str,
        value: &'static str,
    ) -> Self {
        Self {
            histogram: metrics::histogram!(metric, key => value),
            started: Instant::now(),
        }
    }
}
impl Drop for Timer {
    /// Records seconds even if the operation exits through an error or cancellation.
    fn drop(&mut self) {
        self.histogram.record(self.started.elapsed().as_secs_f64());
    }
}

/// Shared model-consumer measurements keep configuration and owner lifetimes consistent across executors.
pub struct ModelMetrics {
    name: &'static str,
    _configured: Activity,
    _batch_limit: Activity,
}
impl ModelMetrics {
    /// Registers one pool's configured consumers and batch cap until its final owner exits.
    pub fn new(
        name: &'static str,
        consumers: usize,
        batch_limit: usize,
    ) -> Arc<Self> {
        metrics::gauge!("docparse_model_workers_busy", "model" => name)
            .increment(0.0);
        metrics::gauge!("docparse_model_workers_alive", "model" => name)
            .increment(0.0);
        Arc::new(Self {
            name,
            _configured: Activity::new(
                "docparse_model_workers_configured",
                ("model", name),
                consumers as f64,
            ),
            _batch_limit: Activity::new(
                "docparse_model_batch_limit",
                ("model", name),
                batch_limit as f64,
            ),
        })
    }
    /// Accounts for initialized execution slots while their owner retains this pool.
    pub fn alive(&self, consumers: usize) -> Activity {
        Activity::new(
            "docparse_model_workers_alive",
            ("model", self.name),
            consumers as f64,
        )
    }
    /// Measures one consumer's occupied scope independently of physical ONNX calls.
    pub fn batch(&self) -> (Activity, Timer) {
        (
            Activity::new(
                "docparse_model_workers_busy",
                ("model", self.name),
                1.0,
            ),
            Timer::new(
                "docparse_model_batch_service_seconds",
                "model",
                self.name,
            ),
        )
    }
}

/// Admission owns its clock through cancellation, before a queue or page slot is acquired.
pub struct Admission {
    identity: (&'static str, &'static str),
    started: Instant,
    outcome: &'static str,
}
impl Admission {
    /// Defaults to cancellation so dropping a pending future still reports its real wait.
    pub fn new(metric: &'static str, queue: &'static str) -> Self {
        Self {
            identity: (metric, queue),
            started: Instant::now(),
            outcome: "cancelled",
        }
    }
    /// Completes a successful or closed admission exactly once.
    pub fn finish(mut self, outcome: &'static str) {
        self.outcome = outcome;
    }
}
impl Drop for Admission {
    /// Observes pending waits on every exit, independently of blocked-producer occupancy.
    fn drop(&mut self) {
        metrics::histogram!(self.identity.0, "queue" => self.identity.1, "outcome" => self.outcome).record(self.started.elapsed().as_secs_f64());
    }
}

/// One physical ORT call, separate from crop/page attribution and repeated decoder steps.
pub struct Inference {
    identity: (&'static str, &'static str),
    started: Instant,
    outcome: &'static str,
}
impl Inference {
    /// Counts the actual tensor batch once at the real runtime invocation boundary.
    pub fn new(model: &'static str, graph: &'static str, batch: usize) -> Self {
        metrics::gauge!("docparse_onnx_active_calls", "model" => model, "graph" => graph).increment(1.0);
        metrics::histogram!("docparse_onnx_batch_items", "model" => model, "graph" => graph).record(batch as f64);
        Self {
            identity: (model, graph),
            started: Instant::now(),
            outcome: "cancelled",
        }
    }
    /// Marks the call's outcome; consuming the guard emits exactly one observation.
    pub fn finish(mut self, success: bool) {
        self.outcome = if success { "success" } else { "error" };
    }
}
impl Drop for Inference {
    /// Keeps active calls occupied until the real runtime returns, including abandoned caller work.
    fn drop(&mut self) {
        let (model, graph) = self.identity;
        metrics::gauge!("docparse_onnx_active_calls", "model" => model, "graph" => graph).decrement(1.0);
        metrics::histogram!("docparse_onnx_run_seconds", "model" => model, "graph" => graph, "outcome" => self.outcome).record(self.started.elapsed().as_secs_f64());
    }
}

/// Shared queue metadata has one lifetime regardless of producer and receiver clones.
#[derive(typed_builder::TypedBuilder)]
pub(crate) struct QueueMetrics {
    pub name: &'static str,
    pub capacity: usize,
    pub pressure: Arc<crate::queue::QueuePressure>,
    _capacity: Activity,
}
impl QueueMetrics {
    /// Registers an explicitly bounded queue without introducing labels derived from request data.
    pub fn new(name: &'static str, capacity: usize) -> Arc<Self> {
        metrics::gauge!("docparse_queue_items", "queue" => name).increment(0.0);
        metrics::gauge!("docparse_queue_blocked_producers", "queue" => name)
            .increment(0.0);
        Arc::new(
            Self::builder()
                .name(name)
                .capacity(capacity)
                .pressure(Arc::new(crate::queue::QueuePressure::new(
                    name, capacity,
                )))
                ._capacity(Activity::new(
                    "docparse_queue_capacity_items",
                    ("queue", name),
                    capacity as f64,
                ))
                .build(),
        )
    }
    /// Tracks capacity waits even when a caller packet cannot fit in a partially full queue.
    pub fn blocked(&self) -> Activity {
        Activity::new(
            "docparse_queue_blocked_producers",
            ("queue", self.name),
            1.0,
        )
    }
}

/// A queued item owns its occupancy until removal, including channel shutdown and discarded work.
pub(crate) struct Queued<R> {
    value: Option<R>,
    queued: Instant,
    metrics: Arc<QueueMetrics>,
}
impl<R> Queued<R> {
    /// Records admission at the point where the queue actually takes ownership.
    pub fn new(value: R, metrics: &Arc<QueueMetrics>) -> Self {
        metrics.pressure.change(true);
        metrics::gauge!("docparse_queue_items", "queue" => metrics.name)
            .increment(1.0);
        metrics::counter!("docparse_queue_enqueued_items_total", "queue" => metrics.name).increment(1);
        Self {
            value: Some(value),
            queued: Instant::now(),
            metrics: Arc::clone(metrics),
        }
    }
    /// Removes an item with an explicit outcome, including native queue shutdown.
    pub fn take(mut self, outcome: &'static str) -> R {
        self.removed(outcome);
        self.value.take().expect("queued value is consumed once")
    }
    /// Shares one release path for dequeued and dropped channel entries.
    fn removed(&self, outcome: &'static str) {
        let name = self.metrics.name;
        self.metrics.pressure.change(false);
        metrics::gauge!("docparse_queue_items", "queue" => name).decrement(1.0);
        metrics::counter!("docparse_queue_removed_items_total", "queue" => name, "outcome" => outcome).increment(1);
        metrics::histogram!("docparse_queue_residence_seconds", "queue" => name, "outcome" => outcome).record(self.queued.elapsed().as_secs_f64());
    }
}

impl<R> Drop for Queued<R> {
    /// Channels can discard pending entries without invoking a consumer.
    fn drop(&mut self) {
        if self.value.is_some() {
            self.removed("discarded");
        }
    }
}
