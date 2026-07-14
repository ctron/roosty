use sea_orm_migration::prelude::*;

/// Retains canonical reply targets for cached remote Notes.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(
            "ALTER TABLE remote_status ADD COLUMN in_reply_to text; \
             ALTER TABLE remote_status ADD COLUMN in_reply_to_local_status_id uuid REFERENCES local_status(id) ON DELETE SET NULL; \
             ALTER TABLE remote_status ADD COLUMN in_reply_to_remote_status_id uuid REFERENCES remote_status(id) ON DELETE SET NULL;",
        ).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE remote_status DROP COLUMN IF EXISTS in_reply_to_remote_status_id; \
             ALTER TABLE remote_status DROP COLUMN IF EXISTS in_reply_to_local_status_id; \
             ALTER TABLE remote_status DROP COLUMN IF EXISTS in_reply_to;",
            )
            .await?;
        Ok(())
    }
}
