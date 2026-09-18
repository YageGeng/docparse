//! Portable task contracts and finite CPU execution.
use std::{future::Future, pin::Pin};

#[cfg(not(target_arch = "wasm32"))]
/// Requires values to be transferable between native threads.
pub trait WasmCompatSend: Send {}
#[cfg(target_arch = "wasm32")]
/// Marks values confined to a single browser Worker.
pub trait WasmCompatSend {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Send + ?Sized> WasmCompatSend for T {}
#[cfg(target_arch = "wasm32")]
impl<T: ?Sized> WasmCompatSend for T {}

#[cfg(not(target_arch = "wasm32"))]
/// Requires shared native values to support concurrent access.
pub trait WasmCompatSync: Sync {}
#[cfg(target_arch = "wasm32")]
/// Marks shared values confined to a single browser Worker.
pub trait WasmCompatSync {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Sync + ?Sized> WasmCompatSync for T {}
#[cfg(target_arch = "wasm32")]
impl<T: ?Sized> WasmCompatSync for T {}

#[cfg(not(target_arch = "wasm32"))]
/// A boxed future that preserves the native Send requirement.
pub type WasmBoxedFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
#[cfg(target_arch = "wasm32")]
/// A boxed future polled locally inside the current browser Worker.
pub type WasmBoxedFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// Owned task failure that never contains browser handles.
#[derive(Debug, thiserror::Error)]
#[error("runtime task failed: {0}")]
pub struct TaskError(pub(crate) String);

impl TaskError {
    /// Converts an owned platform failure into a portable task error.
    pub fn from_message(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod platform {
    use super::*;
    impl From<tokio::task::JoinError> for TaskError {
        /// Retains the native task failure without exposing Tokio in shared APIs.
        fn from(error: tokio::task::JoinError) -> Self {
            Self(error.to_string())
        }
    }

    /// Runs owned CPU work away from the native async executor.
    pub async fn run_cpu<F, T>(operation: F) -> Result<T, TaskError>
    where
        F: FnOnce() -> T + WasmCompatSend + 'static,
        T: WasmCompatSend + 'static,
    {
        // A span enters its own subscriber but does not make that subscriber the thread's default dispatcher.
        let span = tracing::Span::current();
        let dispatcher = tracing::dispatcher::get_default(Clone::clone);
        let lease = crate::PageLease::current();
        tokio::task::spawn_blocking(move || {
            let output = tracing::dispatcher::with_default(&dispatcher, || {
                if let Some(lease) = &lease {
                    return lease.scope_sync(|| span.in_scope(operation));
                }
                span.in_scope(operation)
            });
            // Uncollected outputs also retain the delivery when the async caller has been canceled.
            (output, lease)
        })
        .await
        .map(|(output, _lease)| output)
        .map_err(TaskError::from)
    }
}
#[cfg(target_arch = "wasm32")]
mod platform {
    use super::*;
    /// Executes a CPU segment within the dedicated browser Worker.
    pub async fn run_cpu<F, T>(operation: F) -> Result<T, TaskError>
    where
        F: FnOnce() -> T + WasmCompatSend + 'static,
        T: WasmCompatSend + 'static,
    {
        Ok(operation())
    }
}
pub use platform::run_cpu;
