use sea_orm_migration::prelude::*;

/// Index unresolved reply URLs and queue bounded repair for existing cached Notes.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE INDEX remote_status_unresolved_parent_idx
                    ON remote_status(in_reply_to)
                    WHERE in_reply_to IS NOT NULL
                      AND in_reply_to_local_status_id IS NULL
                      AND in_reply_to_remote_status_id IS NULL
                      AND deleted_at IS NULL;

                WITH unresolved AS (
                    SELECT id, row_number() OVER (ORDER BY created_at, id) AS position
                    FROM remote_status
                    WHERE in_reply_to IS NOT NULL
                      AND in_reply_to_local_status_id IS NULL
                      AND in_reply_to_remote_status_id IS NULL
                      AND deleted_at IS NULL
                )
                INSERT INTO job (id, kind, payload, deduplication_key, run_after)
                SELECT
                    md5('thread-resolve:' || id::text)::uuid,
                    'federation_thread_resolve',
                    jsonb_build_object('status_id', id::text),
                    'thread-resolve:' || id::text,
                    now() + (((position - 1) / 10) * interval '1 second')
                FROM unresolved;

                WITH reply_sources AS (
                    SELECT id, row_number() OVER (ORDER BY created_at, id) AS position
                    FROM remote_status
                    WHERE object ? 'replies'
                      AND object->'replies' <> 'null'::jsonb
                      AND visibility IN ('public', 'unlisted')
                      AND deleted_at IS NULL
                )
                INSERT INTO job (id, kind, payload, deduplication_key, run_after)
                SELECT
                    md5('replies-fetch:' || id::text)::uuid,
                    'federation_replies_fetch',
                    jsonb_build_object('status_id', id::text),
                    'replies-fetch:' || id::text,
                    now() + (((position - 1) / 10) * interval '1 second')
                FROM reply_sources;
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
                DELETE FROM job
                WHERE kind IN ('federation_thread_resolve', 'federation_replies_fetch',
                               'federation_reply_fetch');
                DROP INDEX IF EXISTS remote_status_unresolved_parent_idx;
                "#,
            )
            .await?;
        Ok(())
    }
}
