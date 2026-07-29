use crate::ReportCategory;
use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// Durable local moderation case created by a local or federated report.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "moderation_report")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub source_local_account_id: Option<Uuid>,
    pub source_remote_actor_id: Option<Uuid>,
    pub target_local_account_id: Option<Uuid>,
    pub target_remote_actor_id: Option<Uuid>,
    pub category: ReportCategory,
    pub comment: String,
    pub forwarded: bool,
    pub activitypub_id: Option<String>,
    pub assigned_account_id: Option<Uuid>,
    pub action_taken_by_account_id: Option<Uuid>,
    pub action_taken_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
