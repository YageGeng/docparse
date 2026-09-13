use sea_orm_migration::{prelude::*, schema::*};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
#[allow(
    elided_lifetimes_in_paths,
    reason = "SeaORM migration trait requires its late-bound SchemaManager lifetime"
)]
impl MigrationTrait for Migration {
    /// Stores measured attempt durations without inventing timings for historical jobs.
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(ParseJobs::Table)
                    .add_column(big_integer_null(ParseJobs::DurationMs))
                    .to_owned(),
            )
            .await
    }

    /// Removes only the duration column introduced by this migration.
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(ParseJobs::Table)
                    .drop_column(ParseJobs::DurationMs)
                    .to_owned(),
            )
            .await
    }
}

/// Keeps the duration column in typed SeaQuery expressions.
#[derive(DeriveIden)]
enum ParseJobs {
    Table,
    DurationMs,
}
