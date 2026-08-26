use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// SeaORM model for a locally observed hashtag.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "local_tag")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub name: String,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::local_tag_follow::Entity")]
    Follows,
    #[sea_orm(has_many = "super::local_featured_tag::Entity")]
    FeaturedTags,
}

impl Related<super::local_tag_follow::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Follows.def()
    }
}

impl Related<super::local_featured_tag::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::FeaturedTags.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
