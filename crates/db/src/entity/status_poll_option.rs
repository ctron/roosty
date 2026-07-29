use sea_orm::entity::prelude::*;

/// Ordered option and current authoritative tally for a status poll.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "status_poll_option")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub poll_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub position: i32,
    pub title: String,
    pub votes_count: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
