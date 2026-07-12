use sea_orm_migration::prelude::*;

/// Creates local hashtag tables for Mastodon-compatible tag timelines.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE TABLE IF NOT EXISTS local_tag (
                    id uuid NOT NULL PRIMARY KEY,
                    name text NOT NULL UNIQUE,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    updated_at timestamptz NOT NULL DEFAULT now(),
                    CHECK (name <> '')
                );

                CREATE TABLE IF NOT EXISTS local_status_tag (
                    status_id uuid NOT NULL REFERENCES local_status(id) ON DELETE CASCADE,
                    tag_id uuid NOT NULL REFERENCES local_tag(id) ON DELETE CASCADE,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    PRIMARY KEY (status_id, tag_id)
                );

                CREATE INDEX IF NOT EXISTS local_tag_name_prefix_idx
                    ON local_tag(name text_pattern_ops);
                CREATE INDEX IF NOT EXISTS local_status_tag_tag_status_idx
                    ON local_status_tag(tag_id, status_id DESC);
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
                DROP TABLE IF EXISTS local_status_tag;
                DROP TABLE IF EXISTS local_tag;
                "#,
            )
            .await?;

        Ok(())
    }
}
