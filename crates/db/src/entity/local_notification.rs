use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// SeaORM model for local Mastodon-compatible notifications.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "local_notification")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub account_id: Uuid,
    pub notification_type: String,
    pub actor_account_id: Option<Uuid>,
    pub remote_actor_id: Option<Uuid>,
    pub status_id: Option<Uuid>,
    pub remote_status_id: Option<Uuid>,
    pub created_at: OffsetDateTime,
    pub dismissed_at: Option<OffsetDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
