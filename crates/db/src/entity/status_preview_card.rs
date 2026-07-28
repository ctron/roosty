use crate::PreviewActorOrigin;
use sea_orm::entity::prelude::*;
use time::{Date, OffsetDateTime};
use uuid::Uuid;

/// One local or cached-remote status's selected preview card.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "status_preview_card")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub local_status_id: Option<Uuid>,
    pub remote_status_id: Option<Uuid>,
    pub preview_card_id: Uuid,
    pub usage_day: Date,
    pub actor_origin: PreviewActorOrigin,
    pub actor_id: Uuid,
    pub created_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
