use sea_orm_migration::prelude::*;

/// Stores Mastodon-compatible delivery preferences for local follows of remote actors.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                ALTER TABLE remote_following
                    ADD COLUMN show_reblogs boolean NOT NULL DEFAULT true,
                    ADD COLUMN notify boolean NOT NULL DEFAULT false;
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
                ALTER TABLE remote_following
                    DROP COLUMN IF EXISTS notify,
                    DROP COLUMN IF EXISTS show_reblogs;
                "#,
            )
            .await?;
        Ok(())
    }
}
