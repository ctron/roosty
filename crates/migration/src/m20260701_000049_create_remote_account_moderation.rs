use sea_orm_migration::prelude::*;

/// Adds per-account moderation relationships spanning the local/remote boundary.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
            CREATE TABLE local_remote_account_block (
                id uuid NOT NULL UNIQUE,
                local_account_id uuid NOT NULL REFERENCES local_account(id) ON DELETE CASCADE,
                remote_actor_id uuid NOT NULL REFERENCES remote_actor(id) ON DELETE CASCADE,
                activity_id text NOT NULL UNIQUE,
                created_at timestamptz NOT NULL DEFAULT now(),
                updated_at timestamptz NOT NULL DEFAULT now(),
                PRIMARY KEY (local_account_id, remote_actor_id)
            );
            CREATE INDEX local_remote_account_block_account_cursor_idx
                ON local_remote_account_block(local_account_id, id DESC);
            CREATE INDEX local_remote_account_block_actor_idx
                ON local_remote_account_block(remote_actor_id, local_account_id);

            CREATE TABLE remote_local_account_block (
                id uuid NOT NULL UNIQUE,
                remote_actor_id uuid NOT NULL REFERENCES remote_actor(id) ON DELETE CASCADE,
                local_account_id uuid NOT NULL REFERENCES local_account(id) ON DELETE CASCADE,
                activity_id text NOT NULL UNIQUE,
                created_at timestamptz NOT NULL DEFAULT now(),
                updated_at timestamptz NOT NULL DEFAULT now(),
                PRIMARY KEY (remote_actor_id, local_account_id)
            );
            CREATE INDEX remote_local_account_block_account_idx
                ON remote_local_account_block(local_account_id, remote_actor_id);

            CREATE TABLE local_remote_account_mute (
                id uuid NOT NULL UNIQUE,
                local_account_id uuid NOT NULL REFERENCES local_account(id) ON DELETE CASCADE,
                remote_actor_id uuid NOT NULL REFERENCES remote_actor(id) ON DELETE CASCADE,
                notifications boolean NOT NULL DEFAULT true,
                expires_at timestamptz,
                created_at timestamptz NOT NULL DEFAULT now(),
                updated_at timestamptz NOT NULL DEFAULT now(),
                PRIMARY KEY (local_account_id, remote_actor_id)
            );
            CREATE INDEX local_remote_account_mute_account_cursor_idx
                ON local_remote_account_mute(local_account_id, id DESC);
            CREATE INDEX local_remote_account_mute_actor_idx
                ON local_remote_account_mute(remote_actor_id, local_account_id);
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
            DROP TABLE IF EXISTS local_remote_account_mute;
            DROP TABLE IF EXISTS remote_local_account_block;
            DROP TABLE IF EXISTS local_remote_account_block;
        "#,
            )
            .await?;
        Ok(())
    }
}
