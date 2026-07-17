use sea_orm_migration::prelude::*;

/// Persists mention lifecycle state and Mastodon edit-delivery metadata.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE TABLE local_status_local_mention (
                    status_id uuid NOT NULL REFERENCES local_status(id) ON DELETE CASCADE,
                    account_id uuid NOT NULL REFERENCES local_account(id) ON DELETE CASCADE,
                    active boolean NOT NULL DEFAULT true,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    updated_at timestamptz NOT NULL DEFAULT now(),
                    PRIMARY KEY (status_id, account_id)
                );
                CREATE INDEX local_status_local_mention_active_account_idx
                    ON local_status_local_mention(account_id, status_id) WHERE active;

                CREATE TABLE remote_status_local_mention (
                    remote_status_id uuid NOT NULL REFERENCES remote_status(id) ON DELETE CASCADE,
                    account_id uuid NOT NULL REFERENCES local_account(id) ON DELETE CASCADE,
                    active boolean NOT NULL DEFAULT true,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    updated_at timestamptz NOT NULL DEFAULT now(),
                    PRIMARY KEY (remote_status_id, account_id)
                );
                CREATE INDEX remote_status_local_mention_active_account_idx
                    ON remote_status_local_mention(account_id, remote_status_id) WHERE active;

                INSERT INTO local_status_local_mention (status_id, account_id)
                    SELECT DISTINCT notification.status_id, notification.account_id
                    FROM local_notification notification
                    JOIN local_status status ON status.id = notification.status_id
                    WHERE notification.notification_type = 'mention'
                        AND status.updated_at = status.created_at
                    ON CONFLICT DO NOTHING;
                INSERT INTO remote_status_local_mention (remote_status_id, account_id)
                    SELECT DISTINCT notification.remote_status_id, notification.account_id
                    FROM local_notification notification
                    JOIN remote_status status ON status.id = notification.remote_status_id
                    WHERE notification.notification_type = 'mention'
                        AND status.updated_at = status.published_at
                    ON CONFLICT DO NOTHING;

                ALTER TABLE streaming_event
                    ADD COLUMN notification_recipient_ids jsonb NOT NULL DEFAULT '[]'::jsonb,
                    ADD CONSTRAINT streaming_event_notification_recipient_ids_array
                        CHECK (jsonb_typeof(notification_recipient_ids) = 'array');

                ALTER TABLE local_notification
                    DROP CONSTRAINT IF EXISTS local_notification_notification_type_check,
                    DROP CONSTRAINT IF EXISTS local_notification_status_check,
                    ADD CONSTRAINT local_notification_notification_type_check
                        CHECK (notification_type IN (
                            'mention', 'favourite', 'follow', 'follow_request', 'reblog', 'status',
                            'update'
                        )),
                    ADD CONSTRAINT local_notification_status_check
                        CHECK (
                            (notification_type IN (
                                'mention', 'favourite', 'reblog', 'status', 'update'
                            ) AND ((status_id IS NULL) <> (remote_status_id IS NULL)))
                            OR (notification_type IN ('follow', 'follow_request')
                                AND status_id IS NULL AND remote_status_id IS NULL)
                        );
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
                DELETE FROM local_notification WHERE notification_type = 'update';
                ALTER TABLE local_notification
                    DROP CONSTRAINT IF EXISTS local_notification_notification_type_check,
                    DROP CONSTRAINT IF EXISTS local_notification_status_check,
                    ADD CONSTRAINT local_notification_notification_type_check
                        CHECK (notification_type IN (
                            'mention', 'favourite', 'follow', 'follow_request', 'reblog', 'status'
                        )),
                    ADD CONSTRAINT local_notification_status_check
                        CHECK (
                            (notification_type IN ('mention', 'favourite', 'reblog', 'status')
                                AND ((status_id IS NULL) <> (remote_status_id IS NULL)))
                            OR (notification_type IN ('follow', 'follow_request')
                                AND status_id IS NULL AND remote_status_id IS NULL)
                        );
                ALTER TABLE streaming_event
                    DROP CONSTRAINT IF EXISTS streaming_event_notification_recipient_ids_array,
                    DROP COLUMN notification_recipient_ids;
                DROP TABLE remote_status_local_mention;
                DROP TABLE local_status_local_mention;
                "#,
            )
            .await?;
        Ok(())
    }
}
