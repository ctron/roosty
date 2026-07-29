//! Mastodon-compatible client and administrator report APIs.

use axum::{
    Form, Json, Router,
    extract::{Path, Query, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use roosty_core::{AccountId, RoostyError, StatusId};
use roosty_db::{
    AdminAuditAction, AdminAuditSource, AdminAuditTargetKind, InstanceRule, ModerationReport,
    NewModerationReport, ReportAccount, ReportCategory, ReportListOptions, ReportStatus,
    assign_moderation_report, create_instance_rule as create_instance_rule_record,
    create_moderation_report, discard_instance_rule, enqueue_job_in_transaction,
    find_admin_account_by_id, find_local_account_by_id, find_local_status_by_id,
    find_moderation_report, find_remote_actor_by_id, find_remote_status_by_id,
    insert_admin_audit_entry, list_instance_rules, list_moderation_reports,
    notify_administrators_of_report, remote_status_visible_to_account,
    reorder_instance_rules as reorder_instance_rule_records, set_moderation_report_resolved,
    update_instance_rule as update_instance_rule_record, update_moderation_report,
};
use sea_orm::{DatabaseTransaction, TransactionTrait};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{borrow::Cow, collections::HashSet, str::FromStr};
use thiserror::Error;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    accounts::{RemoteAccountResponse, remote_account_response},
    admin::{
        AdminAccountResponse, AdminPermission, AdminReadPermission, AdminWritePermission,
        require_admin,
    },
    auth::{AccountResponse, AuthenticatedAccessToken, account_response},
    federation,
    http::AppState,
    notifications::publish_committed_notification,
    statuses::{
        StatusResponse, delete_reported_status, remote_status_response_for_viewer,
        status_response_for_viewer, status_visible_to_viewer,
    },
};

/// Mount client filing and administrator workflow routes.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/reports", post(create_report))
        .route("/api/v1/admin/reports", get(admin_reports))
        .route(
            "/api/v1/admin/reports/{report_id}",
            get(admin_report).put(update_admin_report),
        )
        .route(
            "/api/v1/admin/reports/{report_id}/assign_to_self",
            post(assign_report),
        )
        .route(
            "/api/v1/admin/reports/{report_id}/unassign",
            post(unassign_report),
        )
        .route(
            "/api/v1/admin/reports/{report_id}/resolve",
            post(resolve_report),
        )
        .route(
            "/api/v1/admin/reports/{report_id}/reopen",
            post(reopen_report),
        )
        .route(
            "/api/roosty/v1/admin/instance-rules",
            get(admin_instance_rules)
                .post(create_instance_rule)
                .put(reorder_instance_rules),
        )
        .route(
            "/api/roosty/v1/admin/instance-rules/{rule_id}",
            post(update_instance_rule).delete(delete_instance_rule),
        )
        .route(
            "/api/roosty/v1/admin/reports/{report_id}/statuses/{status_id}",
            axum::routing::delete(delete_report_status),
        )
}

#[derive(Deserialize)]
struct CreateReportForm {
    account_id: Uuid,
    #[serde(default, rename = "status_ids[]")]
    status_ids: Vec<Uuid>,
    #[serde(default, rename = "collection_ids[]")]
    collection_ids: Vec<Uuid>,
    #[serde(default)]
    comment: String,
    #[serde(default)]
    forward: bool,
    category: Option<String>,
    #[serde(default, rename = "rule_ids[]")]
    rule_ids: Vec<Uuid>,
}

#[derive(Serialize)]
struct ReportResponse {
    id: String,
    action_taken: bool,
    #[serde(with = "time::serde::rfc3339::option")]
    action_taken_at: Option<OffsetDateTime>,
    category: ReportCategory,
    comment: String,
    forwarded: bool,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    status_ids: Vec<String>,
    rule_ids: Vec<String>,
    target_account: ClientAccountResponse,
}

#[derive(Serialize)]
#[serde(untagged)]
enum ClientAccountResponse {
    Local(Box<AccountResponse>),
    Remote(Box<RemoteAccountResponse>),
}

type ReportApiResult<T> = Result<T, ReportApiError>;

