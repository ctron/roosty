use sea_orm_migration::prelude::*;

/// Stores one Mastodon Web Push subscription per OAuth access token and enqueues delivery jobs.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE TABLE push_subscription (
                    id uuid PRIMARY KEY,
                    access_token_id uuid NOT NULL UNIQUE REFERENCES oauth_access_token(id) ON DELETE CASCADE,
                    account_id uuid NOT NULL REFERENCES local_account(id) ON DELETE CASCADE,
                    endpoint text NOT NULL,
                    p256dh bytea NOT NULL,
                    auth bytea NOT NULL,
                    standard boolean NOT NULL DEFAULT false,
                    policy text NOT NULL DEFAULT 'all' CHECK (policy IN ('all', 'followed', 'follower', 'none')),
                    alerts jsonb NOT NULL DEFAULT '{}'::jsonb,
                    access_token_ciphertext bytea NOT NULL,
                    access_token_nonce bytea NOT NULL,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    updated_at timestamptz NOT NULL DEFAULT now(),
                    CHECK (octet_length(p256dh) = 65),
                    CHECK (octet_length(auth) = 16),
                    CHECK (octet_length(access_token_nonce) = 12)
                );
                CREATE INDEX push_subscription_account_idx ON push_subscription(account_id);
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
                DROP TABLE IF EXISTS push_subscription;
                "#,
            )
            .await?;
        Ok(())
    }
}
