use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// SeaORM model for a local actor following a remote ActivityPub actor.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "remote_following")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub local_account_id: Uuid,
    pub remote_actor_id: Uuid,
    pub activity_id: String,
    pub state: String,
    pub deactivated_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
