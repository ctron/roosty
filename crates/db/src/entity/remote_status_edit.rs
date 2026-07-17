use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// Immutable content and ActivityPub metadata for one cached remote revision.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "remote_status_edit")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub remote_status_id: Uuid,
    pub content: String,
    pub spoiler_text: String,
    pub sensitive: bool,
    pub object: Json,
    pub created_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
