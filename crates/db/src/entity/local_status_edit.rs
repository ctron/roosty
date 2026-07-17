use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// Immutable content and rendering metadata for one local status revision.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "local_status_edit")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub local_status_id: Uuid,
    pub content: String,
    pub spoiler_text: String,
    pub sensitive: bool,
    pub local_mention_ids: Json,
    pub remote_mention_ids: Json,
    pub tag_names: Json,
    pub created_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
