use sea_orm_migration::prelude::*;

/// Adds PostgreSQL trigram documents used by Mastodon-compatible status search.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE EXTENSION IF NOT EXISTS pg_trgm;

                CREATE TABLE status_search_document (
                    id uuid PRIMARY KEY,
                    local_status_id uuid UNIQUE REFERENCES local_status(id) ON DELETE CASCADE,
                    remote_status_id uuid UNIQUE REFERENCES remote_status(id) ON DELETE CASCADE,
                    document text NOT NULL,
                    updated_at timestamptz NOT NULL DEFAULT now(),
                    CHECK ((local_status_id IS NULL) <> (remote_status_id IS NULL))
                );

                INSERT INTO status_search_document
                    (id, local_status_id, document, updated_at)
                SELECT gen_random_uuid(), status.id,
                       concat_ws(
                           ' ',
                           nullif(status.spoiler_text, ''),
                           nullif(status.content, ''),
                           media.descriptions
                       ),
                       status.updated_at
                FROM local_status status
                LEFT JOIN LATERAL (
                    SELECT string_agg(nullif(attachment.description, ''), ' ')
                        AS descriptions
                    FROM local_media_attachment attachment
                    WHERE attachment.status_id = status.id
                ) media ON true
                WHERE status.deleted_at IS NULL;

                INSERT INTO status_search_document
                    (id, remote_status_id, document, updated_at)
                SELECT gen_random_uuid(), status.id,
                       concat_ws(
                           ' ',
                           nullif(status.object->>'summary', ''),
                           nullif(regexp_replace(status.content, '<[^>]*>', ' ', 'g'), ''),
                           media.descriptions
                       ),
                       status.updated_at
                FROM remote_status status
                LEFT JOIN LATERAL (
                    SELECT string_agg(nullif(attachment.description, ''), ' ')
                        AS descriptions
                    FROM remote_media_attachment attachment
                    WHERE attachment.remote_status_id = status.id
                ) media ON true
                WHERE status.deleted_at IS NULL;

                CREATE INDEX status_search_document_trgm_idx
                    ON status_search_document USING gin (document gin_trgm_ops);
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS status_search_document;")
            .await?;
        Ok(())
    }
}
