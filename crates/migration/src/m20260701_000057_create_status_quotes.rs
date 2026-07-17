use sea_orm_migration::prelude::*;

/// Adds consent-aware quote policy and authorization state for local and cached statuses.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
            ALTER TABLE local_status
                ADD COLUMN quote_approval_policy text NOT NULL DEFAULT 'nobody',
                ADD CONSTRAINT local_status_quote_policy_check
                    CHECK (quote_approval_policy IN ('public', 'followers', 'nobody'));
            ALTER TABLE remote_status
                ADD COLUMN quote_automatic_policy jsonb NOT NULL DEFAULT '[]'::jsonb,
                ADD COLUMN quote_manual_policy jsonb NOT NULL DEFAULT '[]'::jsonb,
                ADD CONSTRAINT remote_status_quote_automatic_array
                    CHECK (jsonb_typeof(quote_automatic_policy) = 'array'),
                ADD CONSTRAINT remote_status_quote_manual_array
                    CHECK (jsonb_typeof(quote_manual_policy) = 'array');

            CREATE TABLE status_quote (
                id uuid PRIMARY KEY,
                local_quoting_status_id uuid REFERENCES local_status(id) ON DELETE CASCADE,
                remote_quoting_status_id uuid REFERENCES remote_status(id) ON DELETE CASCADE,
                quoted_local_status_id uuid REFERENCES local_status(id) ON DELETE SET NULL,
                quoted_remote_status_id uuid REFERENCES remote_status(id) ON DELETE SET NULL,
                quoted_activitypub_id text NOT NULL,
                state text NOT NULL,
                quote_request_id text,
                authorization_id text,
                created_at timestamptz NOT NULL DEFAULT now(),
                updated_at timestamptz NOT NULL DEFAULT now(),
                CHECK ((local_quoting_status_id IS NULL) <> (remote_quoting_status_id IS NULL)),
                CHECK ((quoted_local_status_id IS NULL) OR (quoted_remote_status_id IS NULL)),
                CHECK (state IN ('pending', 'accepted', 'rejected', 'revoked', 'deleted'))
            );
            CREATE UNIQUE INDEX status_quote_local_quoting_idx
                ON status_quote(local_quoting_status_id) WHERE local_quoting_status_id IS NOT NULL;
            CREATE UNIQUE INDEX status_quote_remote_quoting_idx
                ON status_quote(remote_quoting_status_id) WHERE remote_quoting_status_id IS NOT NULL;
            CREATE INDEX status_quote_local_target_idx
                ON status_quote(quoted_local_status_id, id DESC) WHERE quoted_local_status_id IS NOT NULL;
            CREATE INDEX status_quote_remote_target_idx
                ON status_quote(quoted_remote_status_id, id DESC) WHERE quoted_remote_status_id IS NOT NULL;
            CREATE UNIQUE INDEX status_quote_request_idx
                ON status_quote(quote_request_id) WHERE quote_request_id IS NOT NULL;
            CREATE UNIQUE INDEX status_quote_authorization_idx
                ON status_quote(authorization_id) WHERE authorization_id IS NOT NULL;

            ALTER TABLE local_notification
                DROP CONSTRAINT IF EXISTS local_notification_notification_type_check,
                DROP CONSTRAINT IF EXISTS local_notification_status_check,
                ADD CONSTRAINT local_notification_notification_type_check
                    CHECK (notification_type IN (
                        'mention', 'favourite', 'follow', 'follow_request', 'reblog', 'status',
                        'update', 'quote', 'quoted_update'
                    )),
                ADD CONSTRAINT local_notification_status_check
                    CHECK (
                        (notification_type IN (
                            'mention', 'favourite', 'reblog', 'status', 'update', 'quote',
                            'quoted_update'
                        ) AND ((status_id IS NULL) <> (remote_status_id IS NULL)))
                        OR (notification_type IN ('follow', 'follow_request')
                            AND status_id IS NULL AND remote_status_id IS NULL)
                    );
        "#).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
            DELETE FROM local_notification WHERE notification_type IN ('quote', 'quoted_update');
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
            DROP TABLE status_quote;
            ALTER TABLE remote_status
                DROP CONSTRAINT remote_status_quote_manual_array,
                DROP CONSTRAINT remote_status_quote_automatic_array,
                DROP COLUMN quote_manual_policy,
                DROP COLUMN quote_automatic_policy;
            ALTER TABLE local_status
                DROP CONSTRAINT local_status_quote_policy_check,
                DROP COLUMN quote_approval_policy;
        "#,
            )
            .await?;
        Ok(())
    }
}
