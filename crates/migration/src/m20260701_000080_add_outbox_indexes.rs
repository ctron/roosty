use sea_orm_migration::prelude::*;

/// Adds cursor and signature-key indexes used by requester-aware ActivityPub outboxes.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE INDEX local_status_outbox_cursor_idx
                    ON local_status(account_id, id DESC)
                    WHERE deleted_at IS NULL;
                CREATE INDEX local_status_reblog_account_cursor_idx
                    ON local_status_reblog(account_id, id DESC);
                CREATE INDEX remote_actor_public_key_id_idx
                    ON remote_actor(public_key_id);
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
                DROP INDEX remote_actor_public_key_id_idx;
                DROP INDEX local_status_reblog_account_cursor_idx;
                DROP INDEX local_status_outbox_cursor_idx;
                "#,
            )
            .await?;
        Ok(())
    }
}
