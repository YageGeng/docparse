use sea_orm_migration::{prelude::*, schema::*};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
#[allow(
    elided_lifetimes_in_paths,
    reason = "SeaORM migration trait requires its late-bound SchemaManager lifetime"
)]
impl MigrationTrait for Migration {
    /// Preserves pagination anchors and submission identities after removing a task from public views.
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(ParseJobs::Table)
                    .add_column(timestamp_with_time_zone_null(
                        ParseJobs::DeletedAt,
                    ))
                    .to_owned(),
            )
            .await
    }

    /// Removes the deletion marker when explicitly rolling back this schema change.
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(ParseJobs::Table)
                    .drop_column(ParseJobs::DeletedAt)
                    .to_owned(),
            )
            .await
    }
}

/// Names deletion metadata through SeaQuery identifiers.
#[derive(DeriveIden)]
enum ParseJobs {
    Table,
    DeletedAt,
}
