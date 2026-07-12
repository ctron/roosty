use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// SeaORM model for local account follow relationships.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "local_follow")]
pub struct Model {
    pub id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub follower_account_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub followed_account_id: Uuid,
    pub show_reblogs: bool,
    pub notify: bool,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
