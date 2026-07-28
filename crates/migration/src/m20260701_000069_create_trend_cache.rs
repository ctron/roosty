use sea_orm_migration::prelude::*;

/// Creates Rust-maintained, cross-process caches for trending tags and statuses.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE TABLE status_trend_metric (
                    local_status_id uuid REFERENCES local_status(id) ON DELETE CASCADE,
                    remote_status_id uuid REFERENCES remote_status(id) ON DELETE CASCADE,
                    favourites_count bigint NOT NULL DEFAULT 0 CHECK (favourites_count >= 0),
                    reblogs_count bigint NOT NULL DEFAULT 0 CHECK (reblogs_count >= 0),
                    published_at timestamptz NOT NULL,
                    score double precision NOT NULL DEFAULT 0 CHECK (score >= 0),
                    expires_at timestamptz,
                    updated_at timestamptz NOT NULL DEFAULT now(),
                    CHECK ((local_status_id IS NULL) <> (remote_status_id IS NULL)),
                    UNIQUE NULLS NOT DISTINCT (local_status_id, remote_status_id)
                );
                CREATE INDEX status_trend_metric_rank_idx
                    ON status_trend_metric(score DESC,
                        (coalesce(local_status_id, remote_status_id)) DESC)
                    WHERE score >= 1;
                CREATE INDEX status_trend_metric_expires_idx
                    ON status_trend_metric(expires_at)
                    WHERE expires_at IS NOT NULL;

                CREATE TABLE tag_daily_actor_usage (
                    tag_id uuid NOT NULL REFERENCES local_tag(id) ON DELETE CASCADE,
                    usage_day date NOT NULL,
                    actor_origin text NOT NULL CHECK (actor_origin IN ('local', 'remote')),
                    actor_id uuid NOT NULL,
                    uses bigint NOT NULL CHECK (uses > 0),
                    PRIMARY KEY (tag_id, usage_day, actor_origin, actor_id)
                );
                CREATE INDEX tag_daily_actor_usage_retention_idx
                    ON tag_daily_actor_usage(usage_day);

                CREATE TABLE tag_daily_usage (
                    tag_id uuid NOT NULL REFERENCES local_tag(id) ON DELETE CASCADE,
                    usage_day date NOT NULL,
                    uses bigint NOT NULL CHECK (uses >= 0),
                    accounts bigint NOT NULL CHECK (accounts >= 0),
                    updated_at timestamptz NOT NULL DEFAULT now(),
                    PRIMARY KEY (tag_id, usage_day)
                );
                CREATE INDEX tag_daily_usage_retention_idx ON tag_daily_usage(usage_day);

                CREATE TABLE tag_trend (
                    tag_id uuid PRIMARY KEY REFERENCES local_tag(id) ON DELETE CASCADE,
                    score double precision NOT NULL CHECK (score >= 0),
                    peak_score double precision NOT NULL CHECK (peak_score >= 0),
                    peak_at timestamptz NOT NULL,
                    expires_at timestamptz NOT NULL,
                    updated_at timestamptz NOT NULL DEFAULT now()
                );
                CREATE INDEX tag_trend_rank_idx ON tag_trend(score DESC, tag_id)
                    WHERE score >= 1;
                CREATE INDEX tag_trend_expires_idx ON tag_trend(expires_at);

                CREATE TABLE trend_dirty (
                    kind text NOT NULL CHECK (kind IN ('local_status', 'remote_status', 'tag')),
                    target_id uuid NOT NULL,
                    touched_at timestamptz NOT NULL DEFAULT now(),
                    PRIMARY KEY (kind, target_id)
                );
                CREATE INDEX trend_dirty_touched_idx
                    ON trend_dirty(touched_at, kind, target_id);

                CREATE INDEX local_remote_status_favourite_status_idx
                    ON local_remote_status_favourite(remote_status_id);
                CREATE INDEX local_remote_status_reblog_status_idx
                    ON local_remote_status_reblog(remote_status_id);
                CREATE INDEX remote_status_reblog_remote_status_idx
                    ON remote_status_reblog(remote_status_id)
                    WHERE remote_status_id IS NOT NULL;

                INSERT INTO tag_daily_actor_usage (
                    tag_id, usage_day, actor_origin, actor_id, uses
                )
                SELECT tag_id, usage_day, actor_origin, actor_id, count(*)
                FROM (
                    SELECT st.tag_id, (s.created_at AT TIME ZONE 'UTC')::date usage_day,
                           'local' actor_origin, s.account_id actor_id
                    FROM local_status_tag st JOIN local_status s ON s.id = st.status_id
                    WHERE s.deleted_at IS NULL AND s.visibility = 'public'
                      AND s.created_at >= now() - interval '8 days'
                    UNION ALL
                    SELECT st.tag_id, (s.published_at AT TIME ZONE 'UTC')::date,
                           'remote', s.remote_actor_id
                    FROM remote_status_tag st JOIN remote_status s
                      ON s.id = st.remote_status_id
                    WHERE s.deleted_at IS NULL AND s.visibility = 'public'
                      AND s.published_at >= now() - interval '8 days'
                ) usage
                GROUP BY tag_id, usage_day, actor_origin, actor_id;

                INSERT INTO tag_daily_usage (tag_id, usage_day, uses, accounts)
                SELECT tag_id, usage_day, sum(uses), count(*)
                FROM tag_daily_actor_usage GROUP BY tag_id, usage_day;

                WITH interactions AS (
                    SELECT status_id, 1::bigint favourites, 0::bigint reblogs
                    FROM local_status_favourite
                    UNION ALL
                    SELECT local_status_id, 1, 0 FROM remote_status_favourite
                    UNION ALL
                    SELECT status_id, 0, 1 FROM local_status_reblog
                    UNION ALL
                    SELECT local_status_id, 0, 1 FROM remote_status_reblog
                    WHERE local_status_id IS NOT NULL
                ), totals AS (
                    SELECT status_id, sum(favourites) favourites, sum(reblogs) reblogs
                    FROM interactions GROUP BY status_id
                )
                INSERT INTO status_trend_metric (
                    local_status_id, favourites_count, reblogs_count, published_at
                )
                SELECT s.id, coalesce(t.favourites, 0), coalesce(t.reblogs, 0), s.created_at
                FROM local_status s JOIN totals t ON t.status_id = s.id;

                WITH interactions AS (
                    SELECT remote_status_id, 1::bigint favourites, 0::bigint reblogs
                    FROM local_remote_status_favourite
                    UNION ALL
                    SELECT remote_status_id, 0, 1 FROM local_remote_status_reblog
                    UNION ALL
                    SELECT remote_status_id, 0, 1 FROM remote_status_reblog
                    WHERE remote_status_id IS NOT NULL
                ), totals AS (
                    SELECT remote_status_id, sum(favourites) favourites, sum(reblogs) reblogs
                    FROM interactions GROUP BY remote_status_id
                )
                INSERT INTO status_trend_metric (
                    remote_status_id, favourites_count, reblogs_count, published_at
                )
                SELECT s.id, coalesce(t.favourites, 0), coalesce(t.reblogs, 0), s.published_at
                FROM remote_status s JOIN totals t ON t.remote_status_id = s.id;

                INSERT INTO trend_dirty(kind, target_id)
                SELECT 'tag', tag_id FROM tag_daily_usage GROUP BY tag_id
                UNION ALL
                SELECT 'local_status', local_status_id FROM status_trend_metric
                    WHERE local_status_id IS NOT NULL
                UNION ALL
                SELECT 'remote_status', remote_status_id FROM status_trend_metric
                    WHERE remote_status_id IS NOT NULL;
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
                DROP TABLE IF EXISTS trend_dirty;
                DROP TABLE IF EXISTS tag_trend;
                DROP TABLE IF EXISTS tag_daily_usage;
                DROP TABLE IF EXISTS tag_daily_actor_usage;
                DROP TABLE IF EXISTS status_trend_metric;
                DROP INDEX IF EXISTS remote_status_reblog_remote_status_idx;
                DROP INDEX IF EXISTS local_remote_status_reblog_status_idx;
                DROP INDEX IF EXISTS local_remote_status_favourite_status_idx;
                "#,
            )
            .await?;
        Ok(())
    }
}
