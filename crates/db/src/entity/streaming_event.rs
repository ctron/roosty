use sea_orm::entity::prelude::*;

/// One retained cross-process Mastodon streaming event.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "streaming_event")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = true)]
    pub sequence: i64,
    pub origin_process_id: Uuid,
    pub event_kind: String,
    pub payload: String,
    pub account_id: Uuid,
    pub recipient_ids: Json,
    pub visibility: String,
    pub created_at: TimeDateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