/// Typed report boundary failures with one canonical Mastodon error projection.
#[derive(Debug, Error)]
enum ReportApiError {
    #[error("{0}")]
    Forbidden(Cow<'static, str>),
    #[error("{0}")]
    InvalidInput(Cow<'static, str>),
    #[error("Record not found")]
    NotFound,
    #[error(transparent)]
    Internal(#[from] RoostyError),
}

impl From<sea_orm::DbErr> for ReportApiError {
    fn from(error: sea_orm::DbErr) -> Self {
        Self::Internal(error.into())
    }
}

impl IntoResponse for ReportApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::Forbidden(message) => (StatusCode::FORBIDDEN, message),
            Self::InvalidInput(message) => (StatusCode::UNPROCESSABLE_ENTITY, message),
            Self::NotFound => (StatusCode::NOT_FOUND, Cow::Borrowed("Record not found")),
            Self::Internal(error) => {
                tracing::error!(%error, "report operation failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Cow::Borrowed("Internal server error"),
                )
            }
        };
        (status, Json(json!({"error": message}))).into_response()
    }
}

fn report_error(error: RoostyError) -> ReportApiError {
    match error {
        RoostyError::InvalidInput(reason) => ReportApiError::InvalidInput(Cow::Owned(reason)),
        error => ReportApiError::Internal(error),
    }
}

async fn create_report(
    State(state): State<AppState>,
    token: AuthenticatedAccessToken,
    Form(form): Form<CreateReportForm>,
) -> ReportApiResult<Response> {
    if !has_scope(&token, "write", "write:reports") {
        return Err(ReportApiError::Forbidden(Cow::Borrowed(
            "This action requires write:reports",
        )));
    }
    if !form.collection_ids.is_empty() {
        return Err(ReportApiError::InvalidInput(Cow::Borrowed(
            "Reporting collections is not supported",
        )));
    }
    if form.comment.chars().count() > 1_000 {
        return Err(ReportApiError::InvalidInput(Cow::Borrowed(
            "Comment is too long (maximum is 1000 characters)",
        )));
    }
    let target = report_target(&state, AccountId(form.account_id))
        .await?
        .ok_or(ReportApiError::InvalidInput(Cow::Borrowed(
            "Account does not exist",
        )))?;
    if target == ReportAccount::Local(token.grant.account.id) {
        return Err(ReportApiError::InvalidInput(Cow::Borrowed(
            "You cannot report your own account",
        )));
    }
    let statuses =
        validate_report_statuses(&state, token.grant.account.id, target, &form.status_ids)
            .await
            .map_err(report_error)?;
    let category = report_category(form.category.as_deref(), !form.rule_ids.is_empty())
        .map_err(|reason| ReportApiError::InvalidInput(Cow::Owned(reason)))?;
    let delivery = if form.forward {
        match target {
            ReportAccount::Remote(remote_actor_id) => Some(
                federation::prepare_report_flag(
                    &state,
                    token.grant.account.id,
                    remote_actor_id,
                    &statuses,
                    &form.comment,
                )
                .await
                .map_err(report_error)?,
            ),
            ReportAccount::Local(_) => None,
        }
    } else {
        None
    };
    let txn = state.db.begin().await?;
    let report = create_moderation_report(
        &txn,
        NewModerationReport {
            source: ReportAccount::Local(token.grant.account.id),
            target,
            category,
            comment: form.comment,
            forwarded: delivery.is_some(),
            activitypub_id: None,
            statuses,
            rule_ids: form.rule_ids,
        },
    )
    .await
    .map_err(report_error)?;
    let notifications = notify_administrators_of_report(&txn, &report).await?;
    if let Some(job) = delivery {
        enqueue_job_in_transaction(&txn, job)
            .await
            .map_err(report_error)?;
    }
    txn.commit().await?;
    for notification in notifications {
        publish_committed_notification(&state, notification.account_id, notification).await?;
    }
    Ok(Json(client_report_response(&state, report).await?).into_response())
}

#[derive(Default, Deserialize)]
struct AdminReportQuery {
    resolved: Option<bool>,
    account_id: Option<Uuid>,
    target_account_id: Option<Uuid>,
    max_id: Option<Uuid>,
    since_id: Option<Uuid>,
    min_id: Option<Uuid>,
    limit: Option<u64>,
}

