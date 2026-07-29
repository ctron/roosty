use sea_orm_migration::prelude::*;

/// Adds reversible account suspension and durable Mastodon-compatible domain moderation.
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
                    ADD COLUMN suspended_at timestamptz,
                    ADD COLUMN data_purged_at timestamptz;
                ALTER TABLE remote_actor
                    ADD COLUMN suspended_at timestamptz,
                    ADD COLUMN data_purged_at timestamptz;

                CREATE INDEX local_account_suspended_idx
                    ON local_account(suspended_at) WHERE suspended_at IS NOT NULL;
                CREATE INDEX remote_actor_suspended_idx
                    ON remote_actor(suspended_at) WHERE suspended_at IS NOT NULL;

                CREATE TABLE federation_domain_block (
                    id uuid PRIMARY KEY,
                    domain text NOT NULL UNIQUE,
                    severity text NOT NULL DEFAULT 'silence',
                    reject_media boolean NOT NULL DEFAULT false,
                    reject_reports boolean NOT NULL DEFAULT false,
                    private_comment text,
                    public_comment text,
                    obfuscate boolean NOT NULL DEFAULT false,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    updated_at timestamptz NOT NULL DEFAULT now(),
                    CONSTRAINT federation_domain_block_domain_valid
                        CHECK (domain = lower(domain)
                            AND domain <> ''
                            AND domain !~ '[/@:*\s]'),
                    CONSTRAINT federation_domain_block_severity_valid
                        CHECK (severity IN ('noop', 'silence', 'suspend'))
                );
                CREATE INDEX federation_domain_block_cursor_idx
                    ON federation_domain_block(id DESC);
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
                DROP TABLE IF EXISTS federation_domain_block;
                DROP INDEX IF EXISTS remote_actor_suspended_idx;
                DROP INDEX IF EXISTS local_account_suspended_idx;
                ALTER TABLE remote_actor
                    DROP COLUMN IF EXISTS data_purged_at,
                    DROP COLUMN IF EXISTS suspended_at;
                ALTER TABLE local_account
                    DROP COLUMN IF EXISTS data_purged_at,
                    DROP COLUMN IF EXISTS suspended_at;
                "#,
            )
            .await?;
        Ok(())
    }
}
