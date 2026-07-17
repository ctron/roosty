use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// A local account mentioned by a cached remote status.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "remote_status_local_mention")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub remote_status_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub account_id: Uuid,
    pub active: bool,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
