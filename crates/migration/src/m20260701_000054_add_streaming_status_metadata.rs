use sea_orm_migration::prelude::*;

/// Adds durable status metadata needed by federated public stream routing.
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
                    ADD COLUMN status_origin text NOT NULL DEFAULT 'local',
                    ADD COLUMN has_media boolean NOT NULL DEFAULT false,
                    ADD CONSTRAINT streaming_event_status_origin
                        CHECK (status_origin IN ('local', 'remote'));

                UPDATE streaming_event
                    SET has_media = jsonb_array_length(
                        COALESCE((payload::jsonb)->'media_attachments', '[]'::jsonb)
                    ) > 0
                    WHERE event_kind IN ('update', 'status_update')
                        AND payload LIKE '{%';
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
                ALTER TABLE streaming_event
                    DROP CONSTRAINT IF EXISTS streaming_event_status_origin,
                    DROP COLUMN has_media,
                    DROP COLUMN status_origin;
                "#,
            )
            .await?;
        Ok(())
    }
}
