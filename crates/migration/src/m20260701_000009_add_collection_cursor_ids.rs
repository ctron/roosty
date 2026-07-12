use sea_orm_migration::prelude::*;

/// Adds opaque UUID cursor identifiers to Mastodon collection tables.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                ALTER TABLE local_status_favourite
                    ADD COLUMN IF NOT EXISTS id uuid;
                UPDATE local_status_favourite
                    SET id = uuidv7()
                    WHERE id IS NULL;
                ALTER TABLE local_status_favourite
                    ALTER COLUMN id SET NOT NULL;
                CREATE UNIQUE INDEX IF NOT EXISTS local_status_favourite_id_idx
                    ON local_status_favourite(id);
                CREATE INDEX IF NOT EXISTS local_status_favourite_account_cursor_idx
                    ON local_status_favourite(account_id, id DESC);

                ALTER TABLE local_status_bookmark
                    ADD COLUMN IF NOT EXISTS id uuid;
                UPDATE local_status_bookmark
                    SET id = uuidv7()
                    WHERE id IS NULL;
                ALTER TABLE local_status_bookmark
                    ALTER COLUMN id SET NOT NULL;
                CREATE UNIQUE INDEX IF NOT EXISTS local_status_bookmark_id_idx
                    ON local_status_bookmark(id);
                CREATE INDEX IF NOT EXISTS local_status_bookmark_account_cursor_idx
                    ON local_status_bookmark(account_id, id DESC);

                ALTER TABLE local_follow
                    ADD COLUMN IF NOT EXISTS id uuid;
                UPDATE local_follow
                    SET id = uuidv7()
                    WHERE id IS NULL;
                ALTER TABLE local_follow
                    ALTER COLUMN id SET NOT NULL;
                CREATE UNIQUE INDEX IF NOT EXISTS local_follow_id_idx
                    ON local_follow(id);
                CREATE INDEX IF NOT EXISTS local_follow_follower_cursor_idx
                    ON local_follow(follower_account_id, id DESC);
                CREATE INDEX IF NOT EXISTS local_follow_followed_cursor_idx
                    ON local_follow(followed_account_id, id DESC);
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
                DROP INDEX IF EXISTS local_follow_followed_cursor_idx;
                DROP INDEX IF EXISTS local_follow_follower_cursor_idx;
                DROP INDEX IF EXISTS local_follow_id_idx;
                ALTER TABLE local_follow DROP COLUMN IF EXISTS id;

                DROP INDEX IF EXISTS local_status_bookmark_account_cursor_idx;
                DROP INDEX IF EXISTS local_status_bookmark_id_idx;
                ALTER TABLE local_status_bookmark DROP COLUMN IF EXISTS id;

                DROP INDEX IF EXISTS local_status_favourite_account_cursor_idx;
                DROP INDEX IF EXISTS local_status_favourite_id_idx;
                ALTER TABLE local_status_favourite DROP COLUMN IF EXISTS id;
                "#,
            )
            .await?;

        Ok(())
    }
}
