use sea_orm_migration::prelude::*;

/// Creates local account bookmarks for statuses.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE TABLE IF NOT EXISTS local_status_bookmark (
                    account_id uuid NOT NULL REFERENCES local_account(id) ON DELETE CASCADE,
                    status_id uuid NOT NULL REFERENCES local_status(id) ON DELETE CASCADE,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    PRIMARY KEY (account_id, status_id)
                );

                CREATE INDEX IF NOT EXISTS local_status_bookmark_status_idx
                    ON local_status_bookmark(status_id);
                "#,
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS local_status_bookmark;")
            .await?;

        Ok(())
    }
}
