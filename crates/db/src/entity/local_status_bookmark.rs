use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// SeaORM model for local account bookmarks on statuses.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "local_status_bookmark")]
pub struct Model {
    pub id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub account_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub status_id: Uuid,
    pub created_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
