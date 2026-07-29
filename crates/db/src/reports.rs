//! Durable instance rules and Mastodon-compatible moderation reports.

use crate::{
    DbConnection,
    entity::{instance_rule, moderation_report, moderation_report_rule, moderation_report_status},
};
use roosty_core::{AccountId, Result, RoostyError, StatusId};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseTransaction, EntityTrait,
    IntoActiveModel, QueryFilter, QueryOrder, QuerySelect, Set,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use strum::{Display, EnumString, IntoStaticStr};
use time::OffsetDateTime;
use uuid::Uuid;

/// Closed Mastodon report categories.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    sea_orm::DeriveValueType,
    Display,
    EnumString,
    Eq,
    IntoStaticStr,
    PartialEq,
    Serialize,
    Deserialize,
)]
#[sea_orm(value_type = "String")]
#[strum(serialize_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ReportCategory {
    Spam,
    Legal,
    Violation,
    #[default]
    Other,
}

/// One active or historically retired instance rule.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstanceRule {
    pub id: Uuid,
    pub text: String,
    pub position: i32,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub discarded_at: Option<OffsetDateTime>,
}

/// Local or cached-remote account referenced by a report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReportAccount {
    Local(AccountId),
    Remote(AccountId),
}

/// Local or cached-remote status evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReportStatus {
    Local(StatusId),
    Remote(StatusId),
}

/// Immutable rule snapshot attached to a report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReportRule {
    pub id: Option<Uuid>,
    pub text: String,
}

