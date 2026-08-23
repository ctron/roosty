use sea_orm_migration::prelude::*;

/// Adds partial indexes used by bounded durable-job retention cleanup.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE INDEX job_successful_cleanup_idx
                    ON job (completed_at)
                    WHERE completed_at IS NOT NULL
                      AND permanently_failed_at IS NULL;
                CREATE INDEX job_permanently_failed_cleanup_idx
                    ON job (permanently_failed_at)
                    WHERE permanently_failed_at IS NOT NULL;
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
                DROP INDEX IF EXISTS job_permanently_failed_cleanup_idx;
                DROP INDEX IF EXISTS job_successful_cleanup_idx;
                "#,
            )
            .await?;
        Ok(())
    }
}
