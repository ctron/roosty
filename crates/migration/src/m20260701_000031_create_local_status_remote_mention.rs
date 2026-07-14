use sea_orm_migration::prelude::*;

/// Links locally authored statuses to successfully resolved remote mentions.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE TABLE local_status_remote_mention (\
                status_id uuid NOT NULL REFERENCES local_status(id) ON DELETE CASCADE, \
                remote_actor_id uuid NOT NULL REFERENCES remote_actor(id) ON DELETE CASCADE, \
                created_at timestamptz NOT NULL DEFAULT now(), \
                PRIMARY KEY (status_id, remote_actor_id)\
             );",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS local_status_remote_mention;")
            .await?;
        Ok(())
    }
}
