//! Platform-specific execution and thread bounds with explicitly scoped compatibility submodules.
mod backend;
mod session_pool;
pub use backend::{ExecutionProvider, OnnxBackend};
pub use docparse_common::{Elapsed, timeout};
#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
pub use docparse_common::{SessionManager, SessionRequest};

pub(crate) use session_pool::LayoutSessionPool;

// Keep public platform paths stable while common owns runtime mechanics.
pub use docparse_common::{
    TaskError, WasmBoxedFuture, WasmCompatSend, WasmCompatSync, run_cpu,
};

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
mod native;
#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
mod web;

#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
pub use docparse_common::SessionWorker;
#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
pub use native::{inspect_model, model_metadata};
#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
pub use web::model_metadata;
