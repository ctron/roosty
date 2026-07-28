use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// Searchable text projection for exactly one local or cached remote status.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "status_search_document")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub local_status_id: Option<Uuid>,
    pub remote_status_id: Option<Uuid>,
    pub document: String,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
