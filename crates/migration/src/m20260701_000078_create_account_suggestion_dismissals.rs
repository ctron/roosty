use sea_orm_migration::prelude::*;

/// Persists per-account suppression of local and cached-remote follow suggestions.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE TABLE local_account_suggestion_dismissal (
                    id uuid PRIMARY KEY,
                    account_id uuid NOT NULL REFERENCES local_account(id) ON DELETE CASCADE,
                    target_account_id uuid REFERENCES local_account(id) ON DELETE CASCADE,
                    target_remote_actor_id uuid REFERENCES remote_actor(id) ON DELETE CASCADE,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    CONSTRAINT local_account_suggestion_dismissal_target_check CHECK (
                        (target_account_id IS NOT NULL)::integer
                        + (target_remote_actor_id IS NOT NULL)::integer = 1
                    )
                );
                CREATE UNIQUE INDEX local_account_suggestion_dismissal_local_idx
                    ON local_account_suggestion_dismissal(account_id, target_account_id)
                    WHERE target_account_id IS NOT NULL;
                CREATE UNIQUE INDEX local_account_suggestion_dismissal_remote_idx
                    ON local_account_suggestion_dismissal(account_id, target_remote_actor_id)
                    WHERE target_remote_actor_id IS NOT NULL;
                CREATE INDEX remote_following_suggestion_follower_idx
                    ON remote_following(remote_actor_id, local_account_id)
                    WHERE state = 'accepted' AND deactivated_at IS NULL;
                CREATE INDEX remote_follow_suggestion_follower_idx
                    ON remote_follow(local_account_id)
                    WHERE state = 'accepted';
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
                DROP INDEX remote_follow_suggestion_follower_idx;
                DROP INDEX remote_following_suggestion_follower_idx;
                DROP TABLE local_account_suggestion_dismissal;
                "#,
            )
            .await?;
        Ok(())
    }
}
