use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// SeaORM model for a resolved remote actor mentioned by a local status.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "local_status_remote_mention")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub status_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub remote_actor_id: Uuid,
    pub created_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
