use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// Cached ordering metadata for a remote actor's featured status.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "remote_status_pin")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub remote_actor_id: Uuid,
    pub remote_status_id: Uuid,
    pub pinned_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
