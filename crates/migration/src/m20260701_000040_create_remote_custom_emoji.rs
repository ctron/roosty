use sea_orm_migration::prelude::*;

/// Caches remote custom emoji assets and actor emoji metadata.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
            ALTER TABLE remote_actor ADD COLUMN IF NOT EXISTS emojis jsonb NOT NULL DEFAULT '[]'::jsonb;
            CREATE TABLE remote_custom_emoji (
                id uuid PRIMARY KEY,
                shortcode text NOT NULL,
                remote_url text NOT NULL UNIQUE,
                content_type text,
                state text NOT NULL CHECK (state IN ('pending', 'ready', 'failed')),
                file_path text,
                file_size bigint,
                fetched_at timestamptz,
                expires_at timestamptz,
                last_error text,
                created_at timestamptz NOT NULL DEFAULT now(),
                updated_at timestamptz NOT NULL DEFAULT now()
            );
            CREATE INDEX remote_custom_emoji_expiry_idx ON remote_custom_emoji(expires_at);
        "#).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
            DROP TABLE IF EXISTS remote_custom_emoji;
            ALTER TABLE remote_actor DROP COLUMN IF EXISTS emojis;
        "#,
            )
            .await?;
        Ok(())
    }
}
