//! Platform declarations and exports for owned task scheduling.
#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
mod native;
#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
pub(crate) use native::{TaskSet, spawn};

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
mod web;
#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
pub(crate) use web::{TaskSet, spawn};
