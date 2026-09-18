pub use sea_orm_migration::prelude::*;

mod m20260912_131310_create_parse_jobs;
mod m20260913_145511_add_job_file_metadata;
mod m20260913_173134_add_job_duration;
mod m20260913_174348_add_job_deletion;
mod m20260918_193547_add_job_timeline;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    /// Returns schema changes in their CLI-generated execution order.
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260912_131310_create_parse_jobs::Migration),
            Box::new(m20260913_145511_add_job_file_metadata::Migration),
            Box::new(m20260913_173134_add_job_duration::Migration),
            Box::new(m20260913_174348_add_job_deletion::Migration),
            Box::new(m20260918_193547_add_job_timeline::Migration),
        ]
    }
}
