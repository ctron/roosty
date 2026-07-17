use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// SeaORM model for statuses authored by local accounts.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "local_status")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub account_id: Uuid,
    pub content: String,
    pub visibility: String,
    pub sensitive: bool,
    pub spoiler_text: String,
    pub language: Option<String>,
    pub in_reply_to_id: Option<Uuid>,
    pub in_reply_to_remote_status_id: Option<Uuid>,
    pub conversation_id: Option<Uuid>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub deleted_at: Option<OffsetDateTime>,
    pub quote_approval_policy: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
