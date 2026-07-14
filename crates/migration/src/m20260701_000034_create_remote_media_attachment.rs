use sea_orm_migration::prelude::*;

/// Creates durable metadata for locally cached remote media.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
            CREATE TABLE remote_media_attachment (
                id uuid PRIMARY KEY,
                remote_status_id uuid NOT NULL REFERENCES remote_status(id) ON DELETE CASCADE,
                remote_url text NOT NULL,
                content_type text,
                description text,
                state text NOT NULL CHECK (state IN ('pending', 'ready', 'failed')),
                file_path text,
                preview_file_path text,
                file_size bigint,
                width integer,
                height integer,
                blurhash text,
                fetched_at timestamptz,
                expires_at timestamptz,
                last_error text,
                created_at timestamptz NOT NULL DEFAULT now(),
                updated_at timestamptz NOT NULL DEFAULT now(),
                UNIQUE(remote_status_id, remote_url)
            );
            CREATE INDEX remote_media_attachment_expiry_idx ON remote_media_attachment(expires_at);
        "#,
            )
            .await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS remote_media_attachment;")
            .await?;
        Ok(())
    }
}
