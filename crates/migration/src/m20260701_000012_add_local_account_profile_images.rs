use sea_orm_migration::prelude::*;

/// Adds local profile image paths for avatar and header uploads.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                ALTER TABLE local_account
                    ADD COLUMN IF NOT EXISTS avatar_file_path text,
                    ADD COLUMN IF NOT EXISTS header_file_path text;
                "#,
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                ALTER TABLE local_account
                    DROP COLUMN IF EXISTS header_file_path,
                    DROP COLUMN IF EXISTS avatar_file_path;
                "#,
            )
            .await?;

        Ok(())
    }
}
