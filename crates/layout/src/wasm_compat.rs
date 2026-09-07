//! Platform-specific execution and thread bounds with explicitly scoped compatibility submodules.
mod session_pool;

pub(crate) use session_pool::LayoutSessionPool;

use std::future::Future;
use std::pin::Pin;

#[cfg(all(target_arch = "wasm32", not(feature = "wasm")))]
compile_error!("docparse Web builds require the wasm feature");
#[cfg(all(target_arch = "wasm32", not(target_os = "unknown")))]
compile_error!("docparse supports wasm32-unknown-unknown browser builds only");
#[cfg(all(
    target_arch = "wasm32",
    any(feature = "cuda", feature = "coreml", feature = "openvino")
))]
compile_error!("native execution providers cannot be enabled in a Web build");
#[cfg(any(
    all(feature = "cuda", feature = "coreml"),
    all(feature = "cuda", feature = "openvino"),
    all(feature = "coreml", feature = "openvino")
))]
compile_error!(
    "only one optional ONNX Runtime execution provider may be enabled"
);

#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
/// Requires values to be transferable between native threads.
pub trait WasmCompatSend: Send {}
#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
/// Marks values confined to a single browser Worker.
pub trait WasmCompatSend {}
#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
impl<T: Send + ?Sized> WasmCompatSend for T {}
#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
impl<T: ?Sized> WasmCompatSend for T {}

#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
/// Requires shared native values to support concurrent access.
pub trait WasmCompatSync: Sync {}
#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
/// Marks shared values confined to a single browser Worker.
pub trait WasmCompatSync {}
#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
impl<T: Sync + ?Sized> WasmCompatSync for T {}
#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
impl<T: ?Sized> WasmCompatSync for T {}

#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
/// A boxed future that preserves the native Send requirement.
pub type WasmBoxedFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
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

#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
mod native;
#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
mod web;

#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
pub use native::{inspect_model, model_metadata, run_cpu};
#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
pub use web::{model_metadata, run_cpu};
