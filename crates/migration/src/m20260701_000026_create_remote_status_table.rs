use sea_orm_migration::prelude::*;

/// Caches public and unlisted ActivityPub Notes received from remote actors.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE TABLE remote_status (
                    id uuid PRIMARY KEY,
                    activitypub_id text NOT NULL UNIQUE,
                    remote_actor_id uuid NOT NULL REFERENCES remote_actor(id) ON DELETE CASCADE,
                    content text NOT NULL,
                    visibility text NOT NULL CHECK (visibility IN ('public', 'unlisted')),
                    published_at timestamptz NOT NULL,
                    updated_at timestamptz NOT NULL,
                    deleted_at timestamptz,
                    object jsonb NOT NULL,
                    created_at timestamptz NOT NULL DEFAULT now()
                );
                CREATE INDEX remote_status_actor_idx ON remote_status(remote_actor_id, id DESC)
                    WHERE deleted_at IS NULL;
            "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS remote_status;")
            .await?;
        Ok(())
    }
}
