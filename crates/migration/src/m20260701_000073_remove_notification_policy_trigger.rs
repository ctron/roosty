use sea_orm_migration::prelude::*;

/// Removes legacy database-side policy creation now owned by Rust transactions.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                DROP TRIGGER IF EXISTS local_account_notification_policy ON local_account;
                DROP FUNCTION IF EXISTS create_default_local_notification_policy();
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE FUNCTION create_default_local_notification_policy() RETURNS trigger AS $$
                BEGIN
                    INSERT INTO local_notification_policy (account_id) VALUES (NEW.id);
                    RETURN NEW;
                END;
                $$ LANGUAGE plpgsql;
                CREATE TRIGGER local_account_notification_policy
                    AFTER INSERT ON local_account FOR EACH ROW
                    EXECUTE FUNCTION create_default_local_notification_policy();
                "#,
            )
            .await?;
        Ok(())
    }
}
