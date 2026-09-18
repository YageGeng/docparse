//! Shared native bounded admission and ready-only draining.
use super::SessionRequest;
use crate::{
    TaskError,
    telemetry::{Admission, QueueMetrics, Queued},
};
use std::sync::Arc;
use std::{
    collections::VecDeque,
    sync::{Condvar, Mutex},
};

/// Queue state remains independent of session initialization and execution.
struct QueueState<R> {
    requests: VecDeque<Queued<R>>,
    closed: bool,
}

/// One crop-counted queue is shared by all consumers of a model.
pub struct BlockingQueue<R> {
    state: Mutex<QueueState<R>>,
    changed: Condvar,
    metrics: Arc<QueueMetrics>,
}

impl<R: SessionRequest> BlockingQueue<R> {
    /// Creates a bounded crop queue independently of consumer initialization.
    pub fn new(name: &'static str, capacity: usize) -> Self {
        assert!(capacity > 0, "queue capacity must be positive");
        Self {
            state: Mutex::new(QueueState {
                requests: VecDeque::new(),
                closed: false,
            }),
            changed: Condvar::new(),
            metrics: QueueMetrics::new(name, capacity),
        }
    }

    /// Publishes one request through the same admission path as atomic caller packets.
    pub fn push(&self, request: R) -> Result<(), TaskError> {
        self.push_batch(vec![request])
    }

    /// Admits a caller packet atomically while bounding every unconsumed crop.
    pub fn push_batch(&self, mut requests: Vec<R>) -> Result<(), TaskError> {
        if requests.is_empty() || requests.len() > self.metrics.capacity {
            return Err(TaskError::from_message("invalid queue packet size"));
        }
        let admission = Admission::new(
            "docparse_queue_admission_wait_seconds",
            self.metrics.name,
        );
        let mut state = self
            .state
            .lock()
            .map_err(|error| TaskError::from_message(error.to_string()))?;
        let blocked = (state.requests.len() + requests.len()
            > self.metrics.capacity)
            .then(|| self.metrics.blocked());
        while !state.closed
            && state.requests.len() + requests.len() > self.metrics.capacity
            && !requests.iter().all(SessionRequest::cancelled)
        {
            state = self
                .changed
                .wait(state)
                .map_err(|error| TaskError::from_message(error.to_string()))?;
        }
        drop(blocked);
        if state.closed || requests.iter().all(SessionRequest::cancelled) {
            admission.finish(if state.closed { "closed" } else { "cancelled" });
            let closed = state.closed;
            drop(state);
            for request in &mut requests {
                request.end_queue();
            }
            return if closed {
                Err(TaskError::from_message("model queue closed"))
            } else {
                Ok(())
            };
        }
        admission.finish("admitted");
        state.requests.extend(
            requests
                .into_iter()
                .map(|request| Queued::new(request, &self.metrics)),
        );
        self.changed.notify_all();
        Ok(())
    }

    /// Waits for the first live request and drains only already-ready work up to the limit.
    pub fn pop(&self, limit: usize) -> Option<Vec<R>> {
        assert!(limit > 0, "batch limit must be positive");
        loop {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            while state.requests.is_empty() && !state.closed {
                state = self
                    .changed
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            if state.closed {
                return None;
            }
            let mut requests = Vec::with_capacity(limit);
            let mut canceled = Vec::new();
            while requests.len() < limit {
                let Some(request) = state.requests.pop_front() else {
                    break;
                };
                let request = request.take("dequeued");
                if request.cancelled() {
                    canceled.push(request);
                } else {
                    requests.push(request);
                }
            }
            self.changed.notify_all();
            drop(state);
            for request in requests.iter_mut().chain(&mut canceled) {
                request.end_queue();
            }
            if !requests.is_empty() {
                return Some(requests);
            }
        }
    }

    /// Wakes producers and consumers and releases queued replies after failure or shutdown.
    pub fn close(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.closed = true;
        let discarded = std::mem::take(&mut state.requests);
        self.changed.notify_all();
        drop(state);
        for request in discarded {
            let mut request = request.take("discarded");
            request.end_queue();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Queue behavior is tested without depending on any model's image, tensor, or error types.
    struct Request(usize, bool);
    impl SessionRequest for Request {
        /// Marks canceled work independently of the observer and payload.
        fn cancelled(&self) -> bool {
            self.1
        }
        /// No observer exists in this queue-only test.
        fn end_queue(&mut self) {}
    }

    /// Caller packet boundaries cannot leave ready capacity unused or lose an unconsumed tail.
    #[test]
    fn ready_packets_fill_batches_and_preserve_capacity() {
        let queue = BlockingQueue::new("test", 19);
        let mut next = 0;
        for size in [2, 8, 3, 6] {
            queue
                .push_batch(
                    (next..next + size)
                        .map(|value| Request(value, false))
                        .collect(),
                )
                .expect("packet");
            next += size;
        }
        let mut received = Vec::new();
        for (size, remaining) in [(8, 11), (8, 3), (3, 0)] {
            let requests = queue.pop(8).expect("ready batch");
            assert_eq!(requests.len(), size);
            assert_eq!(
                queue.state.lock().expect("queue state").requests.len(),
                remaining
            );
            received.extend(requests.into_iter().map(|request| request.0));
        }
        assert_eq!(received, (0..19).collect::<Vec<_>>());
        queue
            .push_batch(vec![Request(19, true), Request(20, false)])
            .expect("canceled peer");
        assert_eq!(queue.pop(8).expect("short live batch").len(), 1);
        queue.close();
        assert!(queue.pop(8).is_none());
        assert!(queue.push(Request(21, false)).is_err());
    }
}
