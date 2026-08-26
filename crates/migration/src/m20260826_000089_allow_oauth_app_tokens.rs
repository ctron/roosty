use sea_orm_migration::prelude::*;

/// Allows OAuth access tokens to represent an application without an authorized user.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE oauth_access_token ALTER COLUMN account_id DROP NOT NULL;",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                DELETE FROM oauth_access_token WHERE account_id IS NULL;
                ALTER TABLE oauth_access_token ALTER COLUMN account_id SET NOT NULL;
                "#,
            )
            .await?;
        Ok(())
    }
}
