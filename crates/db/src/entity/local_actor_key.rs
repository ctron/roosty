use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// SeaORM model for encrypted local ActivityPub actor keys.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "local_actor_key")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub account_id: Uuid,
    pub public_key_pem: String,
    pub private_key_ciphertext: Vec<u8>,
    pub private_key_nonce: Vec<u8>,
    pub created_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
