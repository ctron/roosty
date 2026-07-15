use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// SeaORM model for a local account's view of a direct-message conversation.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "local_conversation_account")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub cursor_id: Uuid,
    pub conversation_id: Uuid,
    pub account_id: Uuid,
    pub unread: bool,
    pub hidden_at: Option<OffsetDateTime>,
    pub last_status_id: Option<Uuid>,
    pub last_remote_status_id: Option<Uuid>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
