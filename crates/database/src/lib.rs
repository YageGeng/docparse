//! SeaORM connections, generated entities, and durable task queries.
pub mod connection;
pub mod entities;
pub mod error;
mod job;
pub mod query;
pub use job::JobStatus;

/// Re-exports the database boundary for downstream queries without a second ORM version.
pub mod seaorm {
    pub use sea_orm::*;
}
