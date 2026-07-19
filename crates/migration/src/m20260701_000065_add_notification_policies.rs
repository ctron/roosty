use sea_orm_migration::prelude::*;

/// Adds Mastodon notification policies, filtered requests, and account limitation state.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
            CREATE TABLE local_notification_policy (
                account_id uuid PRIMARY KEY REFERENCES local_account(id) ON DELETE CASCADE,
                for_not_following text NOT NULL DEFAULT 'accept',
                for_not_followers text NOT NULL DEFAULT 'accept',
                for_new_accounts text NOT NULL DEFAULT 'accept',
                for_private_mentions text NOT NULL DEFAULT 'filter',
                for_limited_accounts text NOT NULL DEFAULT 'filter',
                updated_at timestamptz NOT NULL DEFAULT now(),
                CONSTRAINT local_notification_policy_values CHECK (
                    for_not_following IN ('accept', 'filter', 'drop') AND
                    for_not_followers IN ('accept', 'filter', 'drop') AND
                    for_new_accounts IN ('accept', 'filter', 'drop') AND
                    for_private_mentions IN ('accept', 'filter', 'drop') AND
                    for_limited_accounts IN ('accept', 'filter', 'drop')
                )
            );
            INSERT INTO local_notification_policy (account_id)
                SELECT id FROM local_account;
            CREATE FUNCTION create_default_local_notification_policy() RETURNS trigger AS $$
            BEGIN
                INSERT INTO local_notification_policy (account_id) VALUES (NEW.id);
                RETURN NEW;
            END;
            $$ LANGUAGE plpgsql;
            CREATE TRIGGER local_account_notification_policy
                AFTER INSERT ON local_account FOR EACH ROW
                EXECUTE FUNCTION create_default_local_notification_policy();

            ALTER TABLE local_account ADD COLUMN limited_at timestamptz;
            ALTER TABLE remote_actor ADD COLUMN limited_at timestamptz;

            CREATE TABLE local_notification_permission (
                id uuid PRIMARY KEY,
                account_id uuid NOT NULL REFERENCES local_account(id) ON DELETE CASCADE,
                actor_account_id uuid REFERENCES local_account(id) ON DELETE CASCADE,
                remote_actor_id uuid REFERENCES remote_actor(id) ON DELETE CASCADE,
                created_at timestamptz NOT NULL DEFAULT now(),
                CONSTRAINT local_notification_permission_actor CHECK (
                    (actor_account_id IS NULL) <> (remote_actor_id IS NULL)
                )
            );
            CREATE UNIQUE INDEX local_notification_permission_local_idx
                ON local_notification_permission(account_id, actor_account_id)
                WHERE actor_account_id IS NOT NULL;
            CREATE UNIQUE INDEX local_notification_permission_remote_idx
                ON local_notification_permission(account_id, remote_actor_id)
                WHERE remote_actor_id IS NOT NULL;

            CREATE TABLE local_notification_request (
                id uuid PRIMARY KEY,
                account_id uuid NOT NULL REFERENCES local_account(id) ON DELETE CASCADE,
                actor_account_id uuid REFERENCES local_account(id) ON DELETE CASCADE,
                remote_actor_id uuid REFERENCES remote_actor(id) ON DELETE CASCADE,
                last_status_id uuid REFERENCES local_status(id) ON DELETE SET NULL,
                last_remote_status_id uuid REFERENCES remote_status(id) ON DELETE SET NULL,
                state text NOT NULL DEFAULT 'pending',
                created_at timestamptz NOT NULL DEFAULT now(),
                updated_at timestamptz NOT NULL DEFAULT now(),
                CONSTRAINT local_notification_request_actor CHECK (
                    (actor_account_id IS NULL) <> (remote_actor_id IS NULL)
                ),
                CONSTRAINT local_notification_request_status CHECK (
                    NOT (last_status_id IS NOT NULL AND last_remote_status_id IS NOT NULL)
                ),
                CONSTRAINT local_notification_request_state CHECK (
                    state IN ('pending', 'merging', 'dismissed')
                )
            );
            CREATE UNIQUE INDEX local_notification_request_active_local_idx
                ON local_notification_request(account_id, actor_account_id)
                WHERE actor_account_id IS NOT NULL AND state IN ('pending', 'merging');
            CREATE UNIQUE INDEX local_notification_request_active_remote_idx
                ON local_notification_request(account_id, remote_actor_id)
                WHERE remote_actor_id IS NOT NULL AND state IN ('pending', 'merging');
            CREATE INDEX local_notification_request_cursor_idx
                ON local_notification_request(account_id, id DESC) WHERE state = 'pending';

            ALTER TABLE local_notification ADD COLUMN filtered boolean NOT NULL DEFAULT false;
            ALTER TABLE local_notification ADD COLUMN notification_request_id uuid
                REFERENCES local_notification_request(id) ON DELETE SET NULL;
            CREATE INDEX local_notification_filtered_cursor_idx
                ON local_notification(account_id, filtered, id DESC) WHERE dismissed_at IS NULL;
            CREATE INDEX local_notification_request_notifications_idx
                ON local_notification(notification_request_id, id DESC)
                WHERE notification_request_id IS NOT NULL;

            ALTER TABLE streaming_event DROP CONSTRAINT streaming_event_kind;
            ALTER TABLE streaming_event ADD CONSTRAINT streaming_event_kind CHECK (
                event_kind IN (
                    'update', 'status_update', 'notification', 'conversation', 'delete',
                    'notifications_merged'
                )
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
            DELETE FROM streaming_event WHERE event_kind = 'notifications_merged';
            ALTER TABLE streaming_event DROP CONSTRAINT streaming_event_kind;
            ALTER TABLE streaming_event ADD CONSTRAINT streaming_event_kind CHECK (
                event_kind IN ('update', 'status_update', 'notification', 'conversation', 'delete')
            );
            DROP INDEX IF EXISTS local_notification_request_notifications_idx;
            DROP INDEX IF EXISTS local_notification_filtered_cursor_idx;
            ALTER TABLE local_notification DROP COLUMN IF EXISTS notification_request_id;
            ALTER TABLE local_notification DROP COLUMN IF EXISTS filtered;
            DROP TABLE IF EXISTS local_notification_request;
            DROP TABLE IF EXISTS local_notification_permission;
            ALTER TABLE remote_actor DROP COLUMN IF EXISTS limited_at;
            DROP TRIGGER IF EXISTS local_account_notification_policy ON local_account;
            DROP FUNCTION IF EXISTS create_default_local_notification_policy();
            DROP TABLE IF EXISTS local_notification_policy;
            ALTER TABLE local_account DROP COLUMN IF EXISTS limited_at;
        "#,
            )
            .await?;
        Ok(())
    }
}
