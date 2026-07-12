#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

pub use sea_orm_migration::prelude::*;

mod m20260701_000001_create_job_table;
mod m20260701_000002_create_local_account_table;
mod m20260701_000003_create_oauth_tables;
mod m20260701_000004_add_local_account_settings;
mod m20260701_000005_create_local_status_table;
mod m20260701_000006_create_local_status_favourite_table;
mod m20260701_000007_create_local_status_bookmark_table;
mod m20260701_000008_create_local_follow_table;
mod m20260701_000009_add_collection_cursor_ids;
mod m20260701_000010_create_local_media_attachment_table;
mod m20260701_000011_add_local_media_preview_metadata;
mod m20260701_000012_add_local_account_profile_images;
mod m20260701_000013_create_local_notification_table;
mod m20260701_000014_create_local_status_reblog_table;
mod m20260701_000015_allow_reblog_notifications;

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
            Box::new(m20260701_000006_create_local_status_favourite_table::Migration),
            Box::new(m20260701_000007_create_local_status_bookmark_table::Migration),
            Box::new(m20260701_000008_create_local_follow_table::Migration),
            Box::new(m20260701_000009_add_collection_cursor_ids::Migration),
            Box::new(m20260701_000010_create_local_media_attachment_table::Migration),
            Box::new(m20260701_000011_add_local_media_preview_metadata::Migration),
            Box::new(m20260701_000012_add_local_account_profile_images::Migration),
            Box::new(m20260701_000013_create_local_notification_table::Migration),
            Box::new(m20260701_000014_create_local_status_reblog_table::Migration),
            Box::new(m20260701_000015_allow_reblog_notifications::Migration),
        ]
    }
}
