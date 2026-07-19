use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

use crate::ListRepliesPolicy;

/// One private Mastodon list owned by a local account.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "local_list")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub account_id: Uuid,
    pub title: String,
    pub replies_policy: ListRepliesPolicy,
    pub exclusive: bool,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
