use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// SeaORM model for local media uploaded before status creation.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "local_media_attachment")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub account_id: Uuid,
    pub status_id: Option<Uuid>,
    pub status_order: i32,
    pub content_type: String,
    pub original_filename: String,
    pub file_path: String,
    pub preview_file_path: Option<String>,
    pub file_size: i64,
    pub description: Option<String>,
    pub focus_x: Option<f64>,
    pub focus_y: Option<f64>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub preview_width: Option<i32>,
    pub preview_height: Option<i32>,
    pub blurhash: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
