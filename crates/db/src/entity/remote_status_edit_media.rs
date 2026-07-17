use sea_orm::entity::prelude::*;

/// Immutable media projection belonging to a cached remote revision.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "remote_status_edit_media")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub remote_status_edit_id: Uuid,
    pub source_attachment_id: Option<Uuid>,
    pub status_order: i32,
    pub remote_url: String,
    pub content_type: Option<String>,
    pub file_path: Option<String>,
    pub preview_file_path: Option<String>,
    pub description: Option<String>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub preview_width: Option<i32>,
    pub preview_height: Option<i32>,
    pub blurhash: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
