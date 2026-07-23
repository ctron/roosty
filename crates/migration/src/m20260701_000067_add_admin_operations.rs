use sea_orm_migration::prelude::*;

/// Adds durable job outcomes and an audit ledger for administrator mutations.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                ALTER TABLE job ADD COLUMN permanently_failed_at timestamptz;

                CREATE TABLE admin_audit_log (
                    id uuid PRIMARY KEY,
                    actor_account_id uuid REFERENCES local_account(id) ON DELETE SET NULL,
                    source text NOT NULL,
                    action text NOT NULL,
                    target_kind text NOT NULL,
                    target_id text NOT NULL,
                    metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    CONSTRAINT admin_audit_source_valid
                        CHECK (source IN ('web', 'api', 'cli'))
                );
                CREATE INDEX admin_audit_log_created_idx
                    ON admin_audit_log(created_at DESC, id DESC);
                CREATE INDEX admin_audit_log_actor_idx
                    ON admin_audit_log(actor_account_id, created_at DESC);
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
                DROP TABLE IF EXISTS admin_audit_log;
                ALTER TABLE job DROP COLUMN IF EXISTS permanently_failed_at;
                "#,
            )
            .await?;
        Ok(())
    }
}
