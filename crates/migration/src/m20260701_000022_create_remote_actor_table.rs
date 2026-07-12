use sea_orm_migration::prelude::*;

/// Caches validated ActivityPub actor documents discovered over HTTPS.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
            CREATE TABLE IF NOT EXISTS remote_actor (
                id uuid PRIMARY KEY,
                activitypub_id text NOT NULL UNIQUE,
                username text NOT NULL,
                domain text NOT NULL,
                display_name text NOT NULL DEFAULT '',
                summary text NOT NULL DEFAULT '',
                inbox_url text NOT NULL,
                shared_inbox_url text,
                public_key_id text NOT NULL,
                public_key_pem text NOT NULL,
                fetched_at timestamptz NOT NULL DEFAULT now(),
                expires_at timestamptz NOT NULL,
                created_at timestamptz NOT NULL DEFAULT now(),
                updated_at timestamptz NOT NULL DEFAULT now()
            );
            CREATE INDEX IF NOT EXISTS remote_actor_handle_idx ON remote_actor(username, domain);
        "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS remote_actor;")
            .await?;
        Ok(())
    }
}
