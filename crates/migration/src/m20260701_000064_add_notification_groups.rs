use sea_orm_migration::prelude::*;

/// Persists Mastodon notification group identity and the current rolling group per target.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
            CREATE TABLE local_notification_group_state (
                account_id uuid NOT NULL REFERENCES local_account(id) ON DELETE CASCADE,
                notification_type text NOT NULL CHECK (notification_type IN ('favourite', 'follow', 'reblog')),
                target_kind text NOT NULL CHECK (target_kind IN ('account', 'local_status', 'remote_status')),
                target_id uuid NOT NULL,
                group_id uuid NOT NULL,
                started_at timestamptz NOT NULL,
                updated_at timestamptz NOT NULL,
                PRIMARY KEY (account_id, notification_type, target_kind, target_id)
            );
            ALTER TABLE local_notification ADD COLUMN group_id uuid;

            CREATE TEMP TABLE notification_group_backfill AS
            WITH eligible AS (
                SELECT notification.*,
                    CASE WHEN notification.notification_type = 'follow' THEN 'account'
                         WHEN notification.status_id IS NOT NULL THEN 'local_status'
                         ELSE 'remote_status' END AS target_kind,
                    COALESCE(notification.status_id, notification.remote_status_id, notification.account_id) AS target_id,
                    min(notification.created_at) OVER (
                        PARTITION BY notification.account_id, notification.notification_type,
                            CASE WHEN notification.notification_type = 'follow' THEN 'account'
                                 WHEN notification.status_id IS NOT NULL THEN 'local_status'
                                 ELSE 'remote_status' END,
                            COALESCE(notification.status_id, notification.remote_status_id, notification.account_id)
                    ) AS anchor
                FROM local_notification notification
                WHERE notification.notification_type IN ('favourite', 'follow', 'reblog')
            ), bucketed AS (
                SELECT eligible.*,
                    floor(extract(epoch FROM (created_at - anchor)) / 43200)::bigint AS bucket
                FROM eligible
            ), assigned AS (
                SELECT bucketed.*,
                    first_value(id) OVER (
                        PARTITION BY account_id, notification_type, target_kind, target_id, bucket
                        ORDER BY created_at, id
                    ) AS assigned_group_id
                FROM bucketed
            ) SELECT * FROM assigned;

            UPDATE local_notification notification
            SET group_id = assigned.assigned_group_id
            FROM notification_group_backfill assigned WHERE notification.id = assigned.id;

            INSERT INTO local_notification_group_state (
                account_id, notification_type, target_kind, target_id, group_id, started_at, updated_at
            )
            SELECT DISTINCT ON (account_id, notification_type, target_kind, target_id)
                account_id, notification_type, target_kind, target_id, assigned_group_id,
                min(created_at) OVER (PARTITION BY assigned_group_id),
                max(created_at) OVER (PARTITION BY assigned_group_id)
            FROM notification_group_backfill assigned
            ORDER BY account_id, notification_type, target_kind, target_id, created_at DESC, id DESC;
            DROP TABLE notification_group_backfill;

            CREATE INDEX local_notification_group_cursor_idx
                ON local_notification(account_id, group_id, id DESC) WHERE dismissed_at IS NULL;
            CREATE INDEX local_notification_remote_actor_cursor_idx
                ON local_notification(account_id, remote_actor_id, id DESC) WHERE dismissed_at IS NULL;
        "#).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
            DROP INDEX IF EXISTS local_notification_remote_actor_cursor_idx;
            DROP INDEX IF EXISTS local_notification_group_cursor_idx;
            ALTER TABLE local_notification DROP COLUMN IF EXISTS group_id;
            DROP TABLE IF EXISTS local_notification_group_state;
        "#,
            )
            .await?;
        Ok(())
    }
}
