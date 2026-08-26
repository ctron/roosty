use sea_orm_migration::prelude::*;

/// Stores privacy-preserving account-registration attempts for rolling limits.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE TABLE registration_attempt (
                    id uuid PRIMARY KEY,
                    client_id varchar(43) NOT NULL,
                    attempted_at timestamptz NOT NULL
                );
                CREATE INDEX registration_attempt_client_time_idx
                    ON registration_attempt (client_id, attempted_at);
                CREATE INDEX registration_attempt_cleanup_idx
                    ON registration_attempt (attempted_at, id);
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE registration_attempt;")
            .await?;
        Ok(())
    }
}
