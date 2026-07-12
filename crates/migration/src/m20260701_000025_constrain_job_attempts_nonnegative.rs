use sea_orm_migration::prelude::*;

/// Prevent invalid negative retry counts in durable jobs.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE job ADD CONSTRAINT job_attempts_nonnegative CHECK (attempts >= 0);",
            )
            .await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE job DROP CONSTRAINT IF EXISTS job_attempts_nonnegative;",
            )
            .await?;
        Ok(())
    }
}
