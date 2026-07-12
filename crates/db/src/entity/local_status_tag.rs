use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// SeaORM model linking local statuses to locally observed hashtags.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "local_status_tag")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub status_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub tag_id: Uuid,
    pub created_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
