use sea_orm::entity::prelude::*;

/// Ordered status evidence attached to a moderation report.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "moderation_report_status")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub report_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub position: i32,
    pub local_status_id: Option<Uuid>,
    pub remote_status_id: Option<Uuid>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
