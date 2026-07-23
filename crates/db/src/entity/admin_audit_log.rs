use crate::{AdminAuditAction, AdminAuditSource, AdminAuditTargetKind};
use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// Immutable record of an administrator or operator mutation.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "admin_audit_log")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub actor_account_id: Option<Uuid>,
    pub source: AdminAuditSource,
    pub action: AdminAuditAction,
    pub target_kind: AdminAuditTargetKind,
    pub target_id: String,
    pub metadata: Json,
    pub created_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
