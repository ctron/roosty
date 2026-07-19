use crate::LocalNotificationType;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue, ConnectionTrait, Statement};
use serde_json::json;
use time::OffsetDateTime;

/// SeaORM model for local Mastodon-compatible notifications.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "local_notification")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub account_id: Uuid,
    pub notification_type: LocalNotificationType,
    pub actor_account_id: Option<Uuid>,
    pub remote_actor_id: Option<Uuid>,
    pub status_id: Option<Uuid>,
    pub remote_status_id: Option<Uuid>,
    pub group_id: Option<Uuid>,
    pub created_at: OffsetDateTime,
    pub dismissed_at: Option<OffsetDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

#[async_trait::async_trait]
impl ActiveModelBehavior for ActiveModel {
    async fn before_save<C>(mut self, db: &C, insert: bool) -> Result<Self, DbErr>
    where
        C: ConnectionTrait,
    {
        if !insert {
            return Ok(self);
        }
        let value = |field: &ActiveValue<Uuid>| match field {
            ActiveValue::Set(value) | ActiveValue::Unchanged(value) => Some(*value),
            ActiveValue::NotSet => None,
        };
        let optional = |field: &ActiveValue<Option<Uuid>>| match field {
            ActiveValue::Set(value) | ActiveValue::Unchanged(value) => *value,
            ActiveValue::NotSet => None,
        };
        let notification_type = match &self.notification_type {
            ActiveValue::Set(value) | ActiveValue::Unchanged(value) => Some(*value),
            ActiveValue::NotSet => None,
        };
        let Some(account_id) = value(&self.account_id) else {
            return Ok(self);
        };
        let Some(notification_type) = notification_type else {
            return Ok(self);
        };
        let target = match notification_type {
            LocalNotificationType::Follow => Some(("account", account_id)),
            LocalNotificationType::Favourite | LocalNotificationType::Reblog => {
                optional(&self.status_id)
                    .map(|id| ("local_status", id))
                    .or_else(|| optional(&self.remote_status_id).map(|id| ("remote_status", id)))
            }
            _ => None,
        };
        if let Some((target_kind, target_id)) = target {
            let candidate = Uuid::now_v7();
            let row = db.query_one(Statement::from_sql_and_values(db.get_database_backend(), r#"
                INSERT INTO local_notification_group_state (
                    account_id, notification_type, target_kind, target_id, group_id, started_at, updated_at
                ) VALUES ($1, $2, $3, $4, $5, now(), now())
                ON CONFLICT (account_id, notification_type, target_kind, target_id) DO UPDATE
                SET group_id = CASE
                        WHEN local_notification_group_state.started_at > now() - interval '12 hours'
                        THEN local_notification_group_state.group_id ELSE EXCLUDED.group_id END,
                    started_at = CASE
                        WHEN local_notification_group_state.started_at > now() - interval '12 hours'
                        THEN local_notification_group_state.started_at ELSE now() END,
                    updated_at = now()
                RETURNING group_id
            "#, vec![account_id.into(), notification_type.into(), target_kind.into(), target_id.into(), candidate.into()])).await?
                .ok_or_else(|| DbErr::Custom("notification group was not returned".to_owned()))?;
            self.group_id = ActiveValue::Set(Some(row.try_get("", "group_id")?));
        }
        Ok(self)
    }

    async fn after_save<C>(model: Model, db: &C, insert: bool) -> Result<Model, DbErr>
    where
        C: ConnectionTrait,
    {
        if insert {
            let subscriptions = super::push_subscription::Entity::find()
                .filter(super::push_subscription::Column::AccountId.eq(model.account_id))
                .all(db)
                .await?;
            for subscription in subscriptions {
                let job_id = Uuid::now_v7();
                let deduplication_key = format!("{}:{}", model.id, subscription.id);
                db.execute(Statement::from_sql_and_values(
                    db.get_database_backend(),
                    r#"
                    INSERT INTO job (id, kind, payload, deduplication_key, run_after)
                    VALUES ($1, 'web_push_delivery', $2, $3, now())
                    ON CONFLICT (kind, deduplication_key)
                    WHERE deduplication_key IS NOT NULL AND completed_at IS NULL
                    DO NOTHING
                    "#,
                    vec![
                        job_id.into(),
                        json!({
                            "notification_id": model.id,
                            "subscription_id": subscription.id,
                        })
                        .into(),
                        deduplication_key.into(),
                    ],
                ))
                .await?;
            }
        }
        Ok(model)
    }
}
