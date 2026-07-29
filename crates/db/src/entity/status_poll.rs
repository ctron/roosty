use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// Poll attached to one local or cached remote status.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "status_poll")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub local_status_id: Option<Uuid>,
    pub remote_status_id: Option<Uuid>,
    pub multiple: bool,
    pub hide_totals: bool,
    pub expires_at: Option<OffsetDateTime>,
    pub closed_at: Option<OffsetDateTime>,
    pub notifications_sent_at: Option<OffsetDateTime>,
    pub voters_count: Option<i64>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
