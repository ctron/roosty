use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(LocalAccount::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(LocalAccount::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(LocalAccount::Username).text().not_null())
                    .col(ColumnDef::new(LocalAccount::Email).text().not_null())
                    .col(ColumnDef::new(LocalAccount::PasswordHash).text().not_null())
                    .col(
                        ColumnDef::new(LocalAccount::IsAdmin)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(LocalAccount::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(LocalAccount::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("local_account_username_idx")
                    .table(LocalAccount::Table)
                    .col(LocalAccount::Username)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("local_account_email_idx")
                    .table(LocalAccount::Table)
                    .col(LocalAccount::Email)
                    .unique()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(LocalAccount::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum LocalAccount {
    Table,
    Id,
    Username,
    Email,
    PasswordHash,
    IsAdmin,
    CreatedAt,
    UpdatedAt,
}
