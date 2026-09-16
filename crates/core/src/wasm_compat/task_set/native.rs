//! Native task scheduling with owned cancellation and tracing context.
use crate::wasm_compat::{TaskError, WasmBoxedFuture, WasmCompatSend};
use std::future::Future;
use tracing::{Instrument, instrument::WithSubscriber};

/// Native task collection retaining Tokio scheduling and cancellation semantics.
pub(crate) struct TaskSet<T: WasmCompatSend + 'static> {
    tasks: tokio::task::JoinSet<T>,
}

impl<T: WasmCompatSend + 'static> TaskSet<T> {
    /// Creates an empty bounded-by-caller task group.
    pub(crate) fn new() -> Self {
        Self {
            tasks: tokio::task::JoinSet::new(),
        }
    }
    /// Returns the number of tasks awaiting collection.
    pub(crate) fn len(&self) -> usize {
        self.tasks.len()
    }
    /// Reports whether every task has been collected.
    pub(crate) fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }
    /// Starts native work immediately using the active Tokio runtime.
    pub(crate) fn spawn<F: Future<Output = T> + WasmCompatSend + 'static>(
        &mut self,
        future: F,
    ) {
        // Span identity and the caller's dispatcher must both survive a scheduler hop.
        self.tasks
            .spawn(future.in_current_span().with_current_subscriber());
    }
    /// Collects one completion with a platform-neutral failure.
    pub(crate) async fn join_next(&mut self) -> Option<Result<T, TaskError>> {
        self.tasks
            .join_next()
            .await
            .map(|result| result.map_err(TaskError::from))
    }
    /// Cancels async callers without pretending to stop an already-running blocking closure.
    pub(crate) fn abort_all(&mut self) {
        self.tasks.abort_all();
    }
}

/// Starts an owned native task; dropping its completion future aborts pending work.
pub(crate) fn spawn<F>(
    future: F,
) -> WasmBoxedFuture<'static, Result<F::Output, TaskError>>
where
    F: Future + WasmCompatSend + 'static,
    F::Output: WasmCompatSend + 'static,
{
    // Own the task before the completion future is polled so cancellation cannot detach it.
    let mut task = TaskSet::new();
    task.spawn(future);
    Box::pin(async move {
        task.join_next().await.expect("the owned task was spawned")
    })
}
