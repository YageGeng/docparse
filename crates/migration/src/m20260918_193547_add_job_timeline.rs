use sea_orm_migration::{prelude::*, schema::*};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
#[allow(
    elided_lifetimes_in_paths,
    reason = "SeaORM migration trait requires this lifetime"
)]
impl MigrationTrait for Migration {
    /// Adds explicit timeline facts without inventing start/end times for historical work.
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(ParseJobs::Table)
                    .add_column(timestamp_with_time_zone_null(
                        ParseJobs::StartedAt,
                    ))
                    .add_column(timestamp_with_time_zone_null(
                        ParseJobs::FinishedAt,
                    ))
                    .add_column(timestamp_with_time_zone_null(
                        ParseJobs::AttemptStartedAt,
                    ))
                    .add_column(timestamp_with_time_zone_null(
                        ParseJobs::QueuedAt,
                    ))
                    .to_owned(),
            )
            .await
    }
    /// Removes only the timeline fields introduced here.
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(ParseJobs::Table)
                    .drop_column(ParseJobs::StartedAt)
                    .drop_column(ParseJobs::FinishedAt)
                    .drop_column(ParseJobs::AttemptStartedAt)
                    .drop_column(ParseJobs::QueuedAt)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum ParseJobs {
    Table,
    StartedAt,
    FinishedAt,
    AttemptStartedAt,
    QueuedAt,
}
