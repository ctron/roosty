use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Job::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Job::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Job::Kind).text().not_null())
                    .col(ColumnDef::new(Job::Payload).json_binary().not_null())
                    .col(ColumnDef::new(Job::DeduplicationKey).text())
                    .col(
                        ColumnDef::new(Job::RunAfter)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(Job::Attempts)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(ColumnDef::new(Job::LockedAt).timestamp_with_time_zone())
                    .col(ColumnDef::new(Job::LockedBy).text())
                    .col(ColumnDef::new(Job::LastError).text())
                    .col(
                        ColumnDef::new(Job::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(ColumnDef::new(Job::CompletedAt).timestamp_with_time_zone())
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("job_run_after_idx")
                    .table(Job::Table)
                    .col(Job::RunAfter)
                    .to_owned(),
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE UNIQUE INDEX IF NOT EXISTS job_deduplication_key_idx
                    ON job (kind, deduplication_key)
                    WHERE deduplication_key IS NOT NULL
                    AND completed_at IS NULL
                "#,
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX IF EXISTS job_deduplication_key_idx")
            .await?;

        manager
            .drop_table(Table::drop().table(Job::Table).if_exists().to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Job {
    Table,
    Id,
    Kind,
    Payload,
    DeduplicationKey,
    RunAfter,
    Attempts,
    LockedAt,
    LockedBy,
    LastError,
    CreatedAt,
    CompletedAt,
}