async fn admin_reports(
    State(state): State<AppState>,
    token: AuthenticatedAccessToken,
    Query(query): Query<AdminReportQuery>,
) -> ReportApiResult<Response> {
    require_admin(&token, AdminPermission::Read(AdminReadPermission::Reports))
        .map_err(|error| ReportApiError::Forbidden(Cow::Owned(error.to_string())))?;
    let limit = query.limit.unwrap_or(40).clamp(1, 200);
    let reports = list_moderation_reports(
        &state.db,
        ReportListOptions {
            resolved: query.resolved,
            source_account_id: query.account_id.map(AccountId),
            target_account_id: query.target_account_id.map(AccountId),
            max_id: query.max_id,
            since_id: query.since_id,
            min_id: query.min_id,
            limit: limit.saturating_add(1),
        },
    )
    .await?;
    let has_more = reports.len() > limit as usize;
    let reports = reports.into_iter().take(limit as usize).collect::<Vec<_>>();
    let next = has_more
        .then(|| reports.last().map(|report| report.id))
        .flatten();
    let mut body = Vec::with_capacity(reports.len());
    for report in reports {
        body.push(admin_report_response(&state, report, token.grant.account.id).await?);
    }
    let mut response = Json(body).into_response();
    if let Some(next) = next {
        let link = format!(
            "<{}/api/v1/admin/reports?limit={limit}&max_id={next}>; rel=\"next\"",
            state.config.public_base_url.as_str().trim_end_matches('/')
        );
        if let Ok(value) = HeaderValue::from_str(&link) {
            response.headers_mut().insert(header::LINK, value);
        }
    }
    Ok(response)
}

async fn admin_report(
    State(state): State<AppState>,
    token: AuthenticatedAccessToken,
    Path(report_id): Path<Uuid>,
) -> ReportApiResult<Response> {
    require_admin(&token, AdminPermission::Read(AdminReadPermission::Reports))
        .map_err(|error| ReportApiError::Forbidden(Cow::Owned(error.to_string())))?;
    admin_report_by_id(&state, report_id, token.grant.account.id).await
}

#[derive(Deserialize)]
struct UpdateAdminReportForm {
    category: Option<String>,
    #[serde(default, rename = "rule_ids[]")]
    rule_ids: Vec<Uuid>,
}

async fn update_admin_report(
    State(state): State<AppState>,
    token: AuthenticatedAccessToken,
    Path(report_id): Path<Uuid>,
    Form(form): Form<UpdateAdminReportForm>,
) -> ReportApiResult<Response> {
    let actor = report_admin(&token, true)?;
    let category = report_category(form.category.as_deref(), !form.rule_ids.is_empty())
        .map_err(|reason| ReportApiError::InvalidInput(Cow::Owned(reason)))?;
    let txn = state.db.begin().await?;
    let report = update_moderation_report(&txn, report_id, category, &form.rule_ids)
        .await
        .map_err(report_error)?
        .ok_or(ReportApiError::NotFound)?;
    audit_report(
        &txn,
        actor,
        AdminAuditAction::ReportUpdate,
        report_id,
        json!({"category": category, "rule_ids": form.rule_ids}),
    )
    .await?;
    commit_and_project(&state, txn, report, actor).await
}

async fn assign_report(
    State(state): State<AppState>,
    token: AuthenticatedAccessToken,
    Path(report_id): Path<Uuid>,
) -> ReportApiResult<Response> {
    mutate_assignment(&state, &token, report_id, true).await
}

async fn unassign_report(
    State(state): State<AppState>,
    token: AuthenticatedAccessToken,
    Path(report_id): Path<Uuid>,
) -> ReportApiResult<Response> {
    mutate_assignment(&state, &token, report_id, false).await
}

async fn resolve_report(
    State(state): State<AppState>,
    token: AuthenticatedAccessToken,
    Path(report_id): Path<Uuid>,
) -> ReportApiResult<Response> {
    mutate_resolution(&state, &token, report_id, true).await
}