/// One report and all evidence required by client and administrator projections.
#[derive(Clone, Debug)]
pub struct ModerationReport {
    pub id: Uuid,
    pub source: ReportAccount,
    pub target: ReportAccount,
    pub category: ReportCategory,
    pub comment: String,
    pub forwarded: bool,
    pub activitypub_id: Option<String>,
    pub assigned_account_id: Option<AccountId>,
    pub action_taken_by_account_id: Option<AccountId>,
    pub action_taken_at: Option<OffsetDateTime>,
    pub statuses: Vec<ReportStatus>,
    pub rules: Vec<ReportRule>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// Fully validated report input inserted through a caller-owned transaction.
pub struct NewModerationReport {
    pub source: ReportAccount,
    pub target: ReportAccount,
    pub category: ReportCategory,
    pub comment: String,
    pub forwarded: bool,
    pub activitypub_id: Option<String>,
    pub statuses: Vec<ReportStatus>,
    pub rule_ids: Vec<Uuid>,
}

/// Cursor and account filters supported by Mastodon's administrator report list.
#[derive(Clone, Copy, Debug, Default)]
pub struct ReportListOptions {
    pub resolved: Option<bool>,
    pub source_account_id: Option<AccountId>,
    pub target_account_id: Option<AccountId>,
    pub max_id: Option<Uuid>,
    pub since_id: Option<Uuid>,
    pub min_id: Option<Uuid>,
    pub limit: u64,
}

/// Return active rules in the exact order advertised to clients.
pub async fn list_instance_rules(db: &impl ConnectionTrait) -> Result<Vec<InstanceRule>> {
    Ok(instance_rule::Entity::find()
        .filter(instance_rule::Column::DiscardedAt.is_null())
        .order_by_asc(instance_rule::Column::Position)
        .all(db)
        .await?
        .into_iter()
        .map(instance_rule_from_model)
        .collect())
}

/// Add a rule at the end of the active ordered list.
pub async fn create_instance_rule(txn: &DatabaseTransaction, text: &str) -> Result<InstanceRule> {
    validate_rule_text(text)?;
    lock_instance_rules(txn).await?;
    let position = instance_rule::Entity::find()
        .filter(instance_rule::Column::DiscardedAt.is_null())
        .order_by_desc(instance_rule::Column::Position)
        .one(txn)
        .await?
        .map_or(0, |rule| rule.position.saturating_add(1));
    let model = instance_rule::ActiveModel {
        id: Set(Uuid::now_v7()),
        text: Set(text.trim().to_owned()),
        position: Set(position),
        ..Default::default()
    }
    .insert(txn)
    .await?;
    Ok(instance_rule_from_model(model))
}

/// Change an active rule's text while preserving its identity and order.
pub async fn update_instance_rule(
    txn: &DatabaseTransaction,
    id: Uuid,
    text: &str,
) -> Result<Option<InstanceRule>> {
    validate_rule_text(text)?;
    lock_instance_rules(txn).await?;
    let Some(model) = instance_rule::Entity::find_by_id(id)
        .filter(instance_rule::Column::DiscardedAt.is_null())
        .one(txn)
        .await?
    else {
        return Ok(None);
    };
    let mut active = model.into_active_model();
    active.text = Set(text.trim().to_owned());
    active.updated_at = Set(OffsetDateTime::now_utc());
    Ok(Some(instance_rule_from_model(active.update(txn).await?)))
}

/// Retire a rule without invalidating historical report snapshots.
pub async fn discard_instance_rule(
    txn: &DatabaseTransaction,
    id: Uuid,
) -> Result<Option<InstanceRule>> {
    lock_instance_rules(txn).await?;
    let Some(model) = instance_rule::Entity::find_by_id(id)
        .filter(instance_rule::Column::DiscardedAt.is_null())
        .one(txn)
        .await?
    else {
        return Ok(None);
    };
    let now = OffsetDateTime::now_utc();
    let mut active = model.into_active_model();
    active.discarded_at = Set(Some(now));
    active.updated_at = Set(now);
    Ok(Some(instance_rule_from_model(active.update(txn).await?)))
}

/// Replace active rule ordering using the complete current set of IDs.
pub async fn reorder_instance_rules(
    txn: &DatabaseTransaction,
    ordered_ids: &[Uuid],
) -> Result<Vec<InstanceRule>> {
    lock_instance_rules(txn).await?;
    let current = list_instance_rules(txn).await?;
    let mut expected = current.iter().map(|rule| rule.id).collect::<Vec<_>>();
    let mut supplied = ordered_ids.to_vec();
    expected.sort_unstable();
    supplied.sort_unstable();
    if expected != supplied {
        return Err(RoostyError::InvalidInput(
            "rule order must contain every active rule exactly once".to_owned(),
        ));
    }
    // Move through temporary out-of-band positions so the partial unique index is never violated.
    for (index, id) in ordered_ids.iter().enumerate() {
        let model = instance_rule::Entity::find_by_id(*id)
            .one(txn)
            .await?
            .ok_or_else(|| RoostyError::InvalidInput("instance rule was not found".to_owned()))?;
        let mut active = model.into_active_model();
        active.position = Set(1_000_000_i32.saturating_add(index as i32));
        active.update(txn).await?;
    }
    for (index, id) in ordered_ids.iter().enumerate() {
        let model = instance_rule::Entity::find_by_id(*id)
            .one(txn)
            .await?
            .ok_or_else(|| RoostyError::InvalidInput("instance rule was not found".to_owned()))?;
        let mut active = model.into_active_model();
        active.position = Set(index as i32);
        active.updated_at = Set(OffsetDateTime::now_utc());
        active.update(txn).await?;
    }
    list_instance_rules(txn).await
}

/// Persist a validated report and its ordered evidence.
pub async fn create_moderation_report(
    txn: &DatabaseTransaction,
    input: NewModerationReport,
) -> Result<ModerationReport> {
    if input.comment.chars().count() > 1_000 {
        return Err(RoostyError::InvalidInput(
            "report comment must not exceed 1000 characters".to_owned(),
        ));
    }
    let rules = active_rule_snapshots(txn, &input.rule_ids).await?;
    if input.category == ReportCategory::Violation && rules.is_empty() {
        return Err(RoostyError::InvalidInput(
            "violation reports require at least one instance rule".to_owned(),
        ));
    }
    let (source_local_account_id, source_remote_actor_id) = report_account_columns(input.source);
    let (target_local_account_id, target_remote_actor_id) = report_account_columns(input.target);
    let id = Uuid::now_v7();
    moderation_report::ActiveModel {
        id: Set(id),
        source_local_account_id: Set(source_local_account_id),
        source_remote_actor_id: Set(source_remote_actor_id),
        target_local_account_id: Set(target_local_account_id),
        target_remote_actor_id: Set(target_remote_actor_id),
        category: Set(input.category),
        comment: Set(input.comment),
        forwarded: Set(input.forwarded),
        activitypub_id: Set(input.activitypub_id),
        ..Default::default()
    }
    .insert(txn)
    .await?;
    for (position, status) in input.statuses.into_iter().enumerate() {
        let (local_status_id, remote_status_id) = match status {
            ReportStatus::Local(id) => (Some(id.0), None),
            ReportStatus::Remote(id) => (None, Some(id.0)),
        };
        moderation_report_status::ActiveModel {
            report_id: Set(id),
            position: Set(position as i32),
            local_status_id: Set(local_status_id),
            remote_status_id: Set(remote_status_id),
        }
        .insert(txn)
        .await?;
    }
    replace_report_rule_snapshots(txn, id, rules).await?;
    find_moderation_report(txn, id)
        .await?
        .ok_or_else(|| RoostyError::InvalidInput("created report was not found".to_owned()))
}

/// Find one report with status evidence and rule snapshots.
pub async fn find_moderation_report(
    db: &impl ConnectionTrait,
    id: Uuid,
) -> Result<Option<ModerationReport>> {
    let Some(model) = moderation_report::Entity::find_by_id(id).one(db).await? else {
        return Ok(None);
    };
    let mut reports = hydrate_reports(db, vec![model]).await?;
    Ok(reports.pop())
}

/// Return administrator reports newest-first with Mastodon-compatible cursor filters.
pub async fn list_moderation_reports(
    db: &DbConnection,
    options: ReportListOptions,
) -> Result<Vec<ModerationReport>> {
    let mut query = moderation_report::Entity::find()
        .order_by_desc(moderation_report::Column::Id)
        .limit(options.limit.clamp(1, 200));
    if let Some(resolved) = options.resolved {
        query = if resolved {
            query.filter(moderation_report::Column::ActionTakenAt.is_not_null())
        } else {
            query.filter(moderation_report::Column::ActionTakenAt.is_null())
        };
    }
    if let Some(id) = options.source_account_id {
        query = query.filter(
            moderation_report::Column::SourceLocalAccountId
                .eq(id.0)
                .or(moderation_report::Column::SourceRemoteActorId.eq(id.0)),
        );
    }
    if let Some(id) = options.target_account_id {
        query = query.filter(
            moderation_report::Column::TargetLocalAccountId
                .eq(id.0)
                .or(moderation_report::Column::TargetRemoteActorId.eq(id.0)),
        );
    }
    if let Some(id) = options.max_id {
        query = query.filter(moderation_report::Column::Id.lt(id));
    }
    if let Some(id) = options.since_id {
        query = query.filter(moderation_report::Column::Id.gt(id));
    }
    if let Some(id) = options.min_id {
        query = query.filter(moderation_report::Column::Id.gt(id));
    }
    hydrate_reports(db, query.all(db).await?).await
}

/// Update category and rule snapshots for a report.
pub async fn update_moderation_report(
    txn: &DatabaseTransaction,
    id: Uuid,
    category: ReportCategory,
    rule_ids: &[Uuid],
) -> Result<Option<ModerationReport>> {
    let Some(model) = moderation_report::Entity::find_by_id(id)
        .lock_exclusive()
        .one(txn)
        .await?
    else {
        return Ok(None);
    };
    let rules = active_rule_snapshots(txn, rule_ids).await?;
    if category == ReportCategory::Violation && rules.is_empty() {
        return Err(RoostyError::InvalidInput(
            "violation reports require at least one instance rule".to_owned(),
        ));
    }
    let mut active = model.into_active_model();
    active.category = Set(category);
    active.updated_at = Set(OffsetDateTime::now_utc());
    active.update(txn).await?;
    moderation_report_rule::Entity::delete_many()
        .filter(moderation_report_rule::Column::ReportId.eq(id))
        .exec(txn)
        .await?;
    replace_report_rule_snapshots(txn, id, rules).await?;
    find_moderation_report(txn, id).await
}

/// Assign a report to the acting administrator, replacing any previous assignment.
pub async fn assign_moderation_report(
    txn: &DatabaseTransaction,
    id: Uuid,
    assignee: Option<AccountId>,
) -> Result<Option<ModerationReport>> {
    let Some(model) = moderation_report::Entity::find_by_id(id)
        .lock_exclusive()
        .one(txn)
        .await?
    else {
        return Ok(None);
    };
    let mut active = model.into_active_model();
    active.assigned_account_id = Set(assignee.map(|account| account.0));
    active.updated_at = Set(OffsetDateTime::now_utc());
    active.update(txn).await?;
    find_moderation_report(txn, id).await
}

/// Resolve or reopen a report idempotently.
pub async fn set_moderation_report_resolved(
    txn: &DatabaseTransaction,
    id: Uuid,
    actor: Option<AccountId>,
) -> Result<Option<ModerationReport>> {
    let Some(model) = moderation_report::Entity::find_by_id(id)
        .lock_exclusive()
        .one(txn)
        .await?
    else {
        return Ok(None);
    };
    let mut active = model.into_active_model();
    let now = OffsetDateTime::now_utc();
    active.action_taken_by_account_id = Set(actor.map(|account| account.0));
    active.action_taken_at = Set(actor.map(|_| now));
    active.updated_at = Set(now);
    active.update(txn).await?;
    find_moderation_report(txn, id).await
}

/// Serialize rule mutations across application processes so positions remain unique and contiguous.
async fn lock_instance_rules(txn: &DatabaseTransaction) -> Result<()> {
    // PostgreSQL table locking also serializes the first insert, when no row exists to lock.
    txn.execute_unprepared("LOCK TABLE instance_rule IN SHARE ROW EXCLUSIVE MODE")
        .await?;
    Ok(())
}

fn validate_rule_text(text: &str) -> Result<()> {
    let length = text.trim().chars().count();
    if length == 0 || length > 300 {
        return Err(RoostyError::InvalidInput(
            "instance rule text must contain between 1 and 300 characters".to_owned(),
        ));
    }
    Ok(())
}

async fn active_rule_snapshots(
    db: &impl ConnectionTrait,
    ids: &[Uuid],
) -> Result<Vec<(Option<Uuid>, String)>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let models = instance_rule::Entity::find()
        .filter(instance_rule::Column::Id.is_in(ids.iter().copied()))
        .filter(instance_rule::Column::DiscardedAt.is_null())
        .all(db)
        .await?;
    let by_id = models
        .into_iter()
        .map(|rule| (rule.id, rule.text))
        .collect::<HashMap<_, _>>();
    ids.iter()
        .map(|id| {
            by_id
                .get(id)
                .cloned()
                .map(|text| (Some(*id), text))
                .ok_or_else(|| RoostyError::InvalidInput("instance rule was not found".to_owned()))
        })
        .collect()
}

