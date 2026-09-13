use crate::error::DatabaseError;
use docparse_config::DatabaseConfig;
use docparse_migration::{Migrator, MigratorTrait};
use sea_orm::sea_query::{Expr, Func, Query};
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection,
    TransactionTrait,
};
use std::time::Duration;

/// Opens the configured PostgreSQL pool and applies pending migrations before returning a usable connection.
pub async fn connect(
    config: &DatabaseConfig,
) -> Result<DatabaseConnection, DatabaseError> {
    config.validate()?;
    // SQLx logging must be enabled for the configured regular and slow query levels to take effect.
    let mut options = ConnectOptions::new(&config.url);
    options
        .max_connections(config.max_connections)
        .min_connections(config.min_connections)
        .connect_timeout(Duration::from_millis(config.timeout_ms))
        .acquire_timeout(Duration::from_millis(config.acquire_timeout_ms))
        .idle_timeout(Duration::from_millis(config.idle_timeout_ms))
        .sqlx_logging(true)
        .sqlx_logging_level(config.sqlx_logging_level)
        .sqlx_slow_statements_logging_settings(
            config.sqlx_slow_statements_logging_level,
            Duration::from_millis(config.sqlx_slow_statements_threshold_ms),
        );

    let db = Database::connect(options).await?;
    let transaction = db.begin().await?;
    // Serialize application startup before even creating migration history. Transaction scope releases the lock on error/cancellation.
    let _ = transaction
        .query_one(
            &Query::select()
                .expr(
                    Func::cust("pg_advisory_xact_lock")
                        .arg(Expr::val(i64::from_be_bytes(*b"docparse"))),
                )
                .to_owned(),
        )
        .await?;
    Migrator::up(&transaction, None).await?;
    transaction.commit().await?;
    Ok(db)
}
