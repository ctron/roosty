use sea_orm_migration::prelude::*;

/// Adds bounded keyset indexes for public profile and status sitemap generation.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE INDEX local_account_search_discovery_idx
                    ON local_account(id) INCLUDE (username, updated_at)
                    WHERE discoverable
                      AND limited_at IS NULL
                      AND suspended_at IS NULL
                      AND data_purged_at IS NULL;
                CREATE INDEX local_status_search_discovery_idx
                    ON local_status(id) INCLUDE (account_id, updated_at)
                    WHERE visibility = 'public'
                      AND NOT sensitive
                      AND deleted_at IS NULL;
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
                DROP INDEX IF EXISTS local_status_search_discovery_idx;
                DROP INDEX IF EXISTS local_account_search_discovery_idx;
                "#,
            )
            .await?;
        Ok(())
    }
}
