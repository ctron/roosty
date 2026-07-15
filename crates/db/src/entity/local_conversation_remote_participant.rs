use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// SeaORM model for a remote or unresolved direct-conversation participant.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "local_conversation_remote_participant")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub activitypub_id: String,
    pub remote_actor_id: Option<Uuid>,
    pub mention_name: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
