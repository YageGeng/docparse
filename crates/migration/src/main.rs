use sea_orm_migration::prelude::*;

/// Runs the standalone SeaORM migration command without loading parser models.
#[tokio::main]
async fn main() {
    cli::run_cli(docparse_migration::Migrator).await;
}
