//! Shared scheduling and platform mechanics, independent of model and document types.
pub mod queue;
mod runtime;
mod task_set;
pub use task_set::{TaskSet, spawn};
mod thread;
mod timeout;
pub mod timing;

pub use queue::{PageLease, PageQueue, Queue, SessionRequest};
pub use runtime::{
    TaskError, WasmBoxedFuture, WasmCompatSend, WasmCompatSync, run_cpu,
};
pub use thread::ThreadManager;
pub use timeout::{Elapsed, timeout};

#[cfg(not(target_arch = "wasm32"))]
pub mod session;
#[cfg(not(target_arch = "wasm32"))]
pub use session::{SessionManager, SessionWorker};
