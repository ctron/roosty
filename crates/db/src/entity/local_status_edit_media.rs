use sea_orm::entity::prelude::*;

/// Immutable media projection belonging to a local status revision.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "local_status_edit_media")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub local_status_edit_id: Uuid,
    pub local_media_attachment_id: Uuid,
    pub status_order: i32,
    pub content_type: String,
    pub file_path: String,
    pub preview_file_path: Option<String>,
    pub description: Option<String>,
    pub focus_x: Option<f64>,
    pub focus_y: Option<f64>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub preview_width: Option<i32>,
    pub preview_height: Option<i32>,
    pub blurhash: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
