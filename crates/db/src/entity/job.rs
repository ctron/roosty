use crate::JobKind;
use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// SeaORM model for a durable background job.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "job")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub kind: JobKind,
    pub payload: Json,
    pub deduplication_key: Option<String>,
    pub run_after: OffsetDateTime,
    pub attempts: i32,
    pub locked_at: Option<OffsetDateTime>,
    pub locked_by: Option<String>,
    pub claim_id: Option<Uuid>,
    pub last_error: Option<String>,
    pub created_at: OffsetDateTime,
    pub completed_at: Option<OffsetDateTime>,
    pub permanently_failed_at: Option<OffsetDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
