use sea_orm_migration::prelude::*;

/// Adds configurable instance rules and the durable moderation report workflow.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE TABLE instance_rule (
                    id uuid PRIMARY KEY,
                    text text NOT NULL,
                    position integer NOT NULL,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    updated_at timestamptz NOT NULL DEFAULT now(),
                    discarded_at timestamptz,
                    CONSTRAINT instance_rule_text_valid
                        CHECK (text <> '' AND char_length(text) <= 300),
                    CONSTRAINT instance_rule_position_valid CHECK (position >= 0)
                );
                CREATE UNIQUE INDEX instance_rule_active_position_idx
                    ON instance_rule(position) WHERE discarded_at IS NULL;

                CREATE TABLE moderation_report (
                    id uuid PRIMARY KEY,
                    source_local_account_id uuid REFERENCES local_account(id) ON DELETE SET NULL,
                    source_remote_actor_id uuid REFERENCES remote_actor(id) ON DELETE SET NULL,
                    target_local_account_id uuid REFERENCES local_account(id) ON DELETE SET NULL,
                    target_remote_actor_id uuid REFERENCES remote_actor(id) ON DELETE SET NULL,
                    category text NOT NULL DEFAULT 'other',
                    comment text NOT NULL DEFAULT '',
                    forwarded boolean NOT NULL DEFAULT false,
                    activitypub_id text UNIQUE,
                    assigned_account_id uuid REFERENCES local_account(id) ON DELETE SET NULL,
                    action_taken_by_account_id uuid REFERENCES local_account(id) ON DELETE SET NULL,
                    action_taken_at timestamptz,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    updated_at timestamptz NOT NULL DEFAULT now(),
                    CONSTRAINT moderation_report_source_valid CHECK (
                        (source_local_account_id IS NULL) <> (source_remote_actor_id IS NULL)
                    ),
                    CONSTRAINT moderation_report_target_valid CHECK (
                        (target_local_account_id IS NULL) <> (target_remote_actor_id IS NULL)
                    ),
                    CONSTRAINT moderation_report_category_valid CHECK (
                        category IN ('spam', 'legal', 'violation', 'other')
                    ),
                    CONSTRAINT moderation_report_comment_valid CHECK (char_length(comment) <= 1000),
                    CONSTRAINT moderation_report_resolution_valid CHECK (
                        (action_taken_at IS NULL) = (action_taken_by_account_id IS NULL)
                    )
                );
                CREATE INDEX moderation_report_unresolved_cursor_idx
                    ON moderation_report(id DESC) WHERE action_taken_at IS NULL;
                CREATE INDEX moderation_report_resolved_cursor_idx
                    ON moderation_report(id DESC) WHERE action_taken_at IS NOT NULL;
                CREATE INDEX moderation_report_source_local_idx
                    ON moderation_report(source_local_account_id, id DESC);
                CREATE INDEX moderation_report_source_remote_idx
                    ON moderation_report(source_remote_actor_id, id DESC);
                CREATE INDEX moderation_report_target_local_idx
                    ON moderation_report(target_local_account_id, id DESC);
                CREATE INDEX moderation_report_target_remote_idx
                    ON moderation_report(target_remote_actor_id, id DESC);

                CREATE TABLE moderation_report_status (
                    report_id uuid NOT NULL REFERENCES moderation_report(id) ON DELETE CASCADE,
                    local_status_id uuid REFERENCES local_status(id) ON DELETE SET NULL,
                    remote_status_id uuid REFERENCES remote_status(id) ON DELETE SET NULL,
                    position integer NOT NULL,
                    PRIMARY KEY (report_id, position),
                    CONSTRAINT moderation_report_status_target_valid CHECK (
                        (local_status_id IS NULL) <> (remote_status_id IS NULL)
                    )
                );
                CREATE UNIQUE INDEX moderation_report_local_status_idx
                    ON moderation_report_status(report_id, local_status_id)
                    WHERE local_status_id IS NOT NULL;
                CREATE UNIQUE INDEX moderation_report_remote_status_idx
                    ON moderation_report_status(report_id, remote_status_id)
                    WHERE remote_status_id IS NOT NULL;

                CREATE TABLE moderation_report_rule (
                    report_id uuid NOT NULL REFERENCES moderation_report(id) ON DELETE CASCADE,
                    rule_id uuid REFERENCES instance_rule(id) ON DELETE SET NULL,
                    rule_text text NOT NULL,
                    position integer NOT NULL,
                    PRIMARY KEY (report_id, position)
                );
                CREATE UNIQUE INDEX moderation_report_rule_unique_idx
                    ON moderation_report_rule(report_id, rule_id)
                    WHERE rule_id IS NOT NULL;

                ALTER TABLE local_notification
                    ADD COLUMN report_id uuid REFERENCES moderation_report(id) ON DELETE CASCADE,
                    DROP CONSTRAINT IF EXISTS local_notification_notification_type_check,
                    DROP CONSTRAINT IF EXISTS local_notification_status_check,
                    ADD CONSTRAINT local_notification_notification_type_check
                        CHECK (notification_type IN (
                            'mention', 'favourite', 'follow', 'follow_request', 'reblog',
                            'status', 'update', 'quote', 'quoted_update', 'admin.report'
                        )),
                    ADD CONSTRAINT local_notification_status_check
                        CHECK (
                            (notification_type IN (
                                'mention', 'favourite', 'reblog', 'status', 'update',
                                'quote', 'quoted_update'
                            ) AND ((status_id IS NULL) <> (remote_status_id IS NULL))
                                AND report_id IS NULL)
                            OR (notification_type IN ('follow', 'follow_request')
                                AND status_id IS NULL AND remote_status_id IS NULL
                                AND report_id IS NULL)
                            OR (notification_type = 'admin.report'
                                AND status_id IS NULL AND remote_status_id IS NULL
                                AND report_id IS NOT NULL)
                        );
                CREATE UNIQUE INDEX local_notification_admin_report_idx
                    ON local_notification(account_id, report_id)
                    WHERE notification_type = 'admin.report';
                DROP INDEX IF EXISTS local_notification_unique_event_idx;
                CREATE UNIQUE INDEX local_notification_unique_event_idx
                    ON local_notification(
                        account_id,
                        notification_type,
                        actor_account_id,
                        COALESCE(status_id, '00000000-0000-0000-0000-000000000000'::uuid)
                    ) WHERE notification_type <> 'admin.report';
                DROP INDEX IF EXISTS local_notification_remote_actor_event_idx;
                CREATE UNIQUE INDEX local_notification_remote_actor_event_idx
                    ON local_notification(account_id, notification_type, remote_actor_id)
                    WHERE remote_actor_id IS NOT NULL
                        AND status_id IS NULL
                        AND remote_status_id IS NULL
                        AND notification_type <> 'admin.report';

                ALTER TABLE processed_inbox_activity
                    DROP CONSTRAINT IF EXISTS processed_inbox_activity_type_check,
                    ADD CONSTRAINT processed_inbox_activity_type_check
                    CHECK (activity_type IS NULL OR activity_type IN (
                        'Follow', 'Accept', 'Reject', 'Create', 'Update', 'Delete',
                        'Like', 'Announce', 'Undo', 'Move', 'Block', 'Add', 'Remove',
                        'Flag', 'https://w3id.org/fep/044f#QuoteRequest'
                    ));
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
                DELETE FROM local_notification WHERE notification_type = 'admin.report';
                DELETE FROM processed_inbox_activity WHERE activity_type = 'Flag';
                ALTER TABLE processed_inbox_activity
                    DROP CONSTRAINT IF EXISTS processed_inbox_activity_type_check,
                    ADD CONSTRAINT processed_inbox_activity_type_check
                    CHECK (activity_type IS NULL OR activity_type IN (
                        'Follow', 'Accept', 'Reject', 'Create', 'Update', 'Delete',
                        'Like', 'Announce', 'Undo', 'Move', 'Block', 'Add', 'Remove',
                        'https://w3id.org/fep/044f#QuoteRequest'
                    ));
                DROP INDEX IF EXISTS local_notification_admin_report_idx;
                DROP INDEX IF EXISTS local_notification_unique_event_idx;
                CREATE UNIQUE INDEX local_notification_unique_event_idx
                    ON local_notification(
                        account_id,
                        notification_type,
                        actor_account_id,
                        COALESCE(status_id, '00000000-0000-0000-0000-000000000000'::uuid)
                    );
                DROP INDEX IF EXISTS local_notification_remote_actor_event_idx;
                CREATE UNIQUE INDEX local_notification_remote_actor_event_idx
                    ON local_notification(account_id, notification_type, remote_actor_id)
                    WHERE remote_actor_id IS NOT NULL
                        AND status_id IS NULL
                        AND remote_status_id IS NULL;
                ALTER TABLE local_notification
                    DROP CONSTRAINT IF EXISTS local_notification_notification_type_check,
                    DROP CONSTRAINT IF EXISTS local_notification_status_check,
                    DROP COLUMN IF EXISTS report_id,
                    ADD CONSTRAINT local_notification_notification_type_check
                        CHECK (notification_type IN (
                            'mention', 'favourite', 'follow', 'follow_request', 'reblog',
                            'status', 'update', 'quote', 'quoted_update'
                        )),
                    ADD CONSTRAINT local_notification_status_check
                        CHECK (
                            (notification_type IN (
                                'mention', 'favourite', 'reblog', 'status', 'update',
                                'quote', 'quoted_update'
                            ) AND ((status_id IS NULL) <> (remote_status_id IS NULL)))
                            OR (notification_type IN ('follow', 'follow_request')
                                AND status_id IS NULL AND remote_status_id IS NULL)
                        );
                DROP TABLE IF EXISTS moderation_report_rule;
                DROP TABLE IF EXISTS moderation_report_status;
                DROP TABLE IF EXISTS moderation_report;
                DROP TABLE IF EXISTS instance_rule;
                "#,
            )
            .await?;
        Ok(())
    }
}
