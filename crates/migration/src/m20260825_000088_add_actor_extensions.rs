use sea_orm_migration::prelude::*;

/// Persists Mastodon actor kinds and FEP-5feb search-indexing consent.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE TYPE activitypub_actor_type AS ENUM (
                    'person', 'service', 'application', 'group'
                );
                ALTER TABLE local_account
                    ADD COLUMN indexable boolean NOT NULL DEFAULT false;
                ALTER TABLE remote_actor
                    ADD COLUMN actor_type activitypub_actor_type NOT NULL DEFAULT 'person',
                    ADD COLUMN indexable boolean NOT NULL DEFAULT false;
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
                ALTER TABLE remote_actor DROP COLUMN IF EXISTS indexable;
                ALTER TABLE remote_actor DROP COLUMN IF EXISTS actor_type;
                ALTER TABLE local_account DROP COLUMN IF EXISTS indexable;
                DROP TYPE IF EXISTS activitypub_actor_type;
                "#,
            )
            .await?;
        Ok(())
    }
}