async fn replace_report_rule_snapshots(
    txn: &DatabaseTransaction,
    report_id: Uuid,
    rules: Vec<(Option<Uuid>, String)>,
) -> Result<()> {
    for (position, (rule_id, rule_text)) in rules.into_iter().enumerate() {
        moderation_report_rule::ActiveModel {
            report_id: Set(report_id),
            position: Set(position as i32),
            rule_id: Set(rule_id),
            rule_text: Set(rule_text),
        }
        .insert(txn)
        .await?;
    }
    Ok(())
}

async fn hydrate_reports(
    db: &impl ConnectionTrait,
    models: Vec<moderation_report::Model>,
) -> Result<Vec<ModerationReport>> {
    if models.is_empty() {
        return Ok(Vec::new());
    }
    let ids = models.iter().map(|report| report.id).collect::<Vec<_>>();
    let status_rows = moderation_report_status::Entity::find()
        .filter(moderation_report_status::Column::ReportId.is_in(ids.iter().copied()))
        .order_by_asc(moderation_report_status::Column::Position)
        .all(db)
        .await?;
    let rule_rows = moderation_report_rule::Entity::find()
        .filter(moderation_report_rule::Column::ReportId.is_in(ids))
        .order_by_asc(moderation_report_rule::Column::Position)
        .all(db)
        .await?;
    let mut statuses = HashMap::<Uuid, Vec<ReportStatus>>::new();
    for row in status_rows {
        if let Some(id) = row.local_status_id {
            statuses
                .entry(row.report_id)
                .or_default()
                .push(ReportStatus::Local(StatusId(id)));
        } else if let Some(id) = row.remote_status_id {
            statuses
                .entry(row.report_id)
                .or_default()
                .push(ReportStatus::Remote(StatusId(id)));
        }
    }
    let mut rules = HashMap::<Uuid, Vec<ReportRule>>::new();
    for row in rule_rows {
        rules.entry(row.report_id).or_default().push(ReportRule {
            id: row.rule_id,
            text: row.rule_text,
        });
    }
    models
        .into_iter()
        .map(|model| {
            let source =
                report_account(model.source_local_account_id, model.source_remote_actor_id)?;
            let target =
                report_account(model.target_local_account_id, model.target_remote_actor_id)?;
            Ok(ModerationReport {
                id: model.id,
                source,
                target,
                category: model.category,
                comment: model.comment,
                forwarded: model.forwarded,
                activitypub_id: model.activitypub_id,
                assigned_account_id: model.assigned_account_id.map(AccountId),
                action_taken_by_account_id: model.action_taken_by_account_id.map(AccountId),
                action_taken_at: model.action_taken_at,
                statuses: statuses.remove(&model.id).unwrap_or_default(),
                rules: rules.remove(&model.id).unwrap_or_default(),
                created_at: model.created_at,
                updated_at: model.updated_at,
            })
        })
        .collect()
}

fn report_account_columns(account: ReportAccount) -> (Option<Uuid>, Option<Uuid>) {
    match account {
        ReportAccount::Local(id) => (Some(id.0), None),
        ReportAccount::Remote(id) => (None, Some(id.0)),
    }
}

fn report_account(local: Option<Uuid>, remote: Option<Uuid>) -> Result<ReportAccount> {
    match (local, remote) {
        (Some(id), None) => Ok(ReportAccount::Local(AccountId(id))),
        (None, Some(id)) => Ok(ReportAccount::Remote(AccountId(id))),
        _ => Err(RoostyError::InvalidInput(
            "report account reference is inconsistent".to_owned(),
        )),
    }
}

fn instance_rule_from_model(model: instance_rule::Model) -> InstanceRule {
    InstanceRule {
        id: model.id,
        text: model.text,
        position: model.position,
        created_at: model.created_at,
        updated_at: model.updated_at,
        discarded_at: model.discarded_at,
    }
}
