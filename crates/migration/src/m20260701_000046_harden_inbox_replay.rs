use sea_orm_migration::prelude::*;

/// Adds payload-aware replay metadata while preserving legacy activity markers.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                ALTER TABLE processed_inbox_activity
                    ADD COLUMN payload_digest bytea,
                    ADD COLUMN activity_type text,
                    ADD COLUMN outcome text;
                ALTER TABLE processed_inbox_activity
                    ADD CONSTRAINT processed_inbox_activity_digest_length
                        CHECK (payload_digest IS NULL OR octet_length(payload_digest) = 32),
                    ADD CONSTRAINT processed_inbox_activity_outcome
                        CHECK (outcome IS NULL OR outcome IN ('accepted', 'ignored'));
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
                ALTER TABLE processed_inbox_activity
                    DROP CONSTRAINT IF EXISTS processed_inbox_activity_outcome,
                    DROP CONSTRAINT IF EXISTS processed_inbox_activity_digest_length,
                    DROP COLUMN IF EXISTS outcome,
                    DROP COLUMN IF EXISTS activity_type,
                    DROP COLUMN IF EXISTS payload_digest;
                "#,
            )
            .await?;
        Ok(())
    }
}
