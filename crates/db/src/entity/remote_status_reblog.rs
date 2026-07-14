use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// SeaORM model for a remote actor's boost of a local or cached remote status.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "remote_status_reblog")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub remote_actor_id: Uuid,
    pub local_status_id: Option<Uuid>,
    pub remote_status_id: Option<Uuid>,
    pub activity_id: String,
    pub created_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
