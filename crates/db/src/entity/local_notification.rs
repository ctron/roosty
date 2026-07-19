use crate::LocalNotificationType;
use sea_orm::entity::prelude::*;
use sea_orm::{ConnectionTrait, Statement};
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
    pub created_at: OffsetDateTime,
    pub dismissed_at: Option<OffsetDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

#[async_trait::async_trait]
impl ActiveModelBehavior for ActiveModel {
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
