//! Shared response classification for the server's HTTP middleware.

pub mod cpu_body;
mod routine;
pub mod trace;

pub use routine::{RoutineCompletion, mark_routine};
