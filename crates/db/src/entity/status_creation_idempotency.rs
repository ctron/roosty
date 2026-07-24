use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

use crate::StatusCreationResultKind;

/// One hour account-scoped mapping from an idempotency key to its creation result.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "status_creation_idempotency")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub account_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub key_hash: String,
    pub result_kind: StatusCreationResultKind,
    pub result_id: Uuid,
    pub expires_at: OffsetDateTime,
    pub created_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
