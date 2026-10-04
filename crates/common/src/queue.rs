//! Ready-only batching and cancellation shared by native and browser model consumers.
use crate::{
    WasmCompatSend,
    telemetry::{Admission, QueueMetrics, Queued},
};
use std::sync::Arc;
use tokio::sync::{Mutex, MutexGuard, mpsc};

mod page;
pub use page::{PageLease, PageQueue};

/// A queued request keeps its cancellation and original observation context until consumption.
pub trait SessionRequest: WasmCompatSend + 'static {
    /// Reports cancellation without transferring input or response ownership.
    fn cancelled(&self) -> bool;
    /// Finishes admission timing outside queue locks, including discarded requests.
    fn end_queue(&mut self);
}

/// Bounded admission instruments the actual channel ownership transition.
pub struct QueueSender<R> {
    sender: mpsc::Sender<Queued<R>>,
    metrics: Arc<QueueMetrics>,
}
impl<R> Clone for QueueSender<R> {
    /// Shares the original queue capacity registration across producer clones.
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            metrics: Arc::clone(&self.metrics),
        }
    }
}
impl<R> QueueSender<R> {
    /// Reserves before recording occupancy so blocked producers are not counted as queued items.
    pub async fn send(
        &self,
        value: R,
    ) -> Result<(), mpsc::error::SendError<R>> {
        let admission = Admission::new(
            "docparse_queue_admission_wait_seconds",
            self.metrics.name,
        );
        let permit = match self.sender.try_reserve() {
            Ok(permit) => Ok(permit),
            Err(mpsc::error::TrySendError::Full(_)) => {
                let _blocked = self.metrics.blocked();
                self.sender.reserve().await
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                Err(mpsc::error::SendError(()))
            }
        };
        match permit {
            Ok(permit) => {
                admission.finish("admitted");
                permit.send(Queued::new(value, &self.metrics));
                Ok(())
            }
            Err(_) => {
                admission.finish("closed");
                Err(mpsc::error::SendError(value))
            }
        }
    }
}

/// All asynchronous consumers of a model share the same bounded receiver.
pub struct Queue<R> {
    receiver: Arc<Mutex<mpsc::Receiver<Queued<R>>>>,
}

impl<R> Clone for Queue<R> {
    /// Clones only shared receiver ownership, without requiring requests to be clonable.
    fn clone(&self) -> Self {
        Self {
            receiver: Arc::clone(&self.receiver),
        }
    }
}

impl<R: SessionRequest> Queue<R> {
    /// Creates a positive-capacity queue with Tokio's standard bounded sender and backpressure.
    pub fn new(name: &'static str, capacity: usize) -> (QueueSender<R>, Self) {
        let metrics = QueueMetrics::new(name, capacity);
        let (sender, receiver) = mpsc::channel(metrics.capacity);
        (
            QueueSender { sender, metrics },
            Self {
                receiver: Arc::new(Mutex::new(receiver)),
            },
        )
    }

    /// Waits only for the first request; the caller may acquire a runtime guard before draining ready work.
    pub async fn recv(&self) -> Option<ReadyBatch<'_, R>> {
        let mut receiver = self.receiver.lock().await;
        let first = receiver.recv().await?.take("dequeued");
        Some(ReadyBatch { first, receiver })
    }
}

/// Holds one receiver turn until a consumer is ready to choose its physical model batch.
pub struct ReadyBatch<'a, R> {
    first: R,
    receiver: MutexGuard<'a, mpsc::Receiver<Queued<R>>>,
}

impl<R: SessionRequest> ReadyBatch<'_, R> {
    /// Takes up to a positive limit of live, already-ready requests without waiting to fill the batch.
    pub fn take_ready(self, limit: usize) -> Vec<R> {
        assert!(limit > 0, "batch limit must be positive");
        let Self {
            first,
            mut receiver,
        } = self;
        let mut requests = Vec::with_capacity(limit);
        let mut canceled = Vec::new();
        let mut next = Some(first);
        while let Some(request) = next {
            if request.cancelled() {
                canceled.push(request);
            } else {
                requests.push(request);
            }
            if requests.len() == limit {
                break;
            }
            next = receiver
                .try_recv()
                .ok()
                .map(|request| request.take("dequeued"));
        }
        // Timing observers and input destruction must never run while excluding another consumer.
        drop(receiver);
        for request in requests.iter_mut().chain(&mut canceled) {
            request.end_queue();
        }
        requests
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod blocking;
#[cfg(not(target_arch = "wasm32"))]
pub use blocking::BlockingQueue;

#[cfg(test)]
mod tests {
    use super::*;

    /// A closed response represents caller cancellation without model-specific test support.
    struct Request(usize, tokio::sync::oneshot::Sender<usize>);
    impl SessionRequest for Request {
        /// Uses the same receiver lifetime signal as production model requests.
        fn cancelled(&self) -> bool {
            self.1.is_closed()
        }
        /// This model-independent request has no timing observer.
        fn end_queue(&mut self) {}
    }

    /// Consumers share one queue, skip canceled entries, drain full batches, and flush short tails immediately.
    #[tokio::test]
    async fn consumers_share_ready_work_without_waiting_for_full_batches() {
        let (sender, first) = Queue::new("test", 4);
        let second = first.clone();
        let mut replies = Vec::new();
        for id in 0..4 {
            let (response, reply) = tokio::sync::oneshot::channel();
            // Preserve the channel error in the failure diagnostic instead of discarding it.
            sender.send(Request(id, response)).await.expect("send");
            replies.push(reply);
        }
        drop(replies.remove(1));
        let batch = first.recv().await.expect("first").take_ready(2);
        assert_eq!(
            batch.iter().map(|request| request.0).collect::<Vec<_>>(),
            [0, 2]
        );
        for request in batch {
            request.1.send(request.0).expect("reply");
        }
        let batch = second.recv().await.expect("tail").take_ready(2);
        assert_eq!(batch.len(), 1);
        for request in batch {
            request.1.send(request.0).expect("reply");
        }
        drop(sender);
        assert!(first.recv().await.is_none());
        for (reply, expected) in replies.into_iter().zip([0, 2, 3]) {
            assert_eq!(reply.await.expect("reply"), expected);
        }
    }
}
