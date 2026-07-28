use crate::PreviewFetchState;
use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use uuid::Uuid;

/// Cached metadata for one normalized external article URL.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "preview_card")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    #[sea_orm(unique)]
    pub url: String,
    pub title: String,
    pub description: String,
    pub author_name: String,
    pub author_url: String,
    pub provider_name: String,
    pub provider_url: String,
    pub image_file_path: Option<String>,
    pub image_width: i32,
    pub image_height: i32,
    pub blurhash: Option<String>,
    pub published_at: Option<OffsetDateTime>,
    pub fetch_state: PreviewFetchState,
    pub fetched_at: Option<OffsetDateTime>,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
