//! Shared native bounded admission and ready-only draining.
use super::SessionRequest;
use crate::{
    TaskError,
    telemetry::{Admission, QueueMetrics, Queued},
};
use std::{
    collections::VecDeque,
    future::Future,
    sync::{Arc, Condvar, Mutex},
    task::{Context, Poll, Wake},
    thread::Thread,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Queued inputs retain FIFO admission until a consumer removes them.
struct QueueState<R> {
    requests: VecDeque<(Queued<R>, OwnedSemaphorePermit)>,
    closed: bool,
}

/// One crop-counted queue is shared by all consumers of a model.
#[derive(typed_builder::TypedBuilder)]
pub struct BlockingQueue<R> {
    state: Mutex<QueueState<R>>,
    changed: Condvar,
    capacity: Arc<Semaphore>,
    metrics: Arc<QueueMetrics>,
}

/// Synchronous callers join the same FIFO semaphore without creating a runtime or polling timer.
struct ProducerThread(Thread);
impl Wake for ProducerThread {
    /// A park token preserves notifications arriving between polling and parking.
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
    /// Wakes the registered producer without an additional reference-count operation.
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

impl<R: SessionRequest> BlockingQueue<R> {
    /// Creates a bounded crop queue independently of consumer initialization.
    pub fn new(name: &'static str, capacity: usize) -> Self {
        assert!(capacity > 0, "queue capacity must be positive");
        Self::builder()
            .state(Mutex::new(QueueState {
                requests: VecDeque::new(),
                closed: false,
            }))
            .changed(Condvar::new())
            .capacity(Arc::new(Semaphore::new(capacity)))
            .metrics(QueueMetrics::new(name, capacity))
            .build()
    }

    /// Publishes one request through the same admission path as atomic caller packets.
    pub fn push(&self, request: R) -> Result<(), TaskError> {
        self.push_batch(vec![request])
    }

    /// Reserves FIFO capacity without waking all competing producers after each dequeue.
    pub async fn push_async(&self, mut request: R) -> Result<(), TaskError> {
        let admission = Admission::new(
            "docparse_queue_admission_wait_seconds",
            self.metrics.name,
        );
        // Cancellation is terminal without needing space for an input that will never execute.
        if request.cancelled() && !self.capacity.is_closed() {
            admission.finish("cancelled");
            request.end_queue();
            return Ok(());
        }
        let permit = match Arc::clone(&self.capacity).try_acquire_owned() {
            Ok(permit) => Ok(permit),
            Err(tokio::sync::TryAcquireError::NoPermits) => {
                let _blocked = self.metrics.blocked();
                Arc::clone(&self.capacity).acquire_owned().await
            }
            Err(tokio::sync::TryAcquireError::Closed) => {
                admission.finish("closed");
                request.end_queue();
                return Err(TaskError::from_message("model queue closed"));
            }
        };
        let permit = permit.map_err(|error| {
            request.end_queue();
            TaskError::from_message(error.to_string())
        });
        let permit = match permit {
            Ok(permit) => permit,
            Err(error) => {
                admission.finish("closed");
                return Err(error);
            }
        };
        if request.cancelled() {
            admission.finish("cancelled");
            request.end_queue();
            return Ok(());
        }
        let result = self.publish(std::iter::once(request), permit);
        admission.finish(if result.is_ok() { "admitted" } else { "closed" });
        result
    }

    /// Admits a packet atomically using the same fair semaphore as asynchronous callers.
    pub fn push_batch(&self, mut requests: Vec<R>) -> Result<(), TaskError> {
        if requests.is_empty() || requests.len() > self.metrics.capacity {
            return Err(TaskError::from_message("invalid queue packet size"));
        }
        let count = u32::try_from(requests.len())
            .map_err(|error| TaskError::from_message(error.to_string()))?;
        let admission = Admission::new(
            "docparse_queue_admission_wait_seconds",
            self.metrics.name,
        );
        // Synchronous canceled packets must not park behind live queued work either.
        if requests.iter().all(SessionRequest::cancelled)
            && !self.capacity.is_closed()
        {
            admission.finish("cancelled");
            for request in &mut requests {
                request.end_queue();
            }
            return Ok(());
        }
        let permit =
            match Arc::clone(&self.capacity).try_acquire_many_owned(count) {
                Ok(permit) => Ok(permit),
                Err(tokio::sync::TryAcquireError::NoPermits) => {
                    let _blocked = self.metrics.blocked();
                    let waker =
                        Arc::new(ProducerThread(std::thread::current())).into();
                    let mut context = Context::from_waker(&waker);
                    let mut waiting = std::pin::pin!(
                        Arc::clone(&self.capacity).acquire_many_owned(count)
                    );
                    loop {
                        match waiting.as_mut().poll(&mut context) {
                            Poll::Ready(permit) => break permit,
                            Poll::Pending => std::thread::park(),
                        }
                    }
                }
                Err(tokio::sync::TryAcquireError::Closed) => {
                    admission.finish("closed");
                    for request in &mut requests {
                        request.end_queue();
                    }
                    return Err(TaskError::from_message("model queue closed"));
                }
            };
        let permit = match permit {
            Ok(permit) => permit,
            Err(error) => {
                admission.finish("closed");
                for request in &mut requests {
                    request.end_queue();
                }
                return Err(TaskError::from_message(error.to_string()));
            }
        };
        if requests.iter().all(SessionRequest::cancelled) {
            admission.finish("cancelled");
            for request in &mut requests {
                request.end_queue();
            }
            return Ok(());
        }
        let result = self.publish(requests.into_iter(), permit);
        admission.finish(if result.is_ok() { "admitted" } else { "closed" });
        result
    }

    /// Transfers reserved permits into queue ownership; close and publication share the state lock.
    fn publish(
        &self,
        requests: impl Iterator<Item = R>,
        mut permit: OwnedSemaphorePermit,
    ) -> Result<(), TaskError> {
        let mut state = self
            .state
            .lock()
            .map_err(|error| TaskError::from_message(error.to_string()))?;
        if state.closed {
            drop(state);
            for mut request in requests {
                request.end_queue();
            }
            return Err(TaskError::from_message("model queue closed"));
        }
        for request in requests {
            state.requests.push_back((
                Queued::new(request, &self.metrics),
                permit.split(1).expect("reserved packet capacity"),
            ));
            // Only consumers use this condition variable; producers receive individual semaphore permits.
            self.changed.notify_one();
        }
        Ok(())
    }

    /// Waits for live work and drains already-ready inputs without waiting to fill a batch.
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
            let mut released: Option<OwnedSemaphorePermit> = None;
            while requests.len() < limit {
                let Some((request, permit)) = state.requests.pop_front() else {
                    break;
                };
                let request = request.take("dequeued");
                // Return one batch of capacity to avoid a semaphore lock/wakeup for every input.
                match &mut released {
                    Some(released) => released.merge(permit),
                    None => released = Some(permit),
                }
                if request.cancelled() {
                    canceled.push(request);
                } else {
                    requests.push(request);
                }
            }
            drop(state);
            // Assigned waiters can publish immediately; never wake them while holding the queue lock.
            drop(released);
            for request in requests.iter_mut().chain(&mut canceled) {
                request.end_queue();
            }
            if !requests.is_empty() {
                return Some(requests);
            }
        }
    }

    /// Closes admission and wakes all consumers while releasing queued inputs outside the lock.
    pub fn close(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.closed = true;
        self.capacity.close();
        let discarded = std::mem::take(&mut state.requests);
        self.changed.notify_all();
        drop(state);
        for (request, _permit) in discarded {
            let mut request = request.take("discarded");
            request.end_queue();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Already-cancelled inputs must not wait behind a full queue or consume newly freed capacity.
    #[tokio::test]
    async fn cancelled_input_does_not_wait_for_capacity() {
        let queue = BlockingQueue::new("cancel-test", 1);
        queue.push(Request(0, false)).expect("fill queue");
        let mut canceled = Box::pin(queue.push_async(Request(1, true)));
        assert!(matches!(
            futures_util::poll!(&mut canceled),
            Poll::Ready(Ok(()))
        ));
        queue
            .push_batch(vec![Request(2, true)])
            .expect("cancelled sync packet");
        assert_eq!(
            queue
                .pop(1)
                .expect("original item")
                .first()
                .expect("request")
                .0,
            0
        );
    }

    /// Released capacity belongs to the oldest waiter even before that producer is polled again.
    #[tokio::test]
    async fn admission_does_not_allow_new_producers_to_barge() {
        let queue = BlockingQueue::new("fair-test", 1);
        queue.push(Request(0, false)).expect("fill queue");
        let mut first = Box::pin(queue.push_async(Request(1, false)));
        assert!(futures_util::poll!(&mut first).is_pending());
        assert_eq!(
            queue
                .pop(1)
                .expect("first item")
                .first()
                .expect("request")
                .0,
            0
        );
        let mut later = Box::pin(queue.push_async(Request(2, false)));
        assert!(
            futures_util::poll!(&mut later).is_pending(),
            "new producer stole the waiting producer's slot"
        );
        first.await.expect("oldest producer admitted");
        assert_eq!(
            queue
                .pop(1)
                .expect("oldest item")
                .first()
                .expect("request")
                .0,
            1
        );
        later.await.expect("later producer admitted");
        assert_eq!(
            queue
                .pop(1)
                .expect("later item")
                .first()
                .expect("request")
                .0,
            2
        );
    }

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

    /// Async admission must wake on both dequeue and close without parking the single executor thread.
    #[tokio::test]
    async fn async_admission_observes_capacity_and_closure() {
        let queue = BlockingQueue::new("test", 1);
        queue.push(Request(0, false)).expect("first packet");
        let waiting = queue.push_async(Request(1, false));
        tokio::pin!(waiting);
        assert!(futures_util::poll!(waiting.as_mut()).is_pending());
        assert_eq!(queue.pop(1).expect("first").first().expect("request").0, 0);
        tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
            .await
            .expect("wake after dequeue")
            .expect("admission");
        let waiting = queue.push_async(Request(2, false));
        tokio::pin!(waiting);
        assert!(futures_util::poll!(waiting.as_mut()).is_pending());
        queue.close();
        tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
            .await
            .expect("wake after close")
            .expect_err("closed queue rejects waiting producers");
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
