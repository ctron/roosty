use sea_orm_migration::prelude::*;

/// Stores the validated followers collection declared by a remote actor.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(RemoteActor::Table)
                    .add_column(ColumnDef::new(RemoteActor::FollowersUrl).text())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(RemoteActor::Table)
                    .drop_column(RemoteActor::FollowersUrl)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum RemoteActor {
    Table,
    FollowersUrl,
}
