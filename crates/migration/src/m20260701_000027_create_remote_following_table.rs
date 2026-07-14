use sea_orm_migration::prelude::*;

/// Stores a local actor's pending or accepted follow of a remote actor.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
            CREATE TABLE remote_following (
                id uuid PRIMARY KEY,
                local_account_id uuid NOT NULL REFERENCES local_account(id) ON DELETE CASCADE,
                remote_actor_id uuid NOT NULL REFERENCES remote_actor(id) ON DELETE CASCADE,
                activity_id text NOT NULL UNIQUE,
                state text NOT NULL CHECK (state IN ('pending', 'accepted')),
                created_at timestamptz NOT NULL DEFAULT now(),
                updated_at timestamptz NOT NULL DEFAULT now(),
                UNIQUE(local_account_id, remote_actor_id)
            );
        "#,
            )
            .await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS remote_following;")
            .await?;
        Ok(())
    }
}
