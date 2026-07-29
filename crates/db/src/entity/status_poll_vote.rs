use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// One selected option by a local account or verified remote actor.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "status_poll_vote")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub poll_id: Uuid,
    pub choice_position: i32,
    pub local_account_id: Option<Uuid>,
    pub remote_actor_id: Option<Uuid>,
    pub activitypub_id: Option<String>,
    pub created_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
