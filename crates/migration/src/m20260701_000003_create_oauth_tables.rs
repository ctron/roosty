use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE TABLE IF NOT EXISTS oauth_application (
                    id uuid PRIMARY KEY,
                    client_id text NOT NULL UNIQUE,
                    client_secret_hash text NOT NULL,
                    name text NOT NULL,
                    redirect_uri text NOT NULL,
                    scopes text NOT NULL,
                    website text,
                    created_at timestamptz NOT NULL DEFAULT now()
                );

                CREATE TABLE IF NOT EXISTS oauth_authorization_code (
                    id uuid PRIMARY KEY,
                    code_hash text NOT NULL UNIQUE,
                    account_id uuid NOT NULL REFERENCES local_account(id) ON DELETE CASCADE,
                    application_id uuid NOT NULL REFERENCES oauth_application(id) ON DELETE CASCADE,
                    redirect_uri text NOT NULL,
                    scopes text NOT NULL,
                    code_challenge text NOT NULL,
                    code_challenge_method text NOT NULL,
                    expires_at timestamptz NOT NULL,
                    consumed_at timestamptz,
                    created_at timestamptz NOT NULL DEFAULT now()
                );

                CREATE TABLE IF NOT EXISTS oauth_access_token (
                    id uuid PRIMARY KEY,
                    token_hash text NOT NULL UNIQUE,
                    account_id uuid NOT NULL REFERENCES local_account(id) ON DELETE CASCADE,
                    application_id uuid NOT NULL REFERENCES oauth_application(id) ON DELETE CASCADE,
                    scopes text NOT NULL,
                    issued_at timestamptz NOT NULL DEFAULT now(),
                    expires_at timestamptz,
                    revoked_at timestamptz
                );

                CREATE TABLE IF NOT EXISTS oauth_refresh_token (
                    id uuid PRIMARY KEY,
                    token_hash text NOT NULL UNIQUE,
                    account_id uuid NOT NULL REFERENCES local_account(id) ON DELETE CASCADE,
                    application_id uuid NOT NULL REFERENCES oauth_application(id) ON DELETE CASCADE,
                    scopes text NOT NULL,
                    issued_at timestamptz NOT NULL DEFAULT now(),
                    expires_at timestamptz,
                    revoked_at timestamptz
                );

                CREATE INDEX IF NOT EXISTS oauth_authorization_code_account_idx
                    ON oauth_authorization_code(account_id);

                CREATE INDEX IF NOT EXISTS oauth_access_token_account_idx
                    ON oauth_access_token(account_id);
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
                DROP TABLE IF EXISTS oauth_refresh_token;
                DROP TABLE IF EXISTS oauth_access_token;
                DROP TABLE IF EXISTS oauth_authorization_code;
                DROP TABLE IF EXISTS oauth_application;
                "#,
            )
            .await?;

        Ok(())
    }
}
