use sea_orm_migration::prelude::*;

/// Links locally authored replies to cached remote Notes.
#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared("ALTER TABLE local_status ADD COLUMN in_reply_to_remote_status_id uuid REFERENCES remote_status(id) ON DELETE SET NULL;").await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE local_status DROP COLUMN IF EXISTS in_reply_to_remote_status_id;",
            )
            .await?;
        Ok(())
    }
}
