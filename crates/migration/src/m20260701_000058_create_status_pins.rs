use sea_orm_migration::prelude::*;

/// Adds local pins and the bounded cache of remote actors' featured Notes.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                ALTER TABLE remote_actor ADD COLUMN featured_url text;

                CREATE TABLE local_status_pin (
                    id uuid PRIMARY KEY,
                    account_id uuid NOT NULL REFERENCES local_account(id) ON DELETE CASCADE,
                    status_id uuid NOT NULL UNIQUE REFERENCES local_status(id) ON DELETE CASCADE,
                    pinned_at timestamptz NOT NULL DEFAULT now()
                );
                CREATE INDEX local_status_pin_account_idx
                    ON local_status_pin(account_id, pinned_at DESC, id DESC);

                CREATE TABLE remote_status_pin (
                    id uuid PRIMARY KEY,
                    remote_actor_id uuid NOT NULL REFERENCES remote_actor(id) ON DELETE CASCADE,
                    remote_status_id uuid NOT NULL UNIQUE REFERENCES remote_status(id) ON DELETE CASCADE,
                    pinned_at timestamptz NOT NULL DEFAULT now()
                );
                CREATE INDEX remote_status_pin_actor_idx
                    ON remote_status_pin(remote_actor_id, pinned_at DESC, id DESC);
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
                DROP TABLE remote_status_pin;
                DROP TABLE local_status_pin;
                ALTER TABLE remote_actor DROP COLUMN featured_url;
                "#,
            )
            .await?;
        Ok(())
    }
}
