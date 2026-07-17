use sea_orm_migration::prelude::*;

/// Creates immutable local and remote status revision snapshots.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
            ALTER TABLE remote_media_attachment ADD COLUMN status_order integer;
            WITH ranked AS (
                SELECT id, row_number() OVER (
                    PARTITION BY remote_status_id ORDER BY created_at, id
                ) - 1 AS status_order
                FROM remote_media_attachment
            )
            UPDATE remote_media_attachment
                SET status_order = ranked.status_order
                FROM ranked
                WHERE remote_media_attachment.id = ranked.id;
            ALTER TABLE remote_media_attachment ALTER COLUMN status_order SET NOT NULL;
            CREATE UNIQUE INDEX remote_media_attachment_status_order_idx
                ON remote_media_attachment(remote_status_id, status_order);

            CREATE TABLE local_status_edit (
                id uuid PRIMARY KEY,
                local_status_id uuid NOT NULL REFERENCES local_status(id) ON DELETE CASCADE,
                content text NOT NULL,
                spoiler_text text NOT NULL,
                sensitive boolean NOT NULL,
                local_mention_ids jsonb NOT NULL DEFAULT '[]'::jsonb,
                remote_mention_ids jsonb NOT NULL DEFAULT '[]'::jsonb,
                tag_names jsonb NOT NULL DEFAULT '[]'::jsonb,
                created_at timestamptz NOT NULL,
                UNIQUE (local_status_id, created_at)
            );
            CREATE INDEX local_status_edit_history_idx ON local_status_edit(local_status_id, created_at, id);
            CREATE TABLE local_status_edit_media (
                id uuid PRIMARY KEY,
                local_status_edit_id uuid NOT NULL REFERENCES local_status_edit(id) ON DELETE CASCADE,
                local_media_attachment_id uuid NOT NULL REFERENCES local_media_attachment(id) ON DELETE RESTRICT,
                status_order integer NOT NULL,
                content_type text NOT NULL,
                file_path text NOT NULL,
                preview_file_path text,
                description text,
                focus_x double precision,
                focus_y double precision,
                width integer,
                height integer,
                preview_width integer,
                preview_height integer,
                blurhash text,
                UNIQUE (local_status_edit_id, status_order)
            );
            CREATE TABLE remote_status_edit (
                id uuid PRIMARY KEY,
                remote_status_id uuid NOT NULL REFERENCES remote_status(id) ON DELETE CASCADE,
                content text NOT NULL,
                spoiler_text text NOT NULL,
                sensitive boolean NOT NULL,
                object jsonb NOT NULL,
                created_at timestamptz NOT NULL,
                UNIQUE (remote_status_id, created_at)
            );
            CREATE INDEX remote_status_edit_history_idx ON remote_status_edit(remote_status_id, created_at, id);
            CREATE TABLE remote_status_edit_media (
                id uuid PRIMARY KEY,
                remote_status_edit_id uuid NOT NULL REFERENCES remote_status_edit(id) ON DELETE CASCADE,
                source_attachment_id uuid,
                status_order integer NOT NULL,
                remote_url text NOT NULL,
                content_type text,
                file_path text,
                preview_file_path text,
                description text,
                width integer,
                height integer,
                preview_width integer,
                preview_height integer,
                blurhash text,
                UNIQUE (remote_status_edit_id, status_order)
            );
        "#).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
            DROP TABLE IF EXISTS remote_status_edit_media;
            DROP TABLE IF EXISTS remote_status_edit;
            DROP TABLE IF EXISTS local_status_edit_media;
            DROP TABLE IF EXISTS local_status_edit;
            DROP INDEX IF EXISTS remote_media_attachment_status_order_idx;
            ALTER TABLE remote_media_attachment DROP COLUMN status_order;
        "#,
            )
            .await?;
        Ok(())
    }
}
