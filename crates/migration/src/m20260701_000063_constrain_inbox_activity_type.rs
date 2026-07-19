use sea_orm_migration::prelude::*;

/// Keep the replay ledger aligned with the activity kinds handled by the inbox.
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
                ADD CONSTRAINT processed_inbox_activity_type_check
                CHECK (activity_type IS NULL OR activity_type IN (
                    'Follow', 'Accept', 'Reject', 'Create', 'Update', 'Delete',
                    'Like', 'Announce', 'Undo', 'Move', 'Block', 'Add', 'Remove',
                    'https://w3id.org/fep/044f#QuoteRequest'
                ));
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE processed_inbox_activity \
                 DROP CONSTRAINT IF EXISTS processed_inbox_activity_type_check;",
            )
            .await?;
        Ok(())
    }
}
