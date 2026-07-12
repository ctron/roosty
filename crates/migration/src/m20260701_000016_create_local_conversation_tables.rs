use sea_orm_migration::prelude::*;

/// Creates local direct-message conversation records.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE TABLE IF NOT EXISTS local_conversation (
                    id uuid NOT NULL PRIMARY KEY,
                    last_status_id uuid REFERENCES local_status(id) ON DELETE SET NULL,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    updated_at timestamptz NOT NULL DEFAULT now()
                );

                CREATE TABLE IF NOT EXISTS local_conversation_account (
                    id uuid NOT NULL PRIMARY KEY,
                    cursor_id uuid NOT NULL,
                    conversation_id uuid NOT NULL REFERENCES local_conversation(id) ON DELETE CASCADE,
                    account_id uuid NOT NULL REFERENCES local_account(id) ON DELETE CASCADE,
                    unread boolean NOT NULL DEFAULT false,
                    hidden_at timestamptz,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    updated_at timestamptz NOT NULL DEFAULT now(),
                    UNIQUE (conversation_id, account_id)
                );

                ALTER TABLE local_status
                    ADD COLUMN IF NOT EXISTS conversation_id uuid REFERENCES local_conversation(id) ON DELETE SET NULL;

                CREATE UNIQUE INDEX IF NOT EXISTS local_conversation_account_cursor_idx
                    ON local_conversation_account(cursor_id);
                CREATE INDEX IF NOT EXISTS local_conversation_account_list_idx
                    ON local_conversation_account(account_id, cursor_id DESC)
                    WHERE hidden_at IS NULL;
                CREATE INDEX IF NOT EXISTS local_conversation_account_conversation_idx
                    ON local_conversation_account(conversation_id);
                CREATE INDEX IF NOT EXISTS local_status_conversation_idx
                    ON local_status(conversation_id)
                    WHERE conversation_id IS NOT NULL;
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
                ALTER TABLE local_status DROP COLUMN IF EXISTS conversation_id;
                DROP TABLE IF EXISTS local_conversation_account;
                DROP TABLE IF EXISTS local_conversation;
                "#,
            )
            .await?;

        Ok(())
    }
}
