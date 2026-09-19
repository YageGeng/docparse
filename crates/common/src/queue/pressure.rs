//! Queue-owned hysteresis with cancellable deadlines independent of producer activity.
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::{Notify, oneshot};
use web_time::Instant;

#[derive(Clone, Copy)]
struct Policy {
    watermarks: (f64, f64),
    delays: (Duration, Duration),
}
#[derive(Default)]
struct State {
    items: usize,
    paused: bool,
    since: Option<Instant>,
}

/// Cancellation precedes joining the owner, so shutdown cannot wait for a long deadline.
struct DeadlineOwner {
    _stop: oneshot::Sender<()>,
    _worker: crate::ThreadManager,
}
struct Controller {
    policy: Policy,
    changed: Arc<Notify>,
    _deadline: DeadlineOwner,
}

/// Queue-owned pressure state never mixes independent engines or depends on telemetry sampling.
pub struct QueuePressure {
    identity: (&'static str, usize),
    controller: OnceLock<Controller>,
    state: Arc<Mutex<State>>,
}
impl QueuePressure {
    /// Starts disabled without a timer or per-item locking cost on ordinary queues.
    pub(crate) fn new(name: &'static str, capacity: usize) -> Self {
        Self {
            identity: (name, capacity),
            controller: OnceLock::new(),
            state: Arc::new(Mutex::new(State::default())),
        }
    }
    /// Enables deadline ownership before callers receive the empty queue; startup failures remain explicit.
    pub fn configure(
        &self,
        high: f64,
        low: f64,
        pause: Duration,
        resume: Duration,
    ) -> Result<(), crate::TaskError> {
        if self.controller.get().is_some() {
            return Ok(());
        }
        // Serialize initialization before starting a task that can update shared state.
        let _initializing = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.controller.get().is_some() {
            return Ok(());
        }
        let policy = Policy {
            watermarks: (high, low),
            delays: (pause, resume),
        };
        let changed = Arc::new(Notify::new());
        let deadline = DeadlineOwner::start(
            self.identity,
            policy,
            Arc::clone(&self.state),
            Arc::clone(&changed),
        )?;
        // The initialization lock guarantees that only this owner can publish the controller.
        let _ = self.controller.set(Controller {
            policy,
            changed,
            _deadline: deadline,
        });
        metrics::gauge!("docparse_formula_inline_paused", "queue" => self.identity.0).increment(0.0);
        Ok(())
    }
    /// Records every accepted/removal event, including canceled and discarded queued items.
    pub(crate) fn change(&self, added: bool) {
        if self.controller.get().is_none() {
            return;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        self.evaluate(&mut state, now);
        if added {
            state.items += 1;
        } else {
            state.items = state.items.saturating_sub(1);
        }
        self.evaluate(&mut state, now);
    }
    /// Rechecks elapsed time at page admission in case CPU load delayed the deadline callback.
    pub fn paused(&self) -> bool {
        if self.controller.get().is_none() {
            return false;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.evaluate(&mut state, Instant::now());
        state.paused
    }
    /// Counts only inline regions actually omitted by a page's degradation decision.
    pub fn record_skipped(&self, count: usize) {
        metrics::counter!("docparse_formula_inline_skipped_total", "queue" => self.identity.0).increment(count as u64);
    }
    /// Wakes the deadline owner only when its qualifying interval starts, breaks, or changes phase.
    fn evaluate(&self, state: &mut State, now: Instant) {
        let Some(controller) = self.controller.get() else {
            return;
        };
        let before = (state.paused, state.since);
        state.evaluate(self.identity, controller.policy, now);
        if before != (state.paused, state.since) {
            controller.changed.notify_one();
        }
    }
}
impl State {
    /// Applies strict watermarks and emits exactly one metric/log transition per completed interval.
    fn evaluate(
        &mut self,
        identity: (&'static str, usize),
        policy: Policy,
        now: Instant,
    ) {
        let ratio = self.items as f64 / identity.1 as f64;
        let (condition, delay) = if self.paused {
            (ratio < policy.watermarks.1, policy.delays.1)
        } else {
            (ratio > policy.watermarks.0, policy.delays.0)
        };
        if !condition {
            self.since = None;
            return;
        }
        let since = *self.since.get_or_insert(now);
        if now.duration_since(since) >= delay {
            self.paused = !self.paused;
            self.since = None;
            metrics::gauge!("docparse_formula_inline_paused", "queue" => identity.0).increment(if self.paused {1.0} else {-1.0});
            let action = if self.paused { "pause" } else { "resume" };
            metrics::counter!("docparse_formula_inline_transitions_total", "queue" => identity.0, "action" => action).increment(1);
            tracing::info!(
                "formula inline admission {} for queue {} at occupancy {:.3}",
                action,
                identity.0,
                ratio
            );
        }
    }
}
impl DeadlineOwner {
    /// Waits for deadline or interval changes without polling, retaining state but never the queue owner.
    fn start(
        identity: (&'static str, usize),
        policy: Policy,
        state: Arc<Mutex<State>>,
        changed: Arc<Notify>,
    ) -> Result<Self, crate::TaskError> {
        let (stop, mut stopped) = oneshot::channel();
        let worker = crate::ThreadManager::spawn_async(Box::pin(async move {
            loop {
                // Browser tasks may first be polled after their queue was already dropped.
                match stopped.try_recv() {
                    Ok(()) | Err(oneshot::error::TryRecvError::Closed) => break,
                    Err(oneshot::error::TryRecvError::Empty) => {}
                }
                let remaining = {
                    let mut state = state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state.evaluate(identity, policy, Instant::now());
                    state.since.map(|since| {
                        let delay = if state.paused {
                            policy.delays.1
                        } else {
                            policy.delays.0
                        };
                        // Bound JS timer arguments and native Instant arithmetic without shortening the configured delay.
                        delay
                            .saturating_sub(since.elapsed())
                            .min(Duration::from_secs(86400))
                    })
                };
                tokio::select! {
                    biased;
                    _ = &mut stopped => break,
                    _ = async {
                        match remaining {
                            Some(duration) => { let _ = crate::timeout(duration,changed.notified()).await; }
                            None => changed.notified().await,
                        }
                    } => {}
                }
            }
        }))?;
        Ok(Self {
            _stop: stop,
            _worker: worker,
        })
    }
}
impl Drop for QueuePressure {
    /// Stops timers before removing this engine's gauge contribution, preventing late resurrection.
    fn drop(&mut self) {
        let enabled = self.controller.get().is_some();
        // Join the native timer without holding the state mutex it may still need.
        drop(self.controller.take());
        if enabled
            && self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .paused
        {
            metrics::gauge!("docparse_formula_inline_paused", "queue" => self.identity.0).decrement(1.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Real queue removal and channel discard both release occupancy without a metrics recorder.
    #[tokio::test]
    async fn queue_events_track_pending_items_and_discard() {
        struct Request;
        impl crate::SessionRequest for Request {
            /// The pressure stimulus remains a live pending request.
            fn cancelled(&self) -> bool {
                false
            }
            /// This stimulus carries no request timing scope.
            fn end_queue(&mut self) {}
        }
        let (sender, receiver) = crate::Queue::new("pressure_test", 1);
        let pressure = sender.pressure();
        pressure
            .configure(0.85, 0.5, Duration::ZERO, Duration::ZERO)
            .expect("pressure timer");
        sender
            .send(Request)
            .await
            .expect("enqueue pressure stimulus");
        assert!(pressure.paused());
        assert_eq!(
            receiver.recv().await.expect("queued").take_ready(1).len(),
            1
        );
        assert!(!pressure.paused());
        sender
            .send(Request)
            .await
            .expect("enqueue pressure stimulus");
        assert!(pressure.paused());
        drop(receiver);
        assert!(!pressure.paused());
    }

    /// Boundary equality and short excursions reset timers; recovery requires its own uninterrupted interval.
    #[test]
    fn sustained_thresholds_and_hysteresis() {
        let pressure = QueuePressure::new("test", 100);
        pressure
            .configure(
                0.85,
                0.5,
                Duration::from_secs(30),
                Duration::from_secs(30),
            )
            .expect("pressure timer");
        let start = Instant::now();
        let mut state = State {
            items: 86,
            ..State::default()
        };
        pressure.evaluate(&mut state, start);
        pressure.evaluate(&mut state, start + Duration::from_secs(29));
        assert!(!state.paused);
        state.items = 85;
        pressure.evaluate(&mut state, start + Duration::from_secs(29));
        state.items = 86;
        pressure.evaluate(&mut state, start + Duration::from_secs(30));
        pressure.evaluate(&mut state, start + Duration::from_secs(59));
        assert!(!state.paused);
        pressure.evaluate(&mut state, start + Duration::from_secs(60));
        assert!(state.paused);
        state.items = 49;
        pressure.evaluate(&mut state, start + Duration::from_secs(61));
        state.items = 50;
        pressure.evaluate(&mut state, start + Duration::from_secs(80));
        state.items = 0;
        pressure.evaluate(&mut state, start + Duration::from_secs(81));
        pressure.evaluate(&mut state, start + Duration::from_secs(110));
        assert!(state.paused);
        pressure.evaluate(&mut state, start + Duration::from_secs(111));
        assert!(!state.paused);
        assert!(!QueuePressure::new("disabled", 1).paused());
    }
}
