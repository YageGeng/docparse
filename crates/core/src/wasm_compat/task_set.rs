//! Native and browser task scheduling behind one compatibility interface.
use super::TaskError;
use std::future::Future;

#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
mod platform {
    use super::*;
    use crate::wasm_compat::{WasmBoxedFuture, WasmCompatSend};

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
        pub(crate) fn spawn<
            F: Future<Output = T> + WasmCompatSend + 'static,
        >(
            &mut self,
            future: F,
        ) {
            self.tasks.spawn(future);
        }
        /// Collects one completion with a platform-neutral failure.
        pub(crate) async fn join_next(
            &mut self,
        ) -> Option<Result<T, TaskError>> {
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

    /// Starts a native task and retains a joinable completion future.
    pub(crate) fn spawn<F>(
        future: F,
    ) -> WasmBoxedFuture<'static, Result<F::Output, TaskError>>
    where
        F: Future + WasmCompatSend + 'static,
        F::Output: WasmCompatSend + 'static,
    {
        let handle = tokio::spawn(future);
        Box::pin(async move { handle.await.map_err(TaskError::from) })
    }
}

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
mod platform {
    use super::*;
    use crate::wasm_compat::WasmBoxedFuture;
    use futures_util::{StreamExt, stream::FuturesUnordered};
    use tokio::sync::oneshot;

    /// Worker-local task collection polled cooperatively by the shared pipeline.
    pub(crate) struct TaskSet<T: 'static> {
        tasks: FuturesUnordered<WasmBoxedFuture<'static, T>>,
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
        /// Queues local work for the next group poll.
        pub(crate) fn spawn<F: Future<Output = T> + 'static>(
            &mut self,
            future: F,
        ) {
            self.tasks.push(Box::pin(future));
        }
        /// Polls a local completion without imposing Send.
        pub(crate) async fn join_next(
            &mut self,
        ) -> Option<Result<T, TaskError>> {
            self.tasks.next().await.map(Ok)
        }
        /// Drops pending callers; the inference actor still owns any request already dispatched.
        pub(crate) fn abort_all(&mut self) {
            self.tasks.clear();
        }
    }

    /// Starts owned browser-local work with an observable completion channel.
    pub(crate) fn spawn<F>(
        future: F,
    ) -> WasmBoxedFuture<'static, Result<F::Output, TaskError>>
    where
        F: Future + 'static,
        F::Output: 'static,
    {
        let (sender, receiver) = oneshot::channel();
        wasm_bindgen_futures::spawn_local(async move {
            let _ = sender.send(future.await);
        });
        Box::pin(async move {
            receiver
                .await
                .map_err(|error| TaskError::from_message(error.to_string()))
        })
    }
}

pub(crate) use platform::{TaskSet, spawn};
