pub use sea_orm_migration::prelude::*;

mod m20260912_131310_create_parse_jobs;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    /// Returns schema changes in their CLI-generated execution order.
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(m20260912_131310_create_parse_jobs::Migration)]
    }
}
