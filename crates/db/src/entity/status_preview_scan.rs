use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use uuid::Uuid;

/// Durable marker showing that one status was inspected for an article URL.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "status_preview_scan")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub local_status_id: Option<Uuid>,
    pub remote_status_id: Option<Uuid>,
    pub scanned_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
