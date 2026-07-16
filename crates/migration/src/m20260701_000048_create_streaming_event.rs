use sea_orm_migration::prelude::*;

/// Creates the short-lived coordination log used by streaming processes.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE TABLE streaming_event (
                    sequence bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
                    origin_process_id uuid NOT NULL,
                    event_kind text NOT NULL,
                    payload text NOT NULL,
                    account_id uuid NOT NULL,
                    recipient_ids jsonb NOT NULL DEFAULT '[]'::jsonb,
                    visibility text NOT NULL,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    CONSTRAINT streaming_event_kind
                        CHECK (event_kind IN ('update', 'notification', 'conversation', 'delete')),
                    CONSTRAINT streaming_event_recipient_ids_array
                        CHECK (jsonb_typeof(recipient_ids) = 'array'),
                    CONSTRAINT streaming_event_visibility
                        CHECK (visibility IN ('public', 'unlisted', 'private', 'direct'))
                );
                CREATE INDEX streaming_event_created_at_idx
                    ON streaming_event (created_at);
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE streaming_event")
            .await?;
        Ok(())
    }
}
