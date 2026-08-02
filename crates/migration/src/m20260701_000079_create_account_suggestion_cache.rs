use sea_orm_migration::prelude::*;

/// Creates the shared account-suggestion score materialized view.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE MATERIALIZED VIEW account_suggestion_score AS
                SELECT 'local'::text AS account_kind,
                       follow.followed_account_id AS account_id,
                       count(*)::bigint AS local_followers_count
                FROM local_follow follow
                JOIN local_account follower
                  ON follower.id = follow.follower_account_id
                 AND follower.suspended_at IS NULL
                GROUP BY follow.followed_account_id
                UNION ALL
                SELECT 'remote'::text,
                       follow.remote_actor_id,
                       count(*)::bigint
                FROM remote_following follow
                JOIN local_account follower
                  ON follower.id = follow.local_account_id
                 AND follower.suspended_at IS NULL
                WHERE follow.state = 'accepted' AND follow.deactivated_at IS NULL
                GROUP BY follow.remote_actor_id;

                CREATE UNIQUE INDEX account_suggestion_score_identity_idx
                    ON account_suggestion_score(account_kind, account_id);
                CREATE INDEX account_suggestion_score_rank_idx
                    ON account_suggestion_score(
                        account_kind, local_followers_count DESC, account_id DESC
                    );
                CREATE INDEX local_follow_suggestion_edge_idx
                    ON local_follow(
                        follower_account_id, created_at DESC, followed_account_id DESC
                    );
                CREATE INDEX remote_following_suggestion_edge_idx
                    ON remote_following(
                        local_account_id, created_at DESC, remote_actor_id DESC
                    ) WHERE state = 'accepted' AND deactivated_at IS NULL;
                CREATE INDEX remote_follow_suggestion_edge_idx
                    ON remote_follow(
                        remote_actor_id, created_at DESC, local_account_id DESC
                    ) WHERE state = 'accepted';
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
                DROP INDEX remote_follow_suggestion_edge_idx;
                DROP INDEX remote_following_suggestion_edge_idx;
                DROP INDEX local_follow_suggestion_edge_idx;
                DROP MATERIALIZED VIEW account_suggestion_score;
                "#,
            )
            .await?;
        Ok(())
    }
}
