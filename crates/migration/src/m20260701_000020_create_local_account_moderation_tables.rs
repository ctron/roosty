use sea_orm_migration::prelude::*;

/// Creates local account mute and block relationships.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE TABLE IF NOT EXISTS local_account_block (
                    id uuid NOT NULL UNIQUE,
                    account_id uuid NOT NULL REFERENCES local_account(id) ON DELETE CASCADE,
                    target_account_id uuid NOT NULL REFERENCES local_account(id) ON DELETE CASCADE,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    updated_at timestamptz NOT NULL DEFAULT now(),
                    PRIMARY KEY (account_id, target_account_id),
                    CHECK (account_id <> target_account_id)
                );

                CREATE TABLE IF NOT EXISTS local_account_mute (
                    id uuid NOT NULL UNIQUE,
                    account_id uuid NOT NULL REFERENCES local_account(id) ON DELETE CASCADE,
                    target_account_id uuid NOT NULL REFERENCES local_account(id) ON DELETE CASCADE,
                    notifications boolean NOT NULL DEFAULT true,
                    expires_at timestamptz,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    updated_at timestamptz NOT NULL DEFAULT now(),
                    PRIMARY KEY (account_id, target_account_id),
                    CHECK (account_id <> target_account_id)
                );

                CREATE INDEX IF NOT EXISTS local_account_block_account_cursor_idx
                    ON local_account_block(account_id, id DESC);
                CREATE INDEX IF NOT EXISTS local_account_mute_account_cursor_idx
                    ON local_account_mute(account_id, id DESC);
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
                DROP TABLE IF EXISTS local_account_mute;
                DROP TABLE IF EXISTS local_account_block;
                "#,
            )
            .await?;

        Ok(())
    }
}
