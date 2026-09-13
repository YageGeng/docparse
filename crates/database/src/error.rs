/// Typed persistence failures remain independent of server-only Snafu and HTTP status codes.
#[derive(Debug, thiserror::Error)]
pub enum DatabaseError {
    /// Invalid pool settings fail before the driver opens any connection.
    #[error(transparent)]
    Configuration(#[from] docparse_config::ConfigError),
    #[error(transparent)]
    SeaOrm(#[from] sea_orm::DbErr),
    #[error("idempotency key already refers to a different PDF")]
    IdempotencyConflict,
    #[error("invalid task or lease parameters")]
    InvalidInput,
}
