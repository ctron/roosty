use sea_orm_migration::prelude::*;

/// Allows notifications for new posts by accounts followed with `notify=true`.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
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
                DELETE FROM local_notification WHERE notification_type = 'status';
                ALTER TABLE local_notification
                    DROP CONSTRAINT IF EXISTS local_notification_notification_type_check,
                    DROP CONSTRAINT IF EXISTS local_notification_status_check,
                    ADD CONSTRAINT local_notification_notification_type_check
                        CHECK (notification_type IN (
                            'mention', 'favourite', 'follow', 'follow_request', 'reblog'
                        )),
                    ADD CONSTRAINT local_notification_status_check
                        CHECK (
                            (notification_type IN ('mention', 'favourite', 'reblog')
                                AND ((status_id IS NULL) <> (remote_status_id IS NULL)))
                            OR (notification_type IN ('follow', 'follow_request')
                                AND status_id IS NULL AND remote_status_id IS NULL)
                        );
                "#,
            )
            .await?;
        Ok(())
    }
}
