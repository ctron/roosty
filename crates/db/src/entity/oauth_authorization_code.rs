use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// SeaORM model for short-lived OAuth authorization codes.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "oauth_authorization_code")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub code_hash: String,
    pub account_id: Uuid,
    pub application_id: Uuid,
    pub redirect_uri: String,
    pub scopes: String,
    pub code_challenge: String,
    pub code_challenge_method: String,
    pub expires_at: OffsetDateTime,
    pub consumed_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