async fn reopen_report(
    State(state): State<AppState>,
    token: AuthenticatedAccessToken,
    Path(report_id): Path<Uuid>,
) -> ReportApiResult<Response> {
    mutate_resolution(&state, &token, report_id, false).await
}

async fn mutate_assignment(
    state: &AppState,
    token: &AuthenticatedAccessToken,
    report_id: Uuid,
    assign: bool,
) -> ReportApiResult<Response> {
    let actor = report_admin(token, true)?;
    let txn = state.db.begin().await?;
    let report = assign_moderation_report(&txn, report_id, assign.then_some(actor))
        .await?
        .ok_or(ReportApiError::NotFound)?;
    audit_report(
        &txn,
        actor,
        if assign {
            AdminAuditAction::ReportAssign
        } else {
            AdminAuditAction::ReportUnassign
        },
        report_id,
        json!({}),
    )
    .await?;
    commit_and_project(state, txn, report, actor).await
}

async fn mutate_resolution(
    state: &AppState,
    token: &AuthenticatedAccessToken,
    report_id: Uuid,
    resolve: bool,
) -> ReportApiResult<Response> {
    let actor = report_admin(token, true)?;
    let txn = state.db.begin().await?;
    let report = set_moderation_report_resolved(&txn, report_id, resolve.then_some(actor))
        .await
        .map_err(report_error)?
        .ok_or(ReportApiError::NotFound)?;
    audit_report(
        &txn,
        actor,
        if resolve {
            AdminAuditAction::ReportResolve
        } else {
            AdminAuditAction::ReportReopen
        },
        report_id,
        json!({}),
    )
    .await?;
    commit_and_project(state, txn, report, actor).await
}

async fn commit_and_project(
    state: &AppState,
    txn: DatabaseTransaction,
    report: ModerationReport,
    viewer: AccountId,
) -> ReportApiResult<Response> {
    txn.commit().await?;
    Ok(Json(admin_report_response(state, report, viewer).await?).into_response())
}

#[derive(Serialize)]
struct AdminReportResponse {
    id: String,
    action_taken: bool,
    #[serde(with = "time::serde::rfc3339::option")]
    action_taken_at: Option<OffsetDateTime>,
    category: ReportCategory,
    comment: String,
    forwarded: bool,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    updated_at: OffsetDateTime,
    account: AdminAccountResponse,
    target_account: AdminAccountResponse,
    assigned_account: Option<AdminAccountResponse>,
    action_taken_by_account: Option<AdminAccountResponse>,
    statuses: Vec<StatusResponse>,
    rules: Vec<AdminRuleResponse>,
}

#[derive(Serialize)]
struct AdminRuleResponse {
    id: String,
    text: String,
}

impl From<InstanceRule> for AdminRuleResponse {
    fn from(rule: InstanceRule) -> Self {
        Self {
            id: rule.id.to_string(),
            text: rule.text,
        }
    }
}

async fn admin_instance_rules(
    State(state): State<AppState>,
    token: AuthenticatedAccessToken,
) -> ReportApiResult<Json<Vec<AdminRuleResponse>>> {
    report_admin(&token, false)?;
    Ok(Json(
        list_instance_rules(&state.db)
            .await?
            .into_iter()
            .map(AdminRuleResponse::from)
            .collect(),
    ))
}

#[derive(Deserialize)]
struct RuleForm {
    text: String,
}

async fn create_instance_rule(
    State(state): State<AppState>,
    token: AuthenticatedAccessToken,
    Form(form): Form<RuleForm>,
) -> ReportApiResult<Json<AdminRuleResponse>> {
    let actor = report_admin(&token, true)?;
    let txn = state.db.begin().await?;
    let rule = create_instance_rule_record(&txn, &form.text)
        .await
        .map_err(report_error)?;
    audit_rule(&txn, actor, AdminAuditAction::InstanceRuleCreate, &rule).await?;
    txn.commit().await?;
    Ok(Json(rule.into()))
}

