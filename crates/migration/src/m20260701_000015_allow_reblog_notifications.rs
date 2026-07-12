use sea_orm_migration::prelude::*;

/// Allows local boost notifications in the notification type constraints.
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
                    DROP CONSTRAINT IF EXISTS local_notification_check,
                    ADD CONSTRAINT local_notification_notification_type_check
                        CHECK (notification_type IN ('mention', 'favourite', 'follow', 'reblog')),
                    ADD CONSTRAINT local_notification_status_check
                        CHECK (
                            (notification_type IN ('mention', 'favourite', 'reblog') AND status_id IS NOT NULL)
                            OR (notification_type = 'follow' AND status_id IS NULL)
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
                ALTER TABLE local_notification
                    DROP CONSTRAINT IF EXISTS local_notification_notification_type_check,
                    DROP CONSTRAINT IF EXISTS local_notification_status_check,
                    ADD CONSTRAINT local_notification_notification_type_check
                        CHECK (notification_type IN ('mention', 'favourite', 'follow')),
                    ADD CONSTRAINT local_notification_check
                        CHECK (
                            (notification_type IN ('mention', 'favourite') AND status_id IS NOT NULL)
                            OR (notification_type = 'follow' AND status_id IS NULL)
                        );
                "#,
            )
            .await?;

        Ok(())
    }
}
