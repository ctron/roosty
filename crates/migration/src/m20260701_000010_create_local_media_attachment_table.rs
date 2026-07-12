use sea_orm_migration::prelude::*;

/// Creates local media attachments that can be attached to local statuses.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE TABLE IF NOT EXISTS local_media_attachment (
                    id uuid PRIMARY KEY,
                    account_id uuid NOT NULL REFERENCES local_account(id) ON DELETE CASCADE,
                    status_id uuid REFERENCES local_status(id) ON DELETE SET NULL,
                    status_order integer NOT NULL DEFAULT 0,
                    content_type text NOT NULL,
                    original_filename text NOT NULL,
                    file_path text NOT NULL,
                    preview_file_path text,
                    file_size bigint NOT NULL,
                    description text,
                    focus_x double precision,
                    focus_y double precision,
                    width integer,
                    height integer,
                    preview_width integer,
                    preview_height integer,
                    blurhash text,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    updated_at timestamptz NOT NULL DEFAULT now()
                );

                CREATE INDEX IF NOT EXISTS local_media_attachment_account_idx
                    ON local_media_attachment(account_id, created_at DESC);

                CREATE INDEX IF NOT EXISTS local_media_attachment_status_idx
                    ON local_media_attachment(status_id);
                "#,
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS local_media_attachment;")
            .await?;

        Ok(())
    }
}
