use sea_orm_migration::{prelude::*, schema::*};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
#[allow(
    elided_lifetimes_in_paths,
    reason = "SeaORM migration trait requires its late-bound SchemaManager lifetime"
)]
impl MigrationTrait for Migration {
    /// Adds optional upload metadata without inventing names or sizes for previously stored PDFs.
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(ParseJobs::Table)
                    .add_column(string_len_null(ParseJobs::Filename, 255))
                    .add_column(big_integer_null(ParseJobs::SizeBytes))
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_parse_jobs_history")
                    .table(ParseJobs::Table)
                    .col(ParseJobs::CreatedAt)
                    .col(ParseJobs::Id)
                    .to_owned(),
            )
            .await
    }

    /// Removes only metadata and the history index introduced by this migration.
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name("idx_parse_jobs_history")
                    .table(ParseJobs::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(ParseJobs::Table)
                    .drop_column(ParseJobs::Filename)
                    .drop_column(ParseJobs::SizeBytes)
                    .to_owned(),
            )
            .await
    }
}

/// Keeps upload metadata and pagination identifiers in typed SeaQuery expressions.
#[derive(DeriveIden)]
enum ParseJobs {
    Table,
    Filename,
    SizeBytes,
    CreatedAt,
    Id,
}
