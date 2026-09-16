//! Browser task ownership and completion collection, independent of the document scheduler.
use crate::wasm_compat::{TaskError, WasmBoxedFuture};
use futures_util::{StreamExt, stream::FuturesUnordered};
use std::future::Future;
use tokio::sync::oneshot;

/// Eager local tasks whose uncollected results remain bounded by the document scheduler.
pub(crate) struct TaskSet<T: 'static> {
    tasks: FuturesUnordered<WasmBoxedFuture<'static, Result<T, TaskError>>>,
}
impl<T: 'static> TaskSet<T> {
    /// Creates a local task group without a Tokio runtime.
    pub(crate) fn new() -> Self {
        Self {
            tasks: FuturesUnordered::new(),
        }
    }
    /// Returns the outstanding local task count.
    pub(crate) fn len(&self) -> usize {
        self.tasks.len()
    }
    /// Reports whether all local completions have been collected.
    pub(crate) fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }
    /// Starts local work independently of downstream capacity while retaining cancellation ownership.
    pub(crate) fn spawn<F: Future<Output = T> + 'static>(&mut self, future: F) {
        self.tasks.push(spawn(future));
    }
    /// Polls a local completion without imposing Send.
    pub(crate) async fn join_next(&mut self) -> Option<Result<T, TaskError>> {
        self.tasks.next().await
    }
    /// Drops pending callers; the inference actor still owns any request already dispatched.
    pub(crate) fn abort_all(&mut self) {
        tracing::debug!("cancelling {} browser tasks", self.tasks.len());
        self.tasks.clear();
    }
}

/// Starts local work immediately and cancels it when its completion future is dropped.
pub(crate) fn spawn<F>(
    future: F,
) -> WasmBoxedFuture<'static, Result<F::Output, TaskError>>
where
    F: Future + 'static,
    F::Output: 'static,
{
    let (mut sender, receiver) = oneshot::channel();
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
