use sea_orm_migration::prelude::*;

/// Allows a notification actor to be either a local account or a remote actor.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
            ALTER TABLE local_notification DROP CONSTRAINT IF EXISTS local_notification_actor_account_id_fkey;
            ALTER TABLE local_notification ALTER COLUMN actor_account_id DROP NOT NULL;
            ALTER TABLE local_notification ADD COLUMN IF NOT EXISTS remote_actor_id uuid REFERENCES remote_actor(id) ON DELETE CASCADE;
            ALTER TABLE local_notification DROP CONSTRAINT IF EXISTS local_notification_actor_kind_check;
            ALTER TABLE local_notification ADD CONSTRAINT local_notification_actor_kind_check CHECK ((actor_account_id IS NULL) <> (remote_actor_id IS NULL));
        "#).await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE local_notification DROP CONSTRAINT IF EXISTS local_notification_actor_kind_check; ALTER TABLE local_notification DROP CONSTRAINT IF EXISTS local_notification_remote_actor_id_fkey; ALTER TABLE local_notification DROP COLUMN IF EXISTS remote_actor_id; ALTER TABLE local_notification ALTER COLUMN actor_account_id SET NOT NULL;",
            )
            .await?;
        Ok(())
    }
}
