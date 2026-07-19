use crate::NotificationPolicyAction;
use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

/// Per-account Mastodon notification filtering policy.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "local_notification_policy")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub account_id: Uuid,
    pub for_not_following: NotificationPolicyAction,
    pub for_not_followers: NotificationPolicyAction,
    pub for_new_accounts: NotificationPolicyAction,
    pub for_private_mentions: NotificationPolicyAction,
    pub for_limited_accounts: NotificationPolicyAction,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
