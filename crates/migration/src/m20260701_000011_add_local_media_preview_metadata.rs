use sea_orm_migration::prelude::*;

/// Adds preview metadata columns to already-created local media attachment tables.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                ALTER TABLE local_media_attachment
                    ADD COLUMN IF NOT EXISTS preview_file_path text,
                    ADD COLUMN IF NOT EXISTS preview_width integer,
                    ADD COLUMN IF NOT EXISTS preview_height integer,
                    ADD COLUMN IF NOT EXISTS blurhash text;
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
                ALTER TABLE local_media_attachment
                    DROP COLUMN IF EXISTS blurhash,
                    DROP COLUMN IF EXISTS preview_height,
                    DROP COLUMN IF EXISTS preview_width,
                    DROP COLUMN IF EXISTS preview_file_path;
                "#,
            )
            .await?;

        Ok(())
    }
}
