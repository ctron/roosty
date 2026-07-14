use sea_orm_migration::prelude::*;

/// Stores inbound remote favourites and locally authored favourites of cached remote Notes.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
            CREATE TABLE remote_status_favourite (
                id uuid PRIMARY KEY,
                remote_actor_id uuid NOT NULL REFERENCES remote_actor(id) ON DELETE CASCADE,
                local_status_id uuid NOT NULL REFERENCES local_status(id) ON DELETE CASCADE,
                activity_id text NOT NULL,
                created_at timestamptz NOT NULL DEFAULT now(),
                UNIQUE(remote_actor_id, local_status_id),
                UNIQUE(activity_id)
            );
            CREATE INDEX remote_status_favourite_status_idx
                ON remote_status_favourite(local_status_id);

            CREATE TABLE local_remote_status_favourite (
                id uuid PRIMARY KEY,
                local_account_id uuid NOT NULL REFERENCES local_account(id) ON DELETE CASCADE,
                remote_status_id uuid NOT NULL REFERENCES remote_status(id) ON DELETE CASCADE,
                activity_id text NOT NULL,
                created_at timestamptz NOT NULL DEFAULT now(),
                UNIQUE(local_account_id, remote_status_id),
                UNIQUE(activity_id)
            );
            CREATE INDEX local_remote_status_favourite_account_cursor_idx
                ON local_remote_status_favourite(local_account_id, id DESC);
            "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "DROP TABLE IF EXISTS local_remote_status_favourite; \
             DROP TABLE IF EXISTS remote_status_favourite;",
            )
            .await?;
        Ok(())
    }
}
