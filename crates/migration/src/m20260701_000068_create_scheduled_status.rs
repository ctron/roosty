use sea_orm_migration::prelude::*;

/// Adds durable scheduled posts, reserved media, and status-creation idempotency.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE TABLE scheduled_status (
                    id uuid PRIMARY KEY,
                    account_id uuid NOT NULL REFERENCES local_account(id) ON DELETE CASCADE,
                    publication_status_id uuid NOT NULL UNIQUE,
                    content text NOT NULL,
                    visibility text NOT NULL,
                    sensitive boolean NOT NULL,
                    spoiler_text text NOT NULL,
                    language text,
                    in_reply_to_id uuid,
                    in_reply_to_remote_status_id uuid,
                    quoted_status_id uuid,
                    quote_approval_policy text NOT NULL,
                    scheduled_at timestamptz NOT NULL,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    updated_at timestamptz NOT NULL DEFAULT now(),
                    CONSTRAINT scheduled_status_visibility_check
                        CHECK (visibility IN ('public', 'unlisted', 'private', 'direct')),
                    CONSTRAINT scheduled_status_quote_policy_check
                        CHECK (quote_approval_policy IN ('public', 'followers', 'nobody'))
                );
                CREATE INDEX scheduled_status_account_id_idx
                    ON scheduled_status (account_id, id DESC);
                CREATE INDEX scheduled_status_account_date_idx
                    ON scheduled_status (account_id, scheduled_at);

                ALTER TABLE local_media_attachment
                    ADD COLUMN scheduled_status_id uuid
                        REFERENCES scheduled_status(id) ON DELETE SET NULL;
                ALTER TABLE local_media_attachment
                    ADD CONSTRAINT local_media_attachment_owner_check
                    CHECK (NOT (status_id IS NOT NULL AND scheduled_status_id IS NOT NULL));
                CREATE INDEX local_media_attachment_scheduled_status_idx
                    ON local_media_attachment (scheduled_status_id)
                    WHERE scheduled_status_id IS NOT NULL;

                CREATE TABLE status_creation_idempotency (
                    account_id uuid NOT NULL REFERENCES local_account(id) ON DELETE CASCADE,
                    key_hash text NOT NULL,
                    result_kind text NOT NULL,
                    result_id uuid NOT NULL,
                    expires_at timestamptz NOT NULL,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    PRIMARY KEY (account_id, key_hash),
                    CONSTRAINT status_creation_result_kind_check
                        CHECK (result_kind IN ('status', 'scheduled_status'))
                );
                CREATE INDEX status_creation_idempotency_expiry_idx
                    ON status_creation_idempotency (expires_at);
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
                DROP TABLE IF EXISTS status_creation_idempotency;
                ALTER TABLE local_media_attachment
                    DROP CONSTRAINT IF EXISTS local_media_attachment_owner_check;
                ALTER TABLE local_media_attachment
                    DROP COLUMN IF EXISTS scheduled_status_id;
                DROP TABLE IF EXISTS scheduled_status;
                "#,
            )
            .await?;
        Ok(())
    }
}
