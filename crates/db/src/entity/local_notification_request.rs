use crate::NotificationRequestState;
use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// One sender-scoped collection of filtered notifications.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "local_notification_request")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub account_id: Uuid,
    pub actor_account_id: Option<Uuid>,
    pub remote_actor_id: Option<Uuid>,
    pub last_status_id: Option<Uuid>,
    pub last_remote_status_id: Option<Uuid>,
    pub state: NotificationRequestState,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
