//! Shared response classification for the server's HTTP middleware.

mod routine;
pub mod trace;

pub use routine::{RoutineCompletion, mark_routine};
