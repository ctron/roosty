use crate::{QuoteApprovalPolicy, StatusVisibility};
use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// A local status retained until its configured publication time.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "scheduled_status")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub account_id: Uuid,
    pub publication_status_id: Uuid,
    pub content: String,
    pub visibility: StatusVisibility,
    pub sensitive: bool,
    pub spoiler_text: String,
    pub language: Option<String>,
    pub in_reply_to_id: Option<Uuid>,
    pub in_reply_to_remote_status_id: Option<Uuid>,
    pub quoted_status_id: Option<Uuid>,
    pub quote_approval_policy: QuoteApprovalPolicy,
    pub scheduled_at: OffsetDateTime,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
