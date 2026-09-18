//! Browser task ownership and completion collection, independent of the document scheduler.
use crate::{TaskError, WasmBoxedFuture};
use futures_util::{StreamExt, stream::FuturesUnordered};
use std::future::Future;
use tokio::sync::oneshot;

/// Eager local tasks whose uncollected results remain bounded by the document scheduler.
pub struct TaskSet<T: 'static> {
    tasks: FuturesUnordered<WasmBoxedFuture<'static, Result<T, TaskError>>>,
}

impl<T: 'static> Default for TaskSet<T> {
    /// Creates the same empty owned task collection as the explicit constructor.
    fn default() -> Self {
        Self::new()
    }
}

impl<T: 'static> TaskSet<T> {
    /// Creates a local task group without a Tokio runtime.
    pub fn new() -> Self {
        Self {
            tasks: FuturesUnordered::new(),
        }
    }
    /// Returns the outstanding local task count.
    pub fn len(&self) -> usize {
        self.tasks.len()
    }
    /// Reports whether all local completions have been collected.
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }
    /// Starts local work independently of downstream capacity while retaining cancellation ownership.
    pub fn spawn<F: Future<Output = T> + 'static>(&mut self, future: F) {
        self.tasks.push(spawn(future));
    }
    /// Polls a local completion without imposing Send.
    pub async fn join_next(&mut self) -> Option<Result<T, TaskError>> {
        self.tasks.next().await
    }
    /// Drops pending callers; the inference actor still owns any request already dispatched.
    pub fn abort_all(&mut self) {
        tracing::debug!("cancelling {} browser tasks", self.tasks.len());
        self.tasks.clear();
    }
}

/// Starts local work immediately and cancels it when its completion future is dropped.
pub fn spawn<F>(
    future: F,
) -> WasmBoxedFuture<'static, Result<F::Output, TaskError>>
where
    F: Future + 'static,
    F::Output: 'static,
{
    let (mut sender, receiver) = oneshot::channel();
    let lease = crate::PageLease::current();
    let future = async move {
        match lease {
            Some(lease) => lease.scope(future).await,
            None => future.await,
        }
    };
    wasm_bindgen_futures::spawn_local(async move {
        // Receiver ownership cancels even an unpolled completion; do not poll already-cancelled work.
        tokio::select! {
            biased;
            _ = sender.closed() => {},
            output = future => { let _ = sender.send(output); },
        }
    });
    Box::pin(async move {
        receiver.await.map_err(|error| {
            tracing::warn!("browser task completion failed: {}", error);
            TaskError::from_message(error.to_string())
        })
    })
}
