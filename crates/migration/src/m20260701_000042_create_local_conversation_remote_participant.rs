use sea_orm_migration::prelude::*;

/// Stores remote and unresolved ActivityPub participants in direct conversations.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(
            "CREATE TABLE IF NOT EXISTS local_conversation_remote_participant (id uuid PRIMARY KEY, conversation_id uuid NOT NULL REFERENCES local_conversation(id) ON DELETE CASCADE, activitypub_id text NOT NULL, remote_actor_id uuid REFERENCES remote_actor(id) ON DELETE SET NULL, mention_name text, created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now(), UNIQUE(conversation_id, activitypub_id)); CREATE INDEX IF NOT EXISTS local_conversation_remote_participant_conversation_idx ON local_conversation_remote_participant(conversation_id);",
        ).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS local_conversation_remote_participant;")
            .await?;
        Ok(())
    }
}
