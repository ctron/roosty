use sea_orm_migration::prelude::*;

/// Adds the persisted eligibility and activity fields needed by the profile directory.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                ALTER TABLE local_account
                    ADD COLUMN last_status_at timestamptz;
                ALTER TABLE remote_actor
                    ADD COLUMN discoverable boolean,
                    ADD COLUMN last_status_at timestamptz;

                UPDATE local_account account
                   SET last_status_at = latest.last_status_at
                  FROM (
                    SELECT account_id, max(created_at) AS last_status_at
                      FROM local_status
                     WHERE deleted_at IS NULL
                     GROUP BY account_id
                  ) latest
                 WHERE account.id = latest.account_id;

                UPDATE remote_actor actor
                   SET last_status_at = latest.last_status_at
                  FROM (
                    SELECT remote_actor_id, max(published_at) AS last_status_at
                      FROM remote_status
                     WHERE deleted_at IS NULL
                     GROUP BY remote_actor_id
                  ) latest
                 WHERE actor.id = latest.remote_actor_id;

                CREATE INDEX local_account_directory_active_idx
                    ON local_account(
                        last_status_at DESC NULLS LAST,
                        created_at DESC,
                        id DESC
                    )
                    WHERE discoverable AND limited_at IS NULL;
                CREATE INDEX local_account_directory_new_idx
                    ON local_account(created_at DESC, id DESC)
                    WHERE discoverable AND limited_at IS NULL;
                CREATE INDEX remote_actor_directory_active_idx
                    ON remote_actor(
                        last_status_at DESC NULLS LAST,
                        coalesce(profile_created_at, created_at) DESC,
                        id DESC
                    )
                    WHERE discoverable IS TRUE
                      AND limited_at IS NULL
                      AND deleted_at IS NULL
                      AND moved_to_remote_actor_id IS NULL;
                CREATE INDEX remote_actor_directory_new_idx
                    ON remote_actor(
                        coalesce(profile_created_at, created_at) DESC,
                        id DESC
                    )
                    WHERE discoverable IS TRUE
                      AND limited_at IS NULL
                      AND deleted_at IS NULL
                      AND moved_to_remote_actor_id IS NULL;
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
                DROP INDEX IF EXISTS remote_actor_directory_new_idx;
                DROP INDEX IF EXISTS remote_actor_directory_active_idx;
                DROP INDEX IF EXISTS local_account_directory_new_idx;
                DROP INDEX IF EXISTS local_account_directory_active_idx;
                ALTER TABLE remote_actor
                    DROP COLUMN IF EXISTS last_status_at,
                    DROP COLUMN IF EXISTS discoverable;
                ALTER TABLE local_account
                    DROP COLUMN IF EXISTS last_status_at;
                "#,
            )
            .await?;
        Ok(())
    }
}
