use crate::{InboxActivityOutcome, InboxActivityType};
use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// SeaORM model for an idempotently processed inbound ActivityPub activity.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "processed_inbox_activity")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub activity_id: String,
    pub remote_actor_id: Uuid,
    pub payload_digest: Option<Vec<u8>>,
    pub activity_type: Option<InboxActivityType>,
    pub outcome: Option<InboxActivityOutcome>,
    pub processed_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
