use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// SeaORM model for a public or unlisted Note cached from a remote actor.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "remote_status")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub activitypub_id: String,
    pub remote_actor_id: Uuid,
    pub content: String,
    pub visibility: String,
    pub published_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub deleted_at: Option<OffsetDateTime>,
    pub in_reply_to: Option<String>,
    pub in_reply_to_local_status_id: Option<Uuid>,
    pub in_reply_to_remote_status_id: Option<Uuid>,
    pub object: Json,
    pub created_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
