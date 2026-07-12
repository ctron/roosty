use sea_orm_migration::prelude::*;

/// Adds persisted local followed-tag relationships for existing hashtag databases.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE TABLE IF NOT EXISTS local_tag_follow (
                    account_id uuid NOT NULL REFERENCES local_account(id) ON DELETE CASCADE,
                    tag_id uuid NOT NULL REFERENCES local_tag(id) ON DELETE CASCADE,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    updated_at timestamptz NOT NULL DEFAULT now(),
                    PRIMARY KEY (account_id, tag_id)
                );

                CREATE INDEX IF NOT EXISTS local_tag_follow_account_tag_idx
                    ON local_tag_follow(account_id, tag_id);
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
                DROP TABLE IF EXISTS local_tag_follow;
                "#,
            )
            .await?;

        Ok(())
    }
}
