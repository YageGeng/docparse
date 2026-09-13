use crate::error::DatabaseError;
use docparse_config::DatabaseConfig;
use sea_orm::{ConnectOptions, Database, DatabaseConnection};
use std::time::Duration;

/// Opens a configured PostgreSQL pool without logging credentials or running schema changes at API startup.
pub async fn connect(
    config: &DatabaseConfig,
) -> Result<DatabaseConnection, DatabaseError> {
    config.validate()?;
    // Pool budgets come from the shared configuration instead of being fixed for every replica.
    let mut options = ConnectOptions::new(&config.url);
    options
        .max_connections(config.max_connections)
        .min_connections(config.min_connections)
        .connect_timeout(Duration::from_millis(config.timeout_ms))
        .acquire_timeout(Duration::from_millis(config.acquire_timeout_ms))
        .idle_timeout(Duration::from_millis(config.idle_timeout_ms))
        .sqlx_logging(false);

    Database::connect(options)
        .await
        .map_err(DatabaseError::from)
}
