use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// SeaORM model for a safely cached remote custom emoji image.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "remote_custom_emoji")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub shortcode: String,
    pub remote_url: String,
    pub content_type: Option<String>,
    pub state: String,
    pub file_path: Option<String>,
    pub file_size: Option<i64>,
    pub fetched_at: Option<OffsetDateTime>,
    pub expires_at: Option<OffsetDateTime>,
    pub last_error: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
