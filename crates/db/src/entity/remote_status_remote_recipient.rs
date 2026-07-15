use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// A remote ActivityPub actor explicitly addressed by a cached remote direct status.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "remote_status_remote_recipient")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub remote_status_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub activitypub_id: String,
    pub remote_actor_id: Option<Uuid>,
    pub mention_name: Option<String>,
    pub created_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
