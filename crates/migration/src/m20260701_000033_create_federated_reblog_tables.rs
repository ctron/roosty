use sea_orm_migration::prelude::*;

/// Stores inbound remote Announce activities and local Announce activities for cached Notes.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
            CREATE TABLE local_remote_status_reblog (
                id uuid PRIMARY KEY,
                local_account_id uuid NOT NULL REFERENCES local_account(id) ON DELETE CASCADE,
                remote_status_id uuid NOT NULL REFERENCES remote_status(id) ON DELETE CASCADE,
                activity_id text NOT NULL UNIQUE,
                created_at timestamptz NOT NULL DEFAULT now(),
                UNIQUE(local_account_id, remote_status_id)
            );
            CREATE INDEX local_remote_status_reblog_account_cursor_idx
                ON local_remote_status_reblog(local_account_id, id DESC);

            CREATE TABLE remote_status_reblog (
                id uuid PRIMARY KEY,
                remote_actor_id uuid NOT NULL REFERENCES remote_actor(id) ON DELETE CASCADE,
                local_status_id uuid REFERENCES local_status(id) ON DELETE CASCADE,
                remote_status_id uuid REFERENCES remote_status(id) ON DELETE CASCADE,
                activity_id text NOT NULL UNIQUE,
                created_at timestamptz NOT NULL DEFAULT now(),
                CHECK ((local_status_id IS NULL) <> (remote_status_id IS NULL))
            );
            CREATE UNIQUE INDEX remote_status_reblog_local_target_idx
                ON remote_status_reblog(remote_actor_id, local_status_id)
                WHERE local_status_id IS NOT NULL;
            CREATE UNIQUE INDEX remote_status_reblog_remote_target_idx
                ON remote_status_reblog(remote_actor_id, remote_status_id)
                WHERE remote_status_id IS NOT NULL;
            CREATE INDEX remote_status_reblog_actor_cursor_idx
                ON remote_status_reblog(remote_actor_id, id DESC);
            CREATE INDEX remote_status_reblog_local_status_idx
                ON remote_status_reblog(local_status_id) WHERE local_status_id IS NOT NULL;
            "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "DROP TABLE IF EXISTS remote_status_reblog; \
                 DROP TABLE IF EXISTS local_remote_status_reblog;",
            )
            .await?;
        Ok(())
    }
}
