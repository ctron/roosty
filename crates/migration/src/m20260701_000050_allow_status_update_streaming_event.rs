use sea_orm_migration::prelude::*;

/// Allows Mastodon's distinct `status.update` event in the streaming coordination log.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                ALTER TABLE streaming_event
                    DROP CONSTRAINT streaming_event_kind;
                ALTER TABLE streaming_event
                    ADD CONSTRAINT streaming_event_kind
                    CHECK (event_kind IN (
                        'update', 'status_update', 'notification', 'conversation', 'delete'
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
                DELETE FROM streaming_event WHERE event_kind = 'status_update';
                ALTER TABLE streaming_event
                    DROP CONSTRAINT streaming_event_kind;
                ALTER TABLE streaming_event
                    ADD CONSTRAINT streaming_event_kind
                    CHECK (event_kind IN ('update', 'notification', 'conversation', 'delete'));
                "#,
            )
            .await?;
        Ok(())
    }
}
