//! Mastodon-compatible client and administrator report APIs.

use axum::{
    Extension, Form, Json, Router,
    extract::{Path, Query, State},
    http::{HeaderValue, StatusCode, header, header::InvalidHeaderValue},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
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
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{borrow::Cow, collections::HashSet, str::FromStr};
use strum::ParseError;
use thiserror::Error;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    accounts::{RemoteAccountResponse, remote_account_response},
    admin::{
        AdminAccountResponse, AdminAuthorizationError, AdminPermission, AdminReadPermission,
        AdminWritePermission, require_admin,
    },
    auth::{AccountResponse, AuthenticatedAccessToken, account_response},
    federation,
    http::{AppState, DatabaseContext, TransactionContext},
    notifications::publish_committed_notification,
    statuses::{
        StatusResponse, delete_reported_status, remote_status_response_for_viewer,
        status_response_for_viewer, status_visible_to_viewer_on,
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
            delete(delete_report_status),
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
    Internal(RoostyError),
}

impl From<RoostyError> for ReportApiError {
    fn from(error: RoostyError) -> Self {
        match error {
            RoostyError::InvalidInput(reason) => Self::InvalidInput(Cow::Owned(reason)),
            error => Self::Internal(error),
        }
    }
}

impl From<DbErr> for ReportApiError {
    fn from(error: DbErr) -> Self {
        Self::Internal(error.into())
    }
}

impl From<AdminAuthorizationError> for ReportApiError {
    fn from(error: AdminAuthorizationError) -> Self {
        Self::Forbidden(Cow::Owned(error.to_string()))
    }
}

impl From<InvalidHeaderValue> for ReportApiError {
    fn from(error: InvalidHeaderValue) -> Self {
        Self::Internal(RoostyError::Configuration(error.to_string()))
    }
}

impl From<String> for ReportApiError {
    fn from(reason: String) -> Self {
        Self::InvalidInput(Cow::Owned(reason))
    }
}

#[derive(Debug, Eq, Error, PartialEq)]
#[error("category must be spam, legal, violation, or other")]
struct InvalidReportCategory;

impl From<ParseError> for InvalidReportCategory {
    fn from(_: ParseError) -> Self {
        Self
    }
}

impl From<InvalidReportCategory> for ReportApiError {
    fn from(_: InvalidReportCategory) -> Self {
        Self::InvalidInput(Cow::Borrowed(
            "category must be spam, legal, violation, or other",
        ))
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

async fn create_report(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
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
    let txn = database.begin_write().await?;
    let target = report_target(&txn, AccountId(form.account_id))
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
        validate_report_statuses(&txn, token.grant.account.id, target, &form.status_ids).await?;
    let category = report_category(form.category.as_deref(), !form.rule_ids.is_empty())?;
    let delivery = if form.forward {
        match target {
            ReportAccount::Remote(remote_actor_id) => Some(
                federation::prepare_report_flag(
                    &state,
                    &txn,
                    token.grant.account.id,
                    remote_actor_id,
                    &statuses,
                    &form.comment,
                )
                .await?,
            ),
            ReportAccount::Local(_) => None,
        }
    } else {
        None
    };
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
    .await?;
    let notifications = notify_administrators_of_report(&txn, &report).await?;
    if let Some(job) = delivery {
        enqueue_job_in_transaction(&txn, job).await?;
    }
    let context = TransactionContext::new(&state, &txn);
    let response = client_report_response(&context, report).await?;
    txn.commit().await?;
    for notification in notifications {
        publish_committed_notification(&state, &database, notification.account_id, notification)
            .await?;
    }
    Ok(Json(response).into_response())
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
    Extension(database): Extension<DatabaseContext>,
    token: AuthenticatedAccessToken,
    Query(query): Query<AdminReportQuery>,
) -> ReportApiResult<Response> {
    require_admin(&token, AdminPermission::Read(AdminReadPermission::Reports))?;
    let limit = query.limit.unwrap_or(40).clamp(1, 200);
    let txn = database.begin_snapshot().await?;
    let context = TransactionContext::new(&state, &txn);
    let reports = list_moderation_reports(
        &txn,
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
        body.push(admin_report_response(&context, report, token.grant.account.id).await?);
    }
    let mut response = Json(body).into_response();
    if let Some(next) = next {
        let link = format!(
            "<{}/api/v1/admin/reports?limit={limit}&max_id={next}>; rel=\"next\"",
            state.config.public_base_url.as_str().trim_end_matches('/')
        );
        response
            .headers_mut()
            .insert(header::LINK, HeaderValue::from_str(&link)?);
    }
    txn.commit().await?;
    Ok(response)
}

async fn admin_report(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    token: AuthenticatedAccessToken,
    Path(report_id): Path<Uuid>,
) -> ReportApiResult<Response> {
    require_admin(&token, AdminPermission::Read(AdminReadPermission::Reports))?;
    admin_report_by_id(&state, &database, report_id, token.grant.account.id).await
}

#[derive(Deserialize)]
struct UpdateAdminReportForm {
    category: Option<String>,
    #[serde(default, rename = "rule_ids[]")]
    rule_ids: Vec<Uuid>,
}

async fn update_admin_report(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    token: AuthenticatedAccessToken,
    Path(report_id): Path<Uuid>,
    Form(form): Form<UpdateAdminReportForm>,
) -> ReportApiResult<Response> {
    let actor = report_admin(&token, true)?;
    let category = report_category(form.category.as_deref(), !form.rule_ids.is_empty())?;
    let txn = database.begin_write().await?;
    let report = update_moderation_report(&txn, report_id, category, &form.rule_ids)
        .await?
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
    Extension(database): Extension<DatabaseContext>,
    token: AuthenticatedAccessToken,
    Path(report_id): Path<Uuid>,
) -> ReportApiResult<Response> {
    mutate_assignment(&state, &database, &token, report_id, true).await
}

async fn unassign_report(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    token: AuthenticatedAccessToken,
    Path(report_id): Path<Uuid>,
) -> ReportApiResult<Response> {
    mutate_assignment(&state, &database, &token, report_id, false).await
}

async fn resolve_report(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    token: AuthenticatedAccessToken,
    Path(report_id): Path<Uuid>,
) -> ReportApiResult<Response> {
    mutate_resolution(&state, &database, &token, report_id, true).await
}

async fn reopen_report(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    token: AuthenticatedAccessToken,
    Path(report_id): Path<Uuid>,
) -> ReportApiResult<Response> {
    mutate_resolution(&state, &database, &token, report_id, false).await
}

async fn mutate_assignment(
    state: &AppState,
    database: &DatabaseContext,
    token: &AuthenticatedAccessToken,
    report_id: Uuid,
    assign: bool,
) -> ReportApiResult<Response> {
    let actor = report_admin(token, true)?;
    let txn = database.begin_write().await?;
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
    database: &DatabaseContext,
    token: &AuthenticatedAccessToken,
    report_id: Uuid,
    resolve: bool,
) -> ReportApiResult<Response> {
    let actor = report_admin(token, true)?;
    let txn = database.begin_write().await?;
    let report = set_moderation_report_resolved(&txn, report_id, resolve.then_some(actor))
        .await?
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
    let context = TransactionContext::new(state, &txn);
    let response = admin_report_response(&context, report, viewer).await?;
    txn.commit().await?;
    Ok(Json(response).into_response())
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
    Extension(database): Extension<DatabaseContext>,
    token: AuthenticatedAccessToken,
) -> ReportApiResult<Json<Vec<AdminRuleResponse>>> {
    report_admin(&token, false)?;
    let txn = database.begin_snapshot().await?;
    let rules = list_instance_rules(&txn).await?;
    txn.commit().await?;
    Ok(Json(
        rules.into_iter().map(AdminRuleResponse::from).collect(),
    ))
}

#[derive(Deserialize)]
struct RuleForm {
    text: String,
}

async fn create_instance_rule(
    Extension(database): Extension<DatabaseContext>,
    token: AuthenticatedAccessToken,
    Form(form): Form<RuleForm>,
) -> ReportApiResult<Json<AdminRuleResponse>> {
    let actor = report_admin(&token, true)?;
    let txn = database.begin_write().await?;
    let rule = create_instance_rule_record(&txn, &form.text).await?;
    audit_rule(&txn, actor, AdminAuditAction::InstanceRuleCreate, &rule).await?;
    txn.commit().await?;
    Ok(Json(rule.into()))
}

async fn update_instance_rule(
    Extension(database): Extension<DatabaseContext>,
    token: AuthenticatedAccessToken,
    Path(rule_id): Path<Uuid>,
    Form(form): Form<RuleForm>,
) -> ReportApiResult<Json<AdminRuleResponse>> {
    let actor = report_admin(&token, true)?;
    let txn = database.begin_write().await?;
    let rule = update_instance_rule_record(&txn, rule_id, &form.text)
        .await?
        .ok_or(ReportApiError::NotFound)?;
    audit_rule(&txn, actor, AdminAuditAction::InstanceRuleUpdate, &rule).await?;
    txn.commit().await?;
    Ok(Json(rule.into()))
}

async fn delete_instance_rule(
    Extension(database): Extension<DatabaseContext>,
    token: AuthenticatedAccessToken,
    Path(rule_id): Path<Uuid>,
) -> ReportApiResult<Json<AdminRuleResponse>> {
    let actor = report_admin(&token, true)?;
    let txn = database.begin_write().await?;
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
    Extension(database): Extension<DatabaseContext>,
    token: AuthenticatedAccessToken,
    Form(form): Form<ReorderRulesForm>,
) -> ReportApiResult<Json<Vec<AdminRuleResponse>>> {
    let actor = report_admin(&token, true)?;
    let txn = database.begin_write().await?;
    let rules = reorder_instance_rule_records(&txn, &form.rule_ids).await?;
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
    metadata: Value,
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
    Extension(database): Extension<DatabaseContext>,
    token: AuthenticatedAccessToken,
    Path((report_id, status_id)): Path<(Uuid, Uuid)>,
) -> ReportApiResult<StatusCode> {
    let actor = report_admin(&token, true)?;
    let txn = database.begin_write().await?;
    let report = find_moderation_report(&txn, report_id)
        .await?
        .ok_or(ReportApiError::NotFound)?;
    let reference = report
        .statuses
        .into_iter()
        .find(|reference| match reference {
            ReportStatus::Local(id) | ReportStatus::Remote(id) => id.0 == status_id,
        })
        .ok_or(ReportApiError::NotFound)?;
    let context = TransactionContext::new(&state, &txn);
    if !delete_reported_status(&context, reference).await? {
        return Err(ReportApiError::NotFound);
    }
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
    database: &DatabaseContext,
    id: Uuid,
    viewer: AccountId,
) -> ReportApiResult<Response> {
    let txn = database.begin_snapshot().await?;
    let report = find_moderation_report(&txn, id)
        .await?
        .ok_or(ReportApiError::NotFound)?;
    let context = TransactionContext::new(state, &txn);
    let response = admin_report_response(&context, report, viewer).await?;
    txn.commit().await?;
    Ok(Json(response).into_response())
}

async fn admin_report_response(
    state: &TransactionContext<'_, impl ConnectionTrait>,
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
                if let Some(status) = find_local_status_by_id(state.db, *id).await?
                    && let Some(author) =
                        find_local_account_by_id(state.db, status.account_id).await?
                {
                    statuses.push(
                        status_response_for_viewer(state, status, author, Some(viewer)).await?,
                    );
                }
            }
            ReportStatus::Remote(id) => {
                if let Some(status) = find_remote_status_by_id(state.db, *id).await? {
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
    state: &TransactionContext<'_, impl ConnectionTrait>,
    report: ModerationReport,
) -> Result<ReportResponse, RoostyError> {
    let target_account = match report.target {
        ReportAccount::Local(id) => {
            let account = find_local_account_by_id(state.db, id)
                .await?
                .ok_or_else(|| {
                    RoostyError::InvalidInput("report target was not found".to_owned())
                })?;
            ClientAccountResponse::Local(Box::new(
                account_response(state, state.db, account).await?,
            ))
        }
        ReportAccount::Remote(id) => {
            let actor = find_remote_actor_by_id(state.db, id)
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
    db: &impl ConnectionTrait,
    id: AccountId,
) -> Result<Option<ReportAccount>, RoostyError> {
    if find_local_account_by_id(db, id).await?.is_some() {
        return Ok(Some(ReportAccount::Local(id)));
    }
    Ok(find_remote_actor_by_id(db, id)
        .await?
        .map(|_| ReportAccount::Remote(id)))
}

async fn validate_report_statuses(
    db: &impl ConnectionTrait,
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
                let status = find_local_status_by_id(db, id)
                    .await?
                    .ok_or_else(|| RoostyError::InvalidInput("status was not found".to_owned()))?;
                if status.account_id != target_id
                    || !status_visible_to_viewer_on(db, &status, Some(reporter)).await?
                {
                    return Err(RoostyError::InvalidInput(
                        "status does not belong to the reported account".to_owned(),
                    ));
                }
                statuses.push(ReportStatus::Local(id));
            }
            ReportAccount::Remote(target_id) => {
                let status = find_remote_status_by_id(db, id)
                    .await?
                    .ok_or_else(|| RoostyError::InvalidInput("status was not found".to_owned()))?;
                if status.remote_actor_id != target_id
                    || !remote_status_visible_to_account(db, &status, reporter).await?
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
    state: &TransactionContext<'_, impl ConnectionTrait>,
    account: ReportAccount,
) -> Result<AdminAccountResponse, RoostyError> {
    let id = match account {
        ReportAccount::Local(id) | ReportAccount::Remote(id) => id,
    };
    find_admin_account_by_id(state.db, id)
        .await?
        .map(AdminAccountResponse::from)
        .ok_or_else(|| RoostyError::InvalidInput("report account was not found".to_owned()))
}

fn report_category(
    value: Option<&str>,
    has_rules: bool,
) -> Result<ReportCategory, InvalidReportCategory> {
    if has_rules {
        return Ok(ReportCategory::Violation);
    }
    if let Some(value) = value {
        Ok(ReportCategory::from_str(value)?)
    } else {
        Ok(ReportCategory::Other)
    }
}

fn report_admin(token: &AuthenticatedAccessToken, write: bool) -> ReportApiResult<AccountId> {
    let permission = if write {
        AdminPermission::Write(AdminWritePermission::Reports)
    } else {
        AdminPermission::Read(AdminReadPermission::Reports)
    };
    Ok(require_admin(token, permission)?)
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
