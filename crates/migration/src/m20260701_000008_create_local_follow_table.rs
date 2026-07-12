use sea_orm_migration::prelude::*;

/// Creates local account follow relationships.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE TABLE IF NOT EXISTS local_follow (
                    follower_account_id uuid NOT NULL REFERENCES local_account(id) ON DELETE CASCADE,
                    followed_account_id uuid NOT NULL REFERENCES local_account(id) ON DELETE CASCADE,
                    show_reblogs boolean NOT NULL DEFAULT true,
                    notify boolean NOT NULL DEFAULT false,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    updated_at timestamptz NOT NULL DEFAULT now(),
                    PRIMARY KEY (follower_account_id, followed_account_id),
                    CHECK (follower_account_id <> followed_account_id)
                );

                CREATE INDEX IF NOT EXISTS local_follow_followed_idx
                    ON local_follow(followed_account_id);
                "#,
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS local_follow;")
            .await?;

        Ok(())
    }
}
