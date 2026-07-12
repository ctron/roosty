use sea_orm_migration::prelude::*;

/// Stores one encrypted ActivityPub signing key per local account.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
            CREATE TABLE IF NOT EXISTS local_actor_key (
                account_id uuid PRIMARY KEY REFERENCES local_account(id) ON DELETE CASCADE,
                public_key_pem text NOT NULL,
                private_key_ciphertext bytea NOT NULL,
                private_key_nonce bytea NOT NULL,
                created_at timestamptz NOT NULL DEFAULT now()
            );
        "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS local_actor_key;")
            .await?;
        Ok(())
    }
}
