use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// Persisted ordering metadata for a locally pinned status.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "local_status_pin")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub account_id: Uuid,
    pub status_id: Uuid,
    pub pinned_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
