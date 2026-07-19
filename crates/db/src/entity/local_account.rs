use crate::{QuoteApprovalPolicy, StatusVisibility};
use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// SeaORM model for local user accounts and account settings.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "local_account")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub username: String,
    pub email: String,
    pub password_hash: String,
    pub is_admin: bool,
    pub display_name: String,
    pub note: String,
    pub locked: bool,
    pub bot: bool,
    pub discoverable: bool,
    pub default_visibility: StatusVisibility,
    pub default_sensitive: bool,
    pub default_language: Option<String>,
    pub default_quote_policy: QuoteApprovalPolicy,
    pub profile_fields: Json,
    pub avatar_file_path: Option<String>,
    pub header_file_path: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
