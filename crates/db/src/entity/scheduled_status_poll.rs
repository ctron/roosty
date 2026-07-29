use sea_orm::entity::prelude::*;

/// Poll intent retained with a scheduled status until publication.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "scheduled_status_poll")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub scheduled_status_id: Uuid,
    pub multiple: bool,
    pub hide_totals: bool,
    pub expires_in: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
