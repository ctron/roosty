use sea_orm_migration::prelude::*;

/// Stores remote followers and verified ActivityPub inbox idempotency records.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
            CREATE TABLE remote_follow (
                id uuid PRIMARY KEY,
                remote_actor_id uuid NOT NULL REFERENCES remote_actor(id) ON DELETE CASCADE,
                local_account_id uuid NOT NULL REFERENCES local_account(id) ON DELETE CASCADE,
                activity_id text NOT NULL UNIQUE,
                activity jsonb NOT NULL,
                state text NOT NULL CHECK (state IN ('pending', 'accepted')),
                created_at timestamptz NOT NULL DEFAULT now(),
                updated_at timestamptz NOT NULL DEFAULT now(),
                UNIQUE(remote_actor_id, local_account_id)
            );
            CREATE INDEX remote_follow_pending_idx ON remote_follow(local_account_id, id DESC) WHERE state = 'pending';
            CREATE TABLE processed_inbox_activity (
                activity_id text PRIMARY KEY,
                remote_actor_id uuid NOT NULL REFERENCES remote_actor(id) ON DELETE CASCADE,
                processed_at timestamptz NOT NULL DEFAULT now()
            );
        "#).await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared("DROP TABLE IF EXISTS processed_inbox_activity; DROP TABLE IF EXISTS remote_follow;").await?;
        Ok(())
    }
}
