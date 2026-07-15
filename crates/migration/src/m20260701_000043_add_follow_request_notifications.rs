use sea_orm_migration::prelude::*;

/// Adds Mastodon follow-request notifications for locked local accounts.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(
            "ALTER TABLE local_notification DROP CONSTRAINT IF EXISTS local_notification_notification_type_check; ALTER TABLE local_notification DROP CONSTRAINT IF EXISTS local_notification_status_check; ALTER TABLE local_notification ADD CONSTRAINT local_notification_notification_type_check CHECK (notification_type IN ('mention', 'favourite', 'follow', 'follow_request', 'reblog')); ALTER TABLE local_notification ADD CONSTRAINT local_notification_status_check CHECK ((notification_type IN ('mention', 'favourite', 'reblog') AND status_id IS NOT NULL) OR (notification_type IN ('follow', 'follow_request') AND status_id IS NULL)); CREATE UNIQUE INDEX IF NOT EXISTS local_notification_remote_actor_event_idx ON local_notification(account_id, notification_type, remote_actor_id) WHERE remote_actor_id IS NOT NULL AND status_id IS NULL AND remote_status_id IS NULL;",
        ).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(
            "DROP INDEX IF EXISTS local_notification_remote_actor_event_idx; ALTER TABLE local_notification DROP CONSTRAINT IF EXISTS local_notification_notification_type_check; ALTER TABLE local_notification DROP CONSTRAINT IF EXISTS local_notification_status_check; ALTER TABLE local_notification ADD CONSTRAINT local_notification_notification_type_check CHECK (notification_type IN ('mention', 'favourite', 'follow', 'reblog')); ALTER TABLE local_notification ADD CONSTRAINT local_notification_status_check CHECK ((notification_type IN ('mention', 'favourite', 'reblog') AND status_id IS NOT NULL) OR (notification_type = 'follow' AND status_id IS NULL));",
        ).await?;
        Ok(())
    }
}