async fn update_instance_rule(
    State(state): State<AppState>,
    token: AuthenticatedAccessToken,
    Path(rule_id): Path<Uuid>,
    Form(form): Form<RuleForm>,
) -> ReportApiResult<Json<AdminRuleResponse>> {
    let actor = report_admin(&token, true)?;
    let txn = state.db.begin().await?;
    let rule = update_instance_rule_record(&txn, rule_id, &form.text)
        .await
        .map_err(report_error)?
        .ok_or(ReportApiError::NotFound)?;
    audit_rule(&txn, actor, AdminAuditAction::InstanceRuleUpdate, &rule).await?;
    txn.commit().await?;
    Ok(Json(rule.into()))
}

async fn delete_instance_rule(
    State(state): State<AppState>,
    token: AuthenticatedAccessToken,
    Path(rule_id): Path<Uuid>,
) -> ReportApiResult<Json<AdminRuleResponse>> {
    let actor = report_admin(&token, true)?;
    let txn = state.db.begin().await?;
    let rule = discard_instance_rule(&txn, rule_id)
        .await?
        .ok_or(ReportApiError::NotFound)?;
    audit_rule(&txn, actor, AdminAuditAction::InstanceRuleDelete, &rule).await?;
    txn.commit().await?;
    Ok(Json(rule.into()))
}

#[derive(Deserialize)]
struct ReorderRulesForm {
    #[serde(default, rename = "rule_ids[]")]
    rule_ids: Vec<Uuid>,
}

async fn reorder_instance_rules(
    State(state): State<AppState>,
    token: AuthenticatedAccessToken,
    Form(form): Form<ReorderRulesForm>,
) -> ReportApiResult<Json<Vec<AdminRuleResponse>>> {
    let actor = report_admin(&token, true)?;
    let txn = state.db.begin().await?;
    let rules = reorder_instance_rule_records(&txn, &form.rule_ids)
        .await
        .map_err(report_error)?;
    insert_admin_audit_entry(
        &txn,
        Some(actor),
        AdminAuditSource::Api,
        AdminAuditAction::InstanceRuleReorder,
        AdminAuditTargetKind::InstanceRule,
        "all",
        json!({"rule_ids": form.rule_ids}),
    )
    .await?;
    txn.commit().await?;
    Ok(Json(
        rules.into_iter().map(AdminRuleResponse::from).collect(),
    ))
}

async fn audit_rule(
    txn: &DatabaseTransaction,
    actor: AccountId,
    action: AdminAuditAction,
    rule: &InstanceRule,
) -> Result<(), RoostyError> {
    insert_admin_audit_entry(
        txn,
        Some(actor),
        AdminAuditSource::Api,
        action,
        AdminAuditTargetKind::InstanceRule,
        &rule.id.to_string(),
        json!({"text": rule.text}),
    )
    .await?;
    Ok(())
}

async fn audit_report(
    txn: &DatabaseTransaction,
    actor: AccountId,
    action: AdminAuditAction,
    report_id: Uuid,
    metadata: serde_json::Value,
) -> Result<(), RoostyError> {
    insert_admin_audit_entry(
        txn,
        Some(actor),
        AdminAuditSource::Api,
        action,
        AdminAuditTargetKind::Report,
        &report_id.to_string(),
        metadata,
    )
    .await?;
    Ok(())
}

async fn delete_report_status(
    State(state): State<AppState>,
    token: AuthenticatedAccessToken,
    Path((report_id, status_id)): Path<(Uuid, Uuid)>,
) -> ReportApiResult<StatusCode> {
    let actor = report_admin(&token, true)?;
    let report = find_moderation_report(&state.db, report_id)
        .await?
        .ok_or(ReportApiError::NotFound)?;
    let reference = report
        .statuses
        .into_iter()
        .find(|reference| match reference {
            ReportStatus::Local(id) | ReportStatus::Remote(id) => id.0 == status_id,
        })
        .ok_or(ReportApiError::NotFound)?;
    if !delete_reported_status(&state, reference).await? {
        return Err(ReportApiError::NotFound);
    }
    let txn = state.db.begin().await?;
    insert_admin_audit_entry(
        &txn,
        Some(actor),
        AdminAuditSource::Api,
        AdminAuditAction::ReportUpdate,
        AdminAuditTargetKind::Report,
        &report_id.to_string(),
        json!({"removed_status_id": status_id}),
    )
    .await?;
    txn.commit().await?;
    Ok(StatusCode::OK)
}

