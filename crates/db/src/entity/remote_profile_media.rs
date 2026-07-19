use crate::{RemoteMediaState, RemoteProfileMediaKind};
use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// SeaORM model for an avatar or header cached for a remote actor.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "remote_profile_media")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub remote_actor_id: Uuid,
    pub kind: RemoteProfileMediaKind,
    pub remote_url: String,
    pub content_type: Option<String>,
    pub state: RemoteMediaState,
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
