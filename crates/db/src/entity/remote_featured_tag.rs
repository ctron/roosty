use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// One cached hashtag from a remote actor's featured-tags collection.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "remote_featured_tag")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub remote_actor_id: Uuid,
    pub tag_id: Uuid,
    pub display_name: String,
    pub href: String,
    pub position: i32,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
