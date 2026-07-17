use sea_orm_migration::prelude::*;

/// Indexes hashtags attached to cached remote statuses in the shared tag namespace.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE TABLE remote_status_tag (
                    remote_status_id uuid NOT NULL REFERENCES remote_status(id) ON DELETE CASCADE,
                    tag_id uuid NOT NULL REFERENCES local_tag(id) ON DELETE CASCADE,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    PRIMARY KEY (remote_status_id, tag_id)
                );

                CREATE INDEX remote_status_tag_tag_status_idx
                    ON remote_status_tag(tag_id, remote_status_id DESC);

                WITH remote_tags AS (
                    SELECT DISTINCT
                        lower(trim(leading '#' FROM tag->>'name')) AS name
                    FROM remote_status AS status
                    CROSS JOIN LATERAL jsonb_array_elements(
                        CASE WHEN jsonb_typeof(status.object->'tag') = 'array'
                            THEN status.object->'tag' ELSE '[]'::jsonb END
                    ) AS tag
                    WHERE status.deleted_at IS NULL
                        AND tag->>'type' IN (
                            'Hashtag',
                            'https://www.w3.org/ns/activitystreams#Hashtag'
                        )
                ), valid_tags AS (
                    SELECT name
                    FROM remote_tags
                    WHERE name <> '' AND name !~ '[^[:alnum:]_]'
                )
                INSERT INTO local_tag (id, name, created_at, updated_at)
                SELECT (
                    substr(md5(name), 1, 8) || '-' ||
                    substr(md5(name), 9, 4) || '-' ||
                    substr(md5(name), 13, 4) || '-' ||
                    substr(md5(name), 17, 4) || '-' ||
                    substr(md5(name), 21, 12)
                )::uuid, name, now(), now()
                FROM valid_tags
                ON CONFLICT (name) DO NOTHING;

                WITH remote_tags AS (
                    SELECT DISTINCT
                        status.id AS remote_status_id,
                        lower(trim(leading '#' FROM tag->>'name')) AS name
                    FROM remote_status AS status
                    CROSS JOIN LATERAL jsonb_array_elements(
                        CASE WHEN jsonb_typeof(status.object->'tag') = 'array'
                            THEN status.object->'tag' ELSE '[]'::jsonb END
                    ) AS tag
                    WHERE status.deleted_at IS NULL
                        AND tag->>'type' IN (
                            'Hashtag',
                            'https://www.w3.org/ns/activitystreams#Hashtag'
                        )
                )
                INSERT INTO remote_status_tag (remote_status_id, tag_id, created_at)
                SELECT remote_tags.remote_status_id, local_tag.id, now()
                FROM remote_tags
                JOIN local_tag ON local_tag.name = remote_tags.name
                WHERE remote_tags.name <> ''
                    AND remote_tags.name !~ '[^[:alnum:]_]'
                ON CONFLICT DO NOTHING;
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE remote_status_tag;")
            .await?;
        Ok(())
    }
}
