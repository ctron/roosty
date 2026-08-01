use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// A follow suggestion that one local account chose not to see again.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "local_account_suggestion_dismissal")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub account_id: Uuid,
    pub target_account_id: Option<Uuid>,
    pub target_remote_actor_id: Option<Uuid>,
    pub created_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
