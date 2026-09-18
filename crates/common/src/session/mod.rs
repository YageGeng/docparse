//! Native session ownership and shared-queue consumers.
mod manager;
mod worker;

pub use manager::SessionManager;
pub use worker::SessionWorker;
