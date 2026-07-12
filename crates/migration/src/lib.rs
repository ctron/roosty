#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

pub use sea_orm_migration::prelude::*;

mod m20260701_000001_create_job_table;
mod m20260701_000002_create_local_account_table;
mod m20260701_000003_create_oauth_tables;
mod m20260701_000004_add_local_account_settings;
mod m20260701_000005_create_local_status_table;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260701_000001_create_job_table::Migration),
            Box::new(m20260701_000002_create_local_account_table::Migration),
            Box::new(m20260701_000003_create_oauth_tables::Migration),
            Box::new(m20260701_000004_add_local_account_settings::Migration),
            Box::new(m20260701_000005_create_local_status_table::Migration),
        ]
    }
}
