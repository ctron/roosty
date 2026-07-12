use sea_orm_migration::prelude::*;

/// Creates persisted Mastodon timeline positions for local accounts.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE TABLE IF NOT EXISTS local_timeline_marker (
                    account_id uuid NOT NULL REFERENCES local_account(id) ON DELETE CASCADE,
                    timeline text NOT NULL CHECK (timeline IN ('home', 'notifications')),
                    last_read_id uuid NOT NULL,
                    version bigint NOT NULL CHECK (version > 0),
                    updated_at timestamptz NOT NULL DEFAULT now(),
                    PRIMARY KEY (account_id, timeline)
                );
                "#,
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS local_timeline_marker;")
            .await?;

        Ok(())
    }
}
