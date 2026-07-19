use sea_orm_migration::prelude::*;

/// Reject unsupported PKCE methods at the authorization-code persistence boundary.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE oauth_authorization_code \
                 ADD CONSTRAINT oauth_authorization_code_pkce_method_check \
                 CHECK (code_challenge_method IN ('', 'S256'));",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE oauth_authorization_code \
                 DROP CONSTRAINT IF EXISTS oauth_authorization_code_pkce_method_check;",
            )
            .await?;
        Ok(())
    }
}
