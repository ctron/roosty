use sea_orm_migration::prelude::*;

/// Adds Mastodon-compatible private lists and their local/cached-remote members.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE TABLE local_list (
                    id uuid PRIMARY KEY,
                    account_id uuid NOT NULL REFERENCES local_account(id) ON DELETE CASCADE,
                    title text NOT NULL CHECK (length(btrim(title)) > 0),
                    replies_policy text NOT NULL DEFAULT 'list'
                        CHECK (replies_policy IN ('followed', 'list', 'none')),
                    exclusive boolean NOT NULL DEFAULT false,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    updated_at timestamptz NOT NULL DEFAULT now()
                );
                CREATE INDEX local_list_account_idx
                    ON local_list(account_id, created_at ASC, id ASC);

                CREATE TABLE local_list_local_member (
                    id uuid PRIMARY KEY,
                    list_id uuid NOT NULL REFERENCES local_list(id) ON DELETE CASCADE,
                    account_id uuid NOT NULL REFERENCES local_account(id) ON DELETE CASCADE,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    UNIQUE (list_id, account_id)
                );
                CREATE INDEX local_list_local_member_list_idx
                    ON local_list_local_member(list_id, id DESC);

                CREATE TABLE local_list_remote_member (
                    id uuid PRIMARY KEY,
                    list_id uuid NOT NULL REFERENCES local_list(id) ON DELETE CASCADE,
                    remote_actor_id uuid NOT NULL REFERENCES remote_actor(id) ON DELETE CASCADE,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    UNIQUE (list_id, remote_actor_id)
                );
                CREATE INDEX local_list_remote_member_list_idx
                    ON local_list_remote_member(list_id, id DESC);
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
                DROP TABLE local_list_remote_member;
                DROP TABLE local_list_local_member;
                DROP TABLE local_list;
                "#,
            )
            .await?;
        Ok(())
    }
}
