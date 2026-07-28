use sea_orm_migration::prelude::*;

/// Adds Rust-maintained preview-card metadata, link usage, and cluster fetch coordination.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE TABLE preview_card (
                    id uuid PRIMARY KEY,
                    url text NOT NULL UNIQUE,
                    title text NOT NULL DEFAULT '',
                    description text NOT NULL DEFAULT '',
                    author_name text NOT NULL DEFAULT '',
                    author_url text NOT NULL DEFAULT '',
                    provider_name text NOT NULL DEFAULT '',
                    provider_url text NOT NULL DEFAULT '',
                    image_file_path text,
                    image_width integer NOT NULL DEFAULT 0 CHECK (image_width >= 0),
                    image_height integer NOT NULL DEFAULT 0 CHECK (image_height >= 0),
                    blurhash text,
                    published_at timestamptz,
                    fetch_state text NOT NULL DEFAULT 'pending'
                        CHECK (fetch_state IN ('pending', 'ready', 'failed')),
                    fetched_at timestamptz,
                    updated_at timestamptz NOT NULL DEFAULT now()
                );

                CREATE TABLE status_preview_scan (
                    id uuid PRIMARY KEY,
                    local_status_id uuid REFERENCES local_status(id) ON DELETE CASCADE,
                    remote_status_id uuid REFERENCES remote_status(id) ON DELETE CASCADE,
                    scanned_at timestamptz NOT NULL DEFAULT now(),
                    CHECK ((local_status_id IS NULL) <> (remote_status_id IS NULL)),
                    UNIQUE NULLS NOT DISTINCT (local_status_id, remote_status_id)
                );

                CREATE TABLE status_preview_card (
                    id uuid PRIMARY KEY,
                    local_status_id uuid REFERENCES local_status(id) ON DELETE CASCADE,
                    remote_status_id uuid REFERENCES remote_status(id) ON DELETE CASCADE,
                    preview_card_id uuid NOT NULL REFERENCES preview_card(id) ON DELETE CASCADE,
                    usage_day date NOT NULL,
                    actor_origin text NOT NULL CHECK (actor_origin IN ('local', 'remote')),
                    actor_id uuid NOT NULL,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    CHECK ((local_status_id IS NULL) <> (remote_status_id IS NULL)),
                    UNIQUE NULLS NOT DISTINCT (local_status_id, remote_status_id)
                );
                CREATE INDEX status_preview_card_card_day_actor_idx
                    ON status_preview_card(preview_card_id, usage_day, actor_origin, actor_id);

                CREATE TABLE link_daily_usage (
                    preview_card_id uuid NOT NULL REFERENCES preview_card(id) ON DELETE CASCADE,
                    usage_day date NOT NULL,
                    uses bigint NOT NULL CHECK (uses >= 0),
                    accounts bigint NOT NULL CHECK (accounts >= 0),
                    updated_at timestamptz NOT NULL DEFAULT now(),
                    PRIMARY KEY (preview_card_id, usage_day)
                );
                CREATE INDEX link_daily_usage_retention_idx ON link_daily_usage(usage_day);

                CREATE TABLE link_trend (
                    preview_card_id uuid PRIMARY KEY REFERENCES preview_card(id) ON DELETE CASCADE,
                    score double precision NOT NULL CHECK (score >= 0),
                    peak_score double precision NOT NULL CHECK (peak_score >= 0),
                    peak_at timestamptz NOT NULL,
                    expires_at timestamptz NOT NULL,
                    updated_at timestamptz NOT NULL DEFAULT now()
                );
                CREATE INDEX link_trend_rank_idx
                    ON link_trend(score DESC, preview_card_id)
                    WHERE score >= 1;
                CREATE INDEX link_trend_expires_idx ON link_trend(expires_at);

                CREATE TABLE preview_fetch_host (
                    host text PRIMARY KEY,
                    lease_until timestamptz,
                    next_fetch_at timestamptz NOT NULL DEFAULT now(),
                    updated_at timestamptz NOT NULL DEFAULT now()
                );

                ALTER TABLE trend_dirty DROP CONSTRAINT trend_dirty_kind_check;
                ALTER TABLE trend_dirty ADD CONSTRAINT trend_dirty_kind_check
                    CHECK (kind IN ('local_status', 'remote_status', 'tag', 'link'));

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
                DELETE FROM job WHERE kind IN ('preview_card_fetch', 'preview_card_backfill');
                ALTER TABLE trend_dirty DROP CONSTRAINT trend_dirty_kind_check;
                ALTER TABLE trend_dirty ADD CONSTRAINT trend_dirty_kind_check
                    CHECK (kind IN ('local_status', 'remote_status', 'tag'));
                DROP TABLE IF EXISTS preview_fetch_host;
                DROP TABLE IF EXISTS link_trend;
                DROP TABLE IF EXISTS link_daily_usage;
                DROP TABLE IF EXISTS status_preview_card;
                DROP TABLE IF EXISTS status_preview_scan;
                DROP TABLE IF EXISTS preview_card;
                "#,
            )
            .await?;
        Ok(())
    }
}
