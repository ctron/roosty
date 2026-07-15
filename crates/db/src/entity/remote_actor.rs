use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// SeaORM model for a validated remote ActivityPub actor cache entry.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "remote_actor")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub activitypub_id: String,
    pub username: String,
    pub domain: String,
    pub display_name: String,
    pub summary: String,
    pub emojis: Json,
    pub inbox_url: String,
    pub shared_inbox_url: Option<String>,
    pub public_key_id: String,
    pub public_key_pem: String,
    pub fetched_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
    pub profile_created_at: Option<OffsetDateTime>,
    pub deleted_at: Option<OffsetDateTime>,
    pub moved_to_remote_actor_id: Option<Uuid>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
