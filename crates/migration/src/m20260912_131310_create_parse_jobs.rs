use sea_orm_migration::{prelude::*, schema::*};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
#[allow(
    elided_lifetimes_in_paths,
    reason = "SeaORM migration trait requires its late-bound SchemaManager lifetime"
)]
impl MigrationTrait for Migration {
    /// Creates durable parse tasks and indexes using SeaQuery schema builders.
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(ParseJobs::Table)
                    .col(uuid(ParseJobs::Id).primary_key())
                    .col(string_len(ParseJobs::InputHash, 64))
                    .col(string_len(ParseJobs::Status, 16).default("queued"))
                    .col(json_binary_null(ParseJobs::Progress))
                    .col(big_integer(ParseJobs::Version).default(1))
                    .col(integer(ParseJobs::Attempts).default(0))
                    .col(uuid_null(ParseJobs::LeaseToken))
                    .col(timestamp_with_time_zone_null(ParseJobs::LeaseUntil))
                    .col(string_null(ParseJobs::ResultPath))
                    .col(string_null(ParseJobs::Error))
                    .col(
                        timestamp_with_time_zone(ParseJobs::CreatedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        timestamp_with_time_zone(ParseJobs::UpdatedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_parse_jobs_queue")
                    .table(ParseJobs::Table)
                    .col(ParseJobs::Status)
                    .col(ParseJobs::CreatedAt)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_parse_jobs_lease")
                    .table(ParseJobs::Table)
                    .col(ParseJobs::Status)
                    .col(ParseJobs::LeaseUntil)
                    .to_owned(),
            )
            .await
    }

    /// Removes only the task table introduced by this migration; its indexes follow the table.
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(ParseJobs::Table).to_owned())
            .await
    }
}

/// Stable database identifiers keep every schema expression typed and quoted.
#[derive(DeriveIden)]
enum ParseJobs {
    Table,
    Id,
    InputHash,
    Status,
    Progress,
    Version,
    Attempts,
    LeaseToken,
    LeaseUntil,
    ResultPath,
    Error,
    CreatedAt,
    UpdatedAt,
}
