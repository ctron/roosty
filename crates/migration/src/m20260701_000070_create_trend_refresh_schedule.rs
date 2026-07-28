use sea_orm_migration::prelude::*;

/// Adds the singleton schedule claimed by trend workers across all instances.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE TABLE trend_refresh_schedule (
                    id smallint PRIMARY KEY CHECK (id = 1),
                    interval_milliseconds bigint NOT NULL
                        CHECK (interval_milliseconds >= 60000),
                    next_run_at timestamptz NOT NULL,
                    updated_at timestamptz NOT NULL DEFAULT now()
                );
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS trend_refresh_schedule")
            .await?;
        Ok(())
    }
}
