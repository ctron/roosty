use sea_orm_migration::prelude::*;

/// Adds local featured hashtags and the bounded cache of remote actors' featured hashtags.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                ALTER TABLE remote_actor ADD COLUMN featured_tags_url text;

                CREATE TABLE local_featured_tag (
                    id uuid PRIMARY KEY,
                    account_id uuid NOT NULL REFERENCES local_account(id) ON DELETE CASCADE,
                    tag_id uuid NOT NULL REFERENCES local_tag(id) ON DELETE CASCADE,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    updated_at timestamptz NOT NULL DEFAULT now(),
                    UNIQUE (account_id, tag_id)
                );
                CREATE INDEX local_featured_tag_account_idx
                    ON local_featured_tag(account_id, created_at DESC, id DESC);

                CREATE TABLE remote_featured_tag (
                    id uuid PRIMARY KEY,
                    remote_actor_id uuid NOT NULL REFERENCES remote_actor(id) ON DELETE CASCADE,
                    tag_id uuid NOT NULL REFERENCES local_tag(id) ON DELETE CASCADE,
                    display_name text NOT NULL,
                    href text NOT NULL,
                    position integer NOT NULL,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    updated_at timestamptz NOT NULL DEFAULT now(),
                    UNIQUE (remote_actor_id, tag_id)
                );
                CREATE INDEX remote_featured_tag_actor_idx
                    ON remote_featured_tag(remote_actor_id, position ASC, id ASC);
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
                DROP TABLE remote_featured_tag;
                DROP TABLE local_featured_tag;
                ALTER TABLE remote_actor DROP COLUMN featured_tags_url;
                "#,
            )
            .await?;
        Ok(())
    }
}
