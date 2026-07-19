use sea_orm::entity::prelude::*;
use serde_json::Value;
use time::OffsetDateTime;

/// SeaORM model for one access token's Web Push subscription.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "push_subscription")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub access_token_id: Uuid,
    pub account_id: Uuid,
    pub endpoint: String,
    pub p256dh: Vec<u8>,
    pub auth: Vec<u8>,
    pub standard: bool,
    pub policy: String,
    pub alerts: Value,
    pub access_token_ciphertext: Vec<u8>,
    pub access_token_nonce: Vec<u8>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
