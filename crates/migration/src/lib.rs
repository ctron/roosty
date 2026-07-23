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
mod m20260701_000039_add_remote_media_preview_metadata;
mod m20260701_000040_create_remote_custom_emoji;
mod m20260701_000041_add_remote_direct_conversations;
mod m20260701_000042_create_local_conversation_remote_participant;
mod m20260701_000043_add_follow_request_notifications;
mod m20260701_000044_add_direct_status_recipients;
mod m20260701_000045_constrain_status_visibility;
mod m20260701_000046_harden_inbox_replay;
mod m20260701_000047_add_remote_actor_followers_url;
mod m20260701_000048_create_streaming_event;
mod m20260701_000049_create_remote_account_moderation;
mod m20260701_000050_allow_status_update_streaming_event;
mod m20260701_000051_add_remote_follow_options;
mod m20260701_000052_allow_status_notifications;
mod m20260701_000053_add_status_edit_delivery;
mod m20260701_000054_add_streaming_status_metadata;
mod m20260701_000055_create_status_edit_history;
mod m20260701_000056_create_remote_status_tag;
mod m20260701_000057_create_status_quotes;
mod m20260701_000058_create_status_pins;
mod m20260701_000059_create_featured_tags;
mod m20260701_000060_create_push_subscription;
mod m20260701_000061_constrain_pkce_method;
mod m20260701_000062_create_lists;
mod m20260701_000063_constrain_inbox_activity_type;
mod m20260701_000064_add_notification_groups;
mod m20260701_000065_add_notification_policies;
mod m20260701_000066_hydrate_remote_threads;

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
            Box::new(m20260701_000039_add_remote_media_preview_metadata::Migration),
            Box::new(m20260701_000040_create_remote_custom_emoji::Migration),
            Box::new(m20260701_000041_add_remote_direct_conversations::Migration),
            Box::new(m20260701_000042_create_local_conversation_remote_participant::Migration),
            Box::new(m20260701_000043_add_follow_request_notifications::Migration),
            Box::new(m20260701_000044_add_direct_status_recipients::Migration),
            Box::new(m20260701_000045_constrain_status_visibility::Migration),
            Box::new(m20260701_000046_harden_inbox_replay::Migration),
            Box::new(m20260701_000047_add_remote_actor_followers_url::Migration),
            Box::new(m20260701_000048_create_streaming_event::Migration),
            Box::new(m20260701_000049_create_remote_account_moderation::Migration),
            Box::new(m20260701_000050_allow_status_update_streaming_event::Migration),
            Box::new(m20260701_000051_add_remote_follow_options::Migration),
            Box::new(m20260701_000052_allow_status_notifications::Migration),
            Box::new(m20260701_000053_add_status_edit_delivery::Migration),
            Box::new(m20260701_000054_add_streaming_status_metadata::Migration),
            Box::new(m20260701_000055_create_status_edit_history::Migration),
            Box::new(m20260701_000056_create_remote_status_tag::Migration),
            Box::new(m20260701_000057_create_status_quotes::Migration),
            Box::new(m20260701_000058_create_status_pins::Migration),
            Box::new(m20260701_000059_create_featured_tags::Migration),
            Box::new(m20260701_000060_create_push_subscription::Migration),
            Box::new(m20260701_000061_constrain_pkce_method::Migration),
            Box::new(m20260701_000062_create_lists::Migration),
            Box::new(m20260701_000063_constrain_inbox_activity_type::Migration),
            Box::new(m20260701_000064_add_notification_groups::Migration),
            Box::new(m20260701_000065_add_notification_policies::Migration),
            Box::new(m20260701_000066_hydrate_remote_threads::Migration),
        ]
    }
}