async fn admin_report_by_id(
    state: &AppState,
    id: Uuid,
    viewer: AccountId,
) -> ReportApiResult<Response> {
    let report = find_moderation_report(&state.db, id)
        .await?
        .ok_or(ReportApiError::NotFound)?;
    Ok(Json(admin_report_response(state, report, viewer).await?).into_response())
}

async fn admin_report_response(
    state: &AppState,
    report: ModerationReport,
    viewer: AccountId,
) -> Result<AdminReportResponse, RoostyError> {
    let account = admin_account(state, report.source).await?;
    let target_account = admin_account(state, report.target).await?;
    let assigned_account = match report.assigned_account_id {
        Some(id) => Some(admin_account(state, ReportAccount::Local(id)).await?),
        None => None,
    };
    let action_taken_by_account = match report.action_taken_by_account_id {
        Some(id) => Some(admin_account(state, ReportAccount::Local(id)).await?),
        None => None,
    };
    let mut statuses = Vec::with_capacity(report.statuses.len());
    for reference in &report.statuses {
        match reference {
            ReportStatus::Local(id) => {
                if let Some(status) = find_local_status_by_id(&state.db, *id).await?
                    && let Some(author) =
                        find_local_account_by_id(&state.db, status.account_id).await?
                {
                    statuses.push(
                        status_response_for_viewer(state, status, author, Some(viewer)).await?,
                    );
                }
            }
            ReportStatus::Remote(id) => {
                if let Some(status) = find_remote_status_by_id(&state.db, *id).await? {
                    statuses.push(
                        remote_status_response_for_viewer(state, status, Some(viewer)).await?,
                    );
                }
            }
        }
    }
    Ok(AdminReportResponse {
        id: report.id.to_string(),
        action_taken: report.action_taken_at.is_some(),
        action_taken_at: report.action_taken_at,
        category: report.category,
        comment: report.comment,
        forwarded: report.forwarded,
        created_at: report.created_at,
        updated_at: report.updated_at,
        account,
        target_account,
        assigned_account,
        action_taken_by_account,
        statuses,
        rules: report
            .rules
            .into_iter()
            .map(|rule| AdminRuleResponse {
                id: rule.id.map_or_else(String::new, |id| id.to_string()),
                text: rule.text,
            })
            .collect(),
    })
}

async fn client_report_response(
    state: &AppState,
    report: ModerationReport,
) -> Result<ReportResponse, RoostyError> {
    let target_account = match report.target {
        ReportAccount::Local(id) => {
            let account = find_local_account_by_id(&state.db, id)
                .await?
                .ok_or_else(|| {
                    RoostyError::InvalidInput("report target was not found".to_owned())
                })?;
            ClientAccountResponse::Local(Box::new(account_response(state, account).await?))
        }
        ReportAccount::Remote(id) => {
            let actor = find_remote_actor_by_id(&state.db, id)
                .await?
                .ok_or_else(|| {
                    RoostyError::InvalidInput("report target was not found".to_owned())
                })?;
            ClientAccountResponse::Remote(Box::new(remote_account_response(state, actor).await?))
        }
    };
    Ok(ReportResponse {
        id: report.id.to_string(),
        action_taken: report.action_taken_at.is_some(),
        action_taken_at: report.action_taken_at,
        category: report.category,
        comment: report.comment,
        forwarded: report.forwarded,
        created_at: report.created_at,
        status_ids: report
            .statuses
            .iter()
            .map(|status| match status {
                ReportStatus::Local(id) | ReportStatus::Remote(id) => id.0.to_string(),
            })
            .collect(),
        rule_ids: report
            .rules
            .iter()
            .filter_map(|rule| rule.id.map(|id| id.to_string()))
            .collect(),
        target_account,
    })
}

async fn report_target(
    state: &AppState,
    id: AccountId,
) -> Result<Option<ReportAccount>, RoostyError> {
    if find_local_account_by_id(&state.db, id).await?.is_some() {
        return Ok(Some(ReportAccount::Local(id)));
    }
    Ok(find_remote_actor_by_id(&state.db, id)
        .await?
        .map(|_| ReportAccount::Remote(id)))
}

