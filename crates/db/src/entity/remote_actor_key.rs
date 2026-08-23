use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

use crate::ActorKeyAlgorithm;

/// SeaORM model for a validated FEP-521a or legacy remote actor key.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "remote_actor_key")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub key_id: String,
    pub remote_actor_id: Uuid,
    pub algorithm: ActorKeyAlgorithm,
    pub public_key: Vec<u8>,
    pub expires_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
