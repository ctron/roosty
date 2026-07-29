use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE TABLE status_poll (
                    id uuid PRIMARY KEY,
                    local_status_id uuid REFERENCES local_status(id) ON DELETE CASCADE,
                    remote_status_id uuid REFERENCES remote_status(id) ON DELETE CASCADE,
                    multiple boolean NOT NULL DEFAULT false,
                    hide_totals boolean NOT NULL DEFAULT false,
                    expires_at timestamptz,
                    closed_at timestamptz,
                    notifications_sent_at timestamptz,
                    voters_count bigint,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    updated_at timestamptz NOT NULL DEFAULT now(),
                    CHECK ((local_status_id IS NOT NULL)::int + (remote_status_id IS NOT NULL)::int = 1),
                    CHECK (local_status_id IS NULL OR expires_at IS NOT NULL),
                    CHECK (voters_count IS NULL OR voters_count >= 0),
                    UNIQUE (local_status_id),
                    UNIQUE (remote_status_id)
                );

                CREATE TABLE status_poll_option (
                    poll_id uuid NOT NULL REFERENCES status_poll(id) ON DELETE CASCADE,
                    position integer NOT NULL CHECK (position >= 0),
                    title text NOT NULL,
                    votes_count bigint NOT NULL DEFAULT 0 CHECK (votes_count >= 0),
                    PRIMARY KEY (poll_id, position)
                );

                CREATE TABLE status_poll_vote (
                    id uuid PRIMARY KEY,
                    poll_id uuid NOT NULL REFERENCES status_poll(id) ON DELETE CASCADE,
                    choice_position integer NOT NULL CHECK (choice_position >= 0),
                    local_account_id uuid REFERENCES local_account(id) ON DELETE CASCADE,
                    remote_actor_id uuid REFERENCES remote_actor(id) ON DELETE CASCADE,
                    activitypub_id text,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    CHECK ((local_account_id IS NOT NULL)::int + (remote_actor_id IS NOT NULL)::int = 1),
                    FOREIGN KEY (poll_id, choice_position)
                        REFERENCES status_poll_option(poll_id, position) ON DELETE CASCADE
                );
                CREATE UNIQUE INDEX status_poll_vote_local_choice_idx
                    ON status_poll_vote(poll_id, local_account_id, choice_position)
                    WHERE local_account_id IS NOT NULL;
                CREATE UNIQUE INDEX status_poll_vote_remote_choice_idx
                    ON status_poll_vote(poll_id, remote_actor_id, choice_position)
                    WHERE remote_actor_id IS NOT NULL;
                CREATE UNIQUE INDEX status_poll_vote_activitypub_idx
                    ON status_poll_vote(activitypub_id) WHERE activitypub_id IS NOT NULL;

                CREATE TABLE scheduled_status_poll (
                    scheduled_status_id uuid PRIMARY KEY
                        REFERENCES scheduled_status(id) ON DELETE CASCADE,
                    multiple boolean NOT NULL DEFAULT false,
                    hide_totals boolean NOT NULL DEFAULT false,
                    expires_in bigint NOT NULL CHECK (expires_in >= 300 AND expires_in <= 2629746)
                );
                CREATE TABLE scheduled_status_poll_option (
                    scheduled_status_id uuid NOT NULL
                        REFERENCES scheduled_status_poll(scheduled_status_id) ON DELETE CASCADE,
                    position integer NOT NULL CHECK (position >= 0),
                    title text NOT NULL,
                    PRIMARY KEY (scheduled_status_id, position)
                );

                ALTER TABLE local_status_edit ADD COLUMN poll_options jsonb;
                ALTER TABLE remote_status_edit ADD COLUMN poll_options jsonb;

                ALTER TABLE local_notification
                    DROP CONSTRAINT IF EXISTS local_notification_notification_type_check,
                    DROP CONSTRAINT IF EXISTS local_notification_status_check,
                    ADD CONSTRAINT local_notification_notification_type_check
                        CHECK (notification_type IN (
                            'mention', 'favourite', 'follow', 'follow_request', 'reblog',
                            'status', 'update', 'quote', 'quoted_update', 'poll', 'admin.report'
                        )),
                    ADD CONSTRAINT local_notification_status_check CHECK (
                        (
                            notification_type IN (
                                'mention', 'favourite', 'reblog', 'status', 'update',
                                'quote', 'quoted_update', 'poll'
                            )
                            AND (
                                (status_id IS NOT NULL AND remote_status_id IS NULL)
                                OR (status_id IS NULL AND remote_status_id IS NOT NULL)
                            )
                            AND report_id IS NULL
                        )
                        OR (
                            notification_type IN ('follow', 'follow_request')
                            AND status_id IS NULL AND remote_status_id IS NULL
                            AND report_id IS NULL
                        )
                        OR (
                            notification_type = 'admin.report'
                            AND status_id IS NULL AND remote_status_id IS NULL
                            AND report_id IS NOT NULL
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
                DELETE FROM local_notification WHERE notification_type = 'poll';
                ALTER TABLE local_notification
                    DROP CONSTRAINT IF EXISTS local_notification_notification_type_check,
                    DROP CONSTRAINT IF EXISTS local_notification_status_check,
                    ADD CONSTRAINT local_notification_notification_type_check
                        CHECK (notification_type IN (
                            'mention', 'favourite', 'follow', 'follow_request', 'reblog',
                            'status', 'update', 'quote', 'quoted_update', 'admin.report'
                        )),
                    ADD CONSTRAINT local_notification_status_check CHECK (
                        (
                            notification_type IN (
                                'mention', 'favourite', 'reblog', 'status',
                                'update', 'quote', 'quoted_update'
                            )
                            AND (
                                (status_id IS NOT NULL AND remote_status_id IS NULL)
                                OR (status_id IS NULL AND remote_status_id IS NOT NULL)
                            )
                            AND report_id IS NULL
                        )
                        OR (
                            notification_type IN ('follow', 'follow_request')
                            AND status_id IS NULL AND remote_status_id IS NULL
                            AND report_id IS NULL
                        )
                        OR (
                            notification_type = 'admin.report'
                            AND status_id IS NULL AND remote_status_id IS NULL
                            AND report_id IS NOT NULL
                        )
                    );
                ALTER TABLE remote_status_edit DROP COLUMN poll_options;
                ALTER TABLE local_status_edit DROP COLUMN poll_options;
                DROP TABLE scheduled_status_poll_option;
                DROP TABLE scheduled_status_poll;
                DROP TABLE status_poll_vote;
                DROP TABLE status_poll_option;
                DROP TABLE status_poll;
                "#,
            )
            .await?;
        Ok(())
    }
}
