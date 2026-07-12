use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// SeaORM model for OAuth client applications.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "oauth_application")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub client_id: String,
    pub client_secret_hash: String,
    pub name: String,
    pub redirect_uri: String,
    pub scopes: String,
    pub website: Option<String>,
    pub created_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
