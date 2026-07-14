use sea_orm_migration::prelude::*;

/// Adds a per-attempt lease token so stale workers cannot persist job outcomes.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Job::Table)
                    .add_column(ColumnDef::new(Job::ClaimId).uuid())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Job::Table)
                    .drop_column(Job::ClaimId)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Job {
    Table,
    ClaimId,
}
