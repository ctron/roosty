use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// Persisted quote edge and its FEP-044f authorization lifecycle.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "status_quote")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub local_quoting_status_id: Option<Uuid>,
    pub remote_quoting_status_id: Option<Uuid>,
    pub quoted_local_status_id: Option<Uuid>,
    pub quoted_remote_status_id: Option<Uuid>,
    pub quoted_activitypub_id: String,
    pub state: String,
    pub quote_request_id: Option<String>,
    pub authorization_id: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
