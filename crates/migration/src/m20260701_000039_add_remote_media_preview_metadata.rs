use sea_orm_migration::prelude::*;

/// Stores generated remote-image preview dimensions for Mastodon media metadata.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(RemoteMediaAttachment::Table)
                    .add_column(ColumnDef::new(RemoteMediaAttachment::PreviewWidth).integer())
                    .add_column(ColumnDef::new(RemoteMediaAttachment::PreviewHeight).integer())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(RemoteMediaAttachment::Table)
                    .drop_column(RemoteMediaAttachment::PreviewWidth)
                    .drop_column(RemoteMediaAttachment::PreviewHeight)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum RemoteMediaAttachment {
    Table,
    PreviewWidth,
    PreviewHeight,
}
