use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// SeaORM model for a local account's favourite of a cached remote Note.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "local_remote_status_favourite")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub local_account_id: Uuid,
    pub remote_status_id: Uuid,
    pub activity_id: String,
    pub created_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
