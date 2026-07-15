use sea_orm_migration::prelude::*;

/// Allows cached direct Notes to participate in local conversation projections.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(
            "ALTER TABLE local_conversation ADD COLUMN IF NOT EXISTS last_remote_status_id uuid REFERENCES remote_status(id) ON DELETE SET NULL; ALTER TABLE remote_status ADD COLUMN IF NOT EXISTS conversation_id uuid REFERENCES local_conversation(id) ON DELETE SET NULL; CREATE INDEX IF NOT EXISTS remote_status_conversation_idx ON remote_status(conversation_id) WHERE conversation_id IS NOT NULL;",
        ).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(
            "DROP INDEX IF EXISTS remote_status_conversation_idx; ALTER TABLE remote_status DROP COLUMN IF EXISTS conversation_id; ALTER TABLE local_conversation DROP COLUMN IF EXISTS last_remote_status_id;",
        ).await?;
        Ok(())
    }
}
