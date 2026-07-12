use sea_orm_migration::prelude::*;

/// Creates local Mastodon-compatible notification records.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE TABLE IF NOT EXISTS local_notification (
                    id uuid NOT NULL PRIMARY KEY,
                    account_id uuid NOT NULL REFERENCES local_account(id) ON DELETE CASCADE,
                    notification_type text NOT NULL,
                    actor_account_id uuid NOT NULL REFERENCES local_account(id) ON DELETE CASCADE,
                    status_id uuid REFERENCES local_status(id) ON DELETE CASCADE,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    dismissed_at timestamptz,
                    CHECK (notification_type IN ('mention', 'favourite', 'follow')),
                    CHECK (
                        (notification_type IN ('mention', 'favourite') AND status_id IS NOT NULL)
                        OR (notification_type = 'follow' AND status_id IS NULL)
                    )
                );

                CREATE UNIQUE INDEX IF NOT EXISTS local_notification_unique_event_idx
                    ON local_notification(
                        account_id,
                        notification_type,
                        actor_account_id,
                        COALESCE(status_id, '00000000-0000-0000-0000-000000000000'::uuid)
                    );
                CREATE INDEX IF NOT EXISTS local_notification_account_cursor_idx
                    ON local_notification(account_id, id DESC)
                    WHERE dismissed_at IS NULL;
                CREATE INDEX IF NOT EXISTS local_notification_status_idx
                    ON local_notification(status_id)
                    WHERE status_id IS NOT NULL;
                "#,
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS local_notification;")
            .await?;

        Ok(())
    }
}
