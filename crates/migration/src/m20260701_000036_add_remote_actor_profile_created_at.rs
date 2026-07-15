use sea_orm_migration::prelude::*;

/// Preserves the optional profile creation date declared by remote ActivityPub actors.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(RemoteActor::Table)
                    .add_column(
                        ColumnDef::new(RemoteActor::ProfileCreatedAt).timestamp_with_time_zone(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(RemoteActor::Table)
                    .drop_column(RemoteActor::ProfileCreatedAt)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum RemoteActor {
    Table,
    ProfileCreatedAt,
}
