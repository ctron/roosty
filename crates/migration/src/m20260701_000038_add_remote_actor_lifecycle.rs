use sea_orm_migration::prelude::*;

/// Adds non-destructive lifecycle state for cached remote ActivityPub actors.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                ALTER TABLE remote_actor
                    ADD COLUMN deleted_at timestamptz,
                    ADD COLUMN moved_to_remote_actor_id uuid REFERENCES remote_actor(id);
                CREATE INDEX remote_actor_moved_to_idx ON remote_actor(moved_to_remote_actor_id);
                ALTER TABLE remote_following ADD COLUMN deactivated_at timestamptz;
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
                DROP INDEX IF EXISTS remote_actor_moved_to_idx;
                ALTER TABLE remote_following DROP COLUMN deactivated_at;
                ALTER TABLE remote_actor DROP COLUMN moved_to_remote_actor_id;
                ALTER TABLE remote_actor DROP COLUMN deleted_at;
                "#,
            )
            .await?;
        Ok(())
    }
}