async fn validate_report_statuses(
    state: &AppState,
    reporter: AccountId,
    target: ReportAccount,
    ids: &[Uuid],
) -> Result<Vec<ReportStatus>, RoostyError> {
    let mut seen = HashSet::new();
    let mut statuses = Vec::with_capacity(ids.len());
    for raw_id in ids {
        if !seen.insert(*raw_id) {
            return Err(RoostyError::InvalidInput(
                "status_ids contains a duplicate".to_owned(),
            ));
        }
        let id = StatusId(*raw_id);
        match target {
            ReportAccount::Local(target_id) => {
                let status = find_local_status_by_id(&state.db, id)
                    .await?
                    .ok_or_else(|| RoostyError::InvalidInput("status was not found".to_owned()))?;
                if status.account_id != target_id
                    || !status_visible_to_viewer(state, &status, Some(reporter)).await?
                {
                    return Err(RoostyError::InvalidInput(
                        "status does not belong to the reported account".to_owned(),
                    ));
                }
                statuses.push(ReportStatus::Local(id));
            }
            ReportAccount::Remote(target_id) => {
                let status = find_remote_status_by_id(&state.db, id)
                    .await?
                    .ok_or_else(|| RoostyError::InvalidInput("status was not found".to_owned()))?;
                if status.remote_actor_id != target_id
                    || !remote_status_visible_to_account(&state.db, &status, reporter).await?
                {
                    return Err(RoostyError::InvalidInput(
                        "status does not belong to the reported account".to_owned(),
                    ));
                }
                statuses.push(ReportStatus::Remote(id));
            }
        }
    }
    Ok(statuses)
}

async fn admin_account(
    state: &AppState,
    account: ReportAccount,
) -> Result<AdminAccountResponse, RoostyError> {
    let id = match account {
        ReportAccount::Local(id) | ReportAccount::Remote(id) => id,
    };
    find_admin_account_by_id(&state.db, id)
        .await?
        .map(AdminAccountResponse::from)
        .ok_or_else(|| RoostyError::InvalidInput("report account was not found".to_owned()))
}

fn report_category(value: Option<&str>, has_rules: bool) -> Result<ReportCategory, String> {
    if has_rules {
        return Ok(ReportCategory::Violation);
    }
    value.map_or(Ok(ReportCategory::Other), |value| {
        ReportCategory::from_str(value)
            .map_err(|_| "category must be spam, legal, violation, or other".to_owned())
    })
}

fn report_admin(token: &AuthenticatedAccessToken, write: bool) -> ReportApiResult<AccountId> {
    let permission = if write {
        AdminPermission::Write(AdminWritePermission::Reports)
    } else {
        AdminPermission::Read(AdminReadPermission::Reports)
    };
    require_admin(token, permission)
        .map_err(|error| ReportApiError::Forbidden(Cow::Owned(error.to_string())))
}

fn has_scope(token: &AuthenticatedAccessToken, broad: &str, specific: &str) -> bool {
    token
        .grant
        .scopes
        .split_ascii_whitespace()
        .any(|scope| scope == broad || scope == specific)
}

#[cfg(test)]
mod tests {
    use axum::response::IntoResponse;

    use super::*;

    /// Given rule IDs, when parsing a client category, then Mastodon's violation precedence wins.
    #[test]
    fn report_rules_select_violation_category() {
        assert_eq!(
            report_category(Some("spam"), true),
            Ok(ReportCategory::Violation)
        );
    }

    /// Given an unknown category, when parsing a report, then a typed validation failure is returned.
    #[test]
    fn unknown_report_category_is_rejected() {
        assert!(report_category(Some("harassment"), false).is_err());
    }

    /// Given a report boundary failure, when converted by Axum, then its stable HTTP status is used.
    #[test]
    fn typed_report_errors_select_http_statuses() {
        assert_eq!(
            ReportApiError::NotFound.into_response().status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            ReportApiError::InvalidInput(Cow::Borrowed("invalid"))
                .into_response()
                .status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
}
