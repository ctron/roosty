use sea_orm_migration::prelude::*;

/// Adds profile settings and posting defaults to local accounts.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                ALTER TABLE local_account
                    ADD COLUMN IF NOT EXISTS display_name text NOT NULL DEFAULT '',
                    ADD COLUMN IF NOT EXISTS note text NOT NULL DEFAULT '',
                    ADD COLUMN IF NOT EXISTS locked boolean NOT NULL DEFAULT false,
                    ADD COLUMN IF NOT EXISTS bot boolean NOT NULL DEFAULT false,
                    ADD COLUMN IF NOT EXISTS discoverable boolean NOT NULL DEFAULT true,
                    ADD COLUMN IF NOT EXISTS default_visibility text NOT NULL DEFAULT 'public',
                    ADD COLUMN IF NOT EXISTS default_sensitive boolean NOT NULL DEFAULT false,
                    ADD COLUMN IF NOT EXISTS default_language text DEFAULT 'en',
                    ADD COLUMN IF NOT EXISTS default_quote_policy text NOT NULL DEFAULT 'followers',
                    ADD COLUMN IF NOT EXISTS profile_fields jsonb NOT NULL DEFAULT '[]'::jsonb;
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
                ALTER TABLE local_account
                    DROP COLUMN IF EXISTS profile_fields,
                    DROP COLUMN IF EXISTS default_quote_policy,
                    DROP COLUMN IF EXISTS default_language,
                    DROP COLUMN IF EXISTS default_sensitive,
                    DROP COLUMN IF EXISTS default_visibility,
                    DROP COLUMN IF EXISTS discoverable,
                    DROP COLUMN IF EXISTS bot,
                    DROP COLUMN IF EXISTS locked,
                    DROP COLUMN IF EXISTS note,
                    DROP COLUMN IF EXISTS display_name;
                "#,
            )
            .await?;

        Ok(())
    }
}
