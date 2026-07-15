use sea_orm_migration::prelude::*;

/// Stores exact direct-status audiences and recipient-specific conversation cursors.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(
            r#"
            CREATE TABLE local_status_local_recipient (
                status_id uuid NOT NULL REFERENCES local_status(id) ON DELETE CASCADE,
                account_id uuid NOT NULL REFERENCES local_account(id) ON DELETE CASCADE,
                created_at timestamptz NOT NULL DEFAULT now(),
                PRIMARY KEY (status_id, account_id)
            );
            CREATE INDEX local_status_local_recipient_account_idx
                ON local_status_local_recipient(account_id, status_id);
            CREATE TABLE remote_status_local_recipient (
                remote_status_id uuid NOT NULL REFERENCES remote_status(id) ON DELETE CASCADE,
                account_id uuid NOT NULL REFERENCES local_account(id) ON DELETE CASCADE,
                created_at timestamptz NOT NULL DEFAULT now(),
                PRIMARY KEY (remote_status_id, account_id)
            );
            CREATE INDEX remote_status_local_recipient_account_idx
                ON remote_status_local_recipient(account_id, remote_status_id);
            CREATE TABLE remote_status_remote_recipient (
                remote_status_id uuid NOT NULL REFERENCES remote_status(id) ON DELETE CASCADE,
                activitypub_id text NOT NULL,
                remote_actor_id uuid REFERENCES remote_actor(id) ON DELETE SET NULL,
                mention_name text,
                created_at timestamptz NOT NULL DEFAULT now(),
                PRIMARY KEY (remote_status_id, activitypub_id)
            );
            ALTER TABLE local_conversation_account
                ADD COLUMN last_status_id uuid REFERENCES local_status(id) ON DELETE SET NULL,
                ADD COLUMN last_remote_status_id uuid REFERENCES remote_status(id) ON DELETE SET NULL;
            INSERT INTO local_status_local_recipient(status_id, account_id)
              SELECT status.id, view.account_id
                FROM local_status status
                JOIN local_conversation_account view ON view.conversation_id = status.conversation_id
               WHERE status.visibility = 'direct'
              ON CONFLICT DO NOTHING;
            INSERT INTO remote_status_local_recipient(remote_status_id, account_id)
              SELECT status.id, view.account_id
                FROM remote_status status
                JOIN local_conversation_account view ON view.conversation_id = status.conversation_id
               WHERE status.visibility = 'direct'
              ON CONFLICT DO NOTHING;
            INSERT INTO remote_status_remote_recipient(remote_status_id, activitypub_id, remote_actor_id, mention_name)
              SELECT status.id, participant.activitypub_id, participant.remote_actor_id, participant.mention_name
                FROM remote_status status
                JOIN local_conversation_remote_participant participant ON participant.conversation_id = status.conversation_id
               WHERE status.visibility = 'direct'
              ON CONFLICT DO NOTHING;
            UPDATE local_conversation_account view
               SET last_status_id = conversation.last_status_id,
                   last_remote_status_id = conversation.last_remote_status_id
              FROM local_conversation conversation
             WHERE conversation.id = view.conversation_id;
            "#,
        ).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(
            "ALTER TABLE local_conversation_account DROP COLUMN IF EXISTS last_remote_status_id; \
             ALTER TABLE local_conversation_account DROP COLUMN IF EXISTS last_status_id; \
             DROP TABLE IF EXISTS remote_status_remote_recipient; \
             DROP TABLE IF EXISTS remote_status_local_recipient; \
             DROP TABLE IF EXISTS local_status_local_recipient;",
        ).await?;
        Ok(())
    }
}
