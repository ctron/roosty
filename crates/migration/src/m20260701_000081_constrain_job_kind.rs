use sea_orm_migration::prelude::*;

/// Replaces the stringly typed durable-job discriminator with a PostgreSQL enum.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE TYPE job_kind AS ENUM (
                    'federation_follow_response',
                    'federation_status_delivery',
                    'federation_quote_delivery',
                    'federation_follow_delivery',
                    'federation_favourite_delivery',
                    'federation_reblog_delivery',
                    'federation_actor_update_delivery',
                    'federation_moderation_delivery',
                    'federation_remote_media_fetch',
                    'federation_featured_refresh',
                    'federation_featured_tags_refresh',
                    'federation_thread_resolve',
                    'federation_replies_fetch',
                    'federation_reply_fetch',
                    'web_push_delivery',
                    'notification_request_merge',
                    'notification_request_cleanup',
                    'account_purge',
                    'domain_moderation_reconcile',
                    'scheduled_status_publish',
                    'poll_expiration',
                    'poll_update',
                    'federation_poll_vote_delivery',
                    'trend_maintenance',
                    'account_suggestion_maintenance',
                    'preview_card_fetch',
                    'preview_card_backfill'
                );
                ALTER TABLE job
                    ALTER COLUMN kind TYPE job_kind
                    USING kind::job_kind;
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
                ALTER TABLE job
                    ALTER COLUMN kind TYPE text
                    USING kind::text;
                DROP TYPE job_kind;
                "#,
            )
            .await?;
        Ok(())
    }
}
