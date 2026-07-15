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
mod m20260701_000016_create_local_conversation_tables;
mod m20260701_000017_create_local_tag_tables;
mod m20260701_000018_create_local_tag_follow_table;
mod m20260701_000019_create_local_timeline_marker_table;
mod m20260701_000020_create_local_account_moderation_tables;
mod m20260701_000021_create_local_actor_key_table;
mod m20260701_000022_create_remote_actor_table;
mod m20260701_000023_create_remote_follow_tables;
mod m20260701_000024_allow_remote_notification_actors;
mod m20260701_000025_constrain_job_attempts_nonnegative;
mod m20260701_000026_create_remote_status_table;
mod m20260701_000027_create_remote_following_table;
mod m20260701_000028_add_remote_status_notifications;
mod m20260701_000029_add_remote_reply_targets;
mod m20260701_000030_add_remote_status_references;
mod m20260701_000031_create_local_status_remote_mention;
mod m20260701_000032_create_federated_favourite_tables;
mod m20260701_000033_create_federated_reblog_tables;
mod m20260701_000034_create_remote_media_attachment;
mod m20260701_000035_add_job_claim_id;
mod m20260701_000036_add_remote_actor_profile_created_at;
mod m20260701_000037_create_remote_profile_media;
mod m20260701_000038_add_remote_actor_lifecycle;

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
            Box::new(m20260701_000016_create_local_conversation_tables::Migration),
            Box::new(m20260701_000017_create_local_tag_tables::Migration),
            Box::new(m20260701_000018_create_local_tag_follow_table::Migration),
            Box::new(m20260701_000019_create_local_timeline_marker_table::Migration),
            Box::new(m20260701_000020_create_local_account_moderation_tables::Migration),
            Box::new(m20260701_000021_create_local_actor_key_table::Migration),
            Box::new(m20260701_000022_create_remote_actor_table::Migration),
            Box::new(m20260701_000023_create_remote_follow_tables::Migration),
            Box::new(m20260701_000024_allow_remote_notification_actors::Migration),
            Box::new(m20260701_000025_constrain_job_attempts_nonnegative::Migration),
            Box::new(m20260701_000026_create_remote_status_table::Migration),
            Box::new(m20260701_000027_create_remote_following_table::Migration),
            Box::new(m20260701_000028_add_remote_status_notifications::Migration),
            Box::new(m20260701_000029_add_remote_reply_targets::Migration),
            Box::new(m20260701_000030_add_remote_status_references::Migration),
            Box::new(m20260701_000031_create_local_status_remote_mention::Migration),
            Box::new(m20260701_000032_create_federated_favourite_tables::Migration),
            Box::new(m20260701_000033_create_federated_reblog_tables::Migration),
            Box::new(m20260701_000034_create_remote_media_attachment::Migration),
            Box::new(m20260701_000035_add_job_claim_id::Migration),
            Box::new(m20260701_000036_add_remote_actor_profile_created_at::Migration),
            Box::new(m20260701_000037_create_remote_profile_media::Migration),
            Box::new(m20260701_000038_add_remote_actor_lifecycle::Migration),
        ]
    }
}
