use sea_orm::entity::prelude::*;

/// Ordered rule snapshot attached to a violation report.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "moderation_report_rule")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub report_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub position: i32,
    pub rule_id: Option<Uuid>,
    pub rule_text: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
