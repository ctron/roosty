use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// Sender override created by accepting a filtered-notification request.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "local_notification_permission")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub account_id: Uuid,
    pub actor_account_id: Option<Uuid>,
    pub remote_actor_id: Option<Uuid>,
    pub created_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
