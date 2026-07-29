use sea_orm::entity::prelude::*;

/// Ordered option retained with a scheduled poll.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "scheduled_status_poll_option")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub scheduled_status_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub position: i32,
    pub title: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
