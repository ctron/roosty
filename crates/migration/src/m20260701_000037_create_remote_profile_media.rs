use sea_orm_migration::prelude::*;

/// Creates the actor-owned cache for remote profile avatars and headers.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE TABLE remote_profile_media (
                    id uuid PRIMARY KEY,
                    remote_actor_id uuid NOT NULL REFERENCES remote_actor(id) ON DELETE CASCADE,
                    kind text NOT NULL CHECK (kind IN ('avatar', 'header')),
                    remote_url text NOT NULL,
                    content_type text,
                    state text NOT NULL CHECK (state IN ('pending', 'ready', 'failed')),
                    file_path text,
                    file_size bigint,
                    fetched_at timestamptz,
                    expires_at timestamptz,
                    last_error text,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    updated_at timestamptz NOT NULL DEFAULT now(),
                    UNIQUE(remote_actor_id, kind)
                );
                CREATE INDEX remote_profile_media_expiry_idx ON remote_profile_media(expires_at);
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS remote_profile_media;")
            .await?;
        Ok(())
    }
}
