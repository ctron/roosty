use sea_orm_migration::prelude::*;

/// Reject unsupported Mastodon visibility values at persistence boundaries.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE local_status DROP CONSTRAINT IF EXISTS local_status_visibility_check; \
                 ALTER TABLE remote_status DROP CONSTRAINT IF EXISTS remote_status_visibility_check; \
                 ALTER TABLE local_account DROP CONSTRAINT IF EXISTS local_account_default_visibility_check; \
                 ALTER TABLE local_status ADD CONSTRAINT local_status_visibility_check CHECK (visibility IN ('public', 'unlisted', 'private', 'direct')); \
                 ALTER TABLE remote_status ADD CONSTRAINT remote_status_visibility_check CHECK (visibility IN ('public', 'unlisted', 'private', 'direct')); \
                 ALTER TABLE local_account ADD CONSTRAINT local_account_default_visibility_check CHECK (default_visibility IN ('public', 'unlisted', 'private', 'direct'));",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE local_account DROP CONSTRAINT IF EXISTS local_account_default_visibility_check; \
                 ALTER TABLE remote_status DROP CONSTRAINT IF EXISTS remote_status_visibility_check; \
                 ALTER TABLE local_status DROP CONSTRAINT IF EXISTS local_status_visibility_check;",
            )
            .await?;
        Ok(())
    }
}
