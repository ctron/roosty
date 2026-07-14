use sea_orm_migration::prelude::*;

/// Allows notifications caused by remote Notes to reference their cached status.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared("ALTER TABLE local_notification ADD COLUMN remote_status_id uuid REFERENCES remote_status(id) ON DELETE CASCADE; CREATE UNIQUE INDEX local_notification_remote_status_event_idx ON local_notification(account_id, notification_type, remote_actor_id, remote_status_id) WHERE remote_status_id IS NOT NULL;").await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared("DROP INDEX IF EXISTS local_notification_remote_status_event_idx; ALTER TABLE local_notification DROP COLUMN IF EXISTS remote_status_id;").await?;
        Ok(())
    }
}
