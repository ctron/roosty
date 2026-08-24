use sea_orm_migration::prelude::*;

/// Adds deterministic keyset indexes for administrator account sorting.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE INDEX local_account_admin_name_idx
                    ON local_account(lower(username), id);
                CREATE INDEX local_account_admin_email_idx
                    ON local_account(lower(email), id);
                CREATE INDEX local_account_admin_role_idx
                    ON local_account(is_admin, id);
                CREATE INDEX local_account_admin_state_idx
                    ON local_account(
                        (CASE WHEN suspended_at IS NOT NULL THEN 2
                              WHEN limited_at IS NOT NULL THEN 1 ELSE 0 END),
                        id
                    );
                CREATE INDEX local_account_admin_created_idx
                    ON local_account(created_at, id);

                CREATE INDEX remote_actor_admin_handle_idx
                    ON remote_actor(lower(username || '@' || domain), id)
                    WHERE deleted_at IS NULL;
                CREATE INDEX remote_actor_admin_state_idx
                    ON remote_actor(
                        (CASE WHEN suspended_at IS NOT NULL THEN 2
                              WHEN limited_at IS NOT NULL THEN 1 ELSE 0 END),
                        id
                    ) WHERE deleted_at IS NULL;
                CREATE INDEX remote_actor_admin_created_idx
                    ON remote_actor(coalesce(profile_created_at, created_at), id)
                    WHERE deleted_at IS NULL;
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
                DROP INDEX IF EXISTS remote_actor_admin_created_idx;
                DROP INDEX IF EXISTS remote_actor_admin_state_idx;
                DROP INDEX IF EXISTS remote_actor_admin_handle_idx;
                DROP INDEX IF EXISTS local_account_admin_created_idx;
                DROP INDEX IF EXISTS local_account_admin_state_idx;
                DROP INDEX IF EXISTS local_account_admin_role_idx;
                DROP INDEX IF EXISTS local_account_admin_email_idx;
                DROP INDEX IF EXISTS local_account_admin_name_idx;
                "#,
            )
            .await?;
        Ok(())
    }
}
