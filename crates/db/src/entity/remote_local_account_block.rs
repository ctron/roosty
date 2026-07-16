use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// A cached remote actor's validated ActivityPub block of a local account.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "remote_local_account_block")]
pub struct Model {
    pub id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub remote_actor_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub local_account_id: Uuid,
    pub activity_id: String,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
