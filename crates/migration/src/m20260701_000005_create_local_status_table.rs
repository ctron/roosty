use sea_orm_migration::prelude::*;

/// Creates local statuses authored by accounts on this instance.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE TABLE IF NOT EXISTS local_status (
                    id uuid PRIMARY KEY,
                    account_id uuid NOT NULL REFERENCES local_account(id) ON DELETE CASCADE,
                    content text NOT NULL,
                    visibility text NOT NULL,
                    sensitive boolean NOT NULL DEFAULT false,
                    spoiler_text text NOT NULL DEFAULT '',
                    language text,
                    in_reply_to_id uuid REFERENCES local_status(id) ON DELETE SET NULL,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    updated_at timestamptz NOT NULL DEFAULT now(),
                    deleted_at timestamptz
                );

                CREATE INDEX IF NOT EXISTS local_status_account_idx
                    ON local_status(account_id, created_at DESC);

                CREATE INDEX IF NOT EXISTS local_status_public_timeline_idx
                    ON local_status(created_at DESC)
                    WHERE visibility = 'public' AND deleted_at IS NULL;
                "#,
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS local_status;")
            .await?;

        Ok(())
    }
}
