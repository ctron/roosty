use sea_orm_migration::prelude::*;

/// Records remote handles that cannot currently be verified without changing actor identity.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE remote_actor ADD COLUMN IF NOT EXISTS invalid_handle boolean NOT NULL DEFAULT false;",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE remote_actor DROP COLUMN IF EXISTS invalid_handle;")
            .await?;
        Ok(())
    }
}
