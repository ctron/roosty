use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// A local account explicitly addressed by a cached remote direct status.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "remote_status_local_recipient")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub remote_status_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub account_id: Uuid,
    pub created_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
