use axum::{
    Extension, Form, Json, Router,
    extract::{Path, RawForm, RawQuery, State},
    http::header,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use roosty_core::{AccountId, RoostyError, StatusId};
use roosty_db::{
    CollectionCursor, CollectionPage, DbConnection, LocalNotification, LocalNotificationType,
    LocalTimeline, ModerationReport, NotificationActor, NotificationFilter, NotificationGroup,
    NotificationPolicyAction, NotificationPolicyUpdate, NotificationRequest, ReportAccount,
    ReportCategory, ReportStatus, find_moderation_report,
};
use sea_orm::ConnectionTrait;
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};
use std::{
    borrow::Cow,
    collections::HashSet,
    future::Future,
    pin::Pin,
    str::{FromStr, Utf8Error},
};
use thiserror::Error;
use time::OffsetDateTime;
use url::Url;
use uuid::{Error, Uuid};

use crate::{
    accounts::RemoteAccountResponse,
    auth::{AccountResponse, AuthenticatedAccount, account_response},
    http::{ApiError, ApiResult, AppState, DatabaseContext, TransactionContext},
    statuses::{CollectionLink, StatusResponse, remote_status_response},
};

const DEFAULT_NOTIFICATION_LIMIT: u64 = 40;
const MAX_NOTIFICATION_LIMIT: u64 = 80;

#[derive(Debug, Error)]
enum NotificationInputError {
    #[error("notification query is invalid")]
    Query(#[from] serde_qs::Error),
    #[error("notification form body is not UTF-8")]
    Utf8(#[from] Utf8Error),
    #[error("notification cursor is invalid")]
    Uuid(#[from] Error),
}

impl From<NotificationInputError> for ApiError {
    fn from(error: NotificationInputError) -> Self {
        Self::BadRequest(Cow::Owned(error.to_string()))
    }
}

/// Build routes for Mastodon-compatible notification collections.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/notifications", get(notifications))
        .route("/api/v1/notifications/unread_count", get(unread_count))
        .route("/api/v1/notifications/clear", post(clear_notifications))
        .route(
            "/api/v1/notifications/{notification_id}",
            get(show_notification),
        )
        .route(
            "/api/v1/notifications/{notification_id}/dismiss",
            post(dismiss_notification),
        )
        .route("/api/v2/notifications", get(grouped_notifications))
        .route(
            "/api/v2/notifications/policy",
            get(show_notification_policy).patch(update_notification_policy),
        )
        .route("/api/v1/notifications/requests", get(notification_requests))
        .route(
            "/api/v1/notifications/requests/accept",
            post(accept_notification_requests),
        )
        .route(
            "/api/v1/notifications/requests/dismiss",
            post(dismiss_notification_requests),
        )
        .route(
            "/api/v1/notifications/requests/merged",
            get(notification_requests_merged),
        )
        .route(
            "/api/v1/notifications/requests/{request_id}",
            get(show_notification_request),
        )
        .route(
            "/api/v1/notifications/requests/{request_id}/accept",
            post(accept_notification_request),
        )
        .route(
            "/api/v1/notifications/requests/{request_id}/dismiss",
            post(dismiss_notification_request),
        )
        .route(
            "/api/v2/notifications/unread_count",
            get(grouped_unread_count),
        )
        .route(
            "/api/v2/notifications/{group_key}",
            get(show_notification_group),
        )
        .route(
            "/api/v2/notifications/{group_key}/dismiss",
            post(dismiss_notification_group),
        )
        .route(
            "/api/v2/notifications/{group_key}/accounts",
            get(notification_group_accounts),
        )
}

#[derive(Deserialize)]
struct NotificationPath {
    notification_id: Uuid,
}

#[derive(Deserialize)]
struct NotificationGroupPath {
    group_key: String,
}

#[derive(Deserialize, Default)]
struct NotificationParams {
    limit: Option<u64>,
    max_id: Option<String>,
    since_id: Option<String>,
    min_id: Option<String>,
    #[serde(default)]
    types: Option<Vec<String>>,
    #[serde(default)]
    exclude_types: Option<Vec<String>>,
    account_id: Option<String>,
    #[serde(default)]
    grouped_types: Option<Vec<String>>,
    expand_accounts: Option<ExpandAccounts>,
    #[serde(rename = "include_filtered")]
    include_filtered: Option<bool>,
    #[serde(default)]
    #[serde(rename = "supported_types")]
    _supported_types: Option<Vec<String>>,
}

#[derive(Clone, Copy, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum ExpandAccounts {
    #[default]
    Full,
    PartialAvatars,
}

#[derive(Serialize)]
struct NotificationResponse {
    id: String,
    #[serde(rename = "type")]
    notification_type: LocalNotificationType,
    group_key: String,
    created_at: String,
    account: NotificationAccountResponse,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<StatusResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    report: Option<ReportNotificationResponse>,
}

#[derive(Serialize)]
struct ReportNotificationResponse {
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
    target_account_id: String,
}

impl From<ModerationReport> for ReportNotificationResponse {
    fn from(report: ModerationReport) -> Self {
        Self {
            id: report.id.to_string(),
            action_taken: report.action_taken_at.is_some(),
            action_taken_at: report.action_taken_at,
            category: report.category,
            comment: report.comment,
            forwarded: report.forwarded,
            created_at: report.created_at,
            status_ids: report
                .statuses
                .into_iter()
                .map(|status| match status {
                    ReportStatus::Local(id) | ReportStatus::Remote(id) => id.0.to_string(),
                })
                .collect(),
            rule_ids: report
                .rules
                .into_iter()
                .filter_map(|rule| rule.id.map(|id| id.to_string()))
                .collect(),
            target_account_id: match report.target {
                ReportAccount::Local(id) | ReportAccount::Remote(id) => id.0.to_string(),
            },
        }
    }
}

#[derive(Serialize)]
#[serde(untagged)]
enum NotificationAccountResponse {
    Local(Box<AccountResponse>),
    Remote(Box<RemoteAccountResponse>),
}

#[derive(Serialize)]
struct PartialAccountResponse {
    id: String,
    acct: String,
    url: String,
    avatar: String,
    avatar_static: String,
    avatar_description: String,
    locked: bool,
    bot: bool,
}

#[derive(Serialize)]
struct NotificationGroupResponse {
    group_key: String,
    notifications_count: u64,
    #[serde(rename = "type")]
    notification_type: LocalNotificationType,
    most_recent_notification_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    page_min_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    page_max_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    latest_page_notification_at: Option<String>,
    sample_account_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status_id: Option<String>,
}

#[derive(Serialize)]
struct GroupedNotificationsResponse {
    accounts: Vec<NotificationAccountResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    partial_accounts: Option<Vec<PartialAccountResponse>>,
    statuses: Vec<StatusResponse>,
    notification_groups: Vec<NotificationGroupResponse>,
}

#[derive(Deserialize)]
struct NotificationRequestPath {
    request_id: Uuid,
}

#[derive(Deserialize, Default)]
struct NotificationRequestParams {
    limit: Option<u64>,
    max_id: Option<String>,
    since_id: Option<String>,
    min_id: Option<String>,
}

#[derive(Deserialize, Default)]
struct NotificationRequestBatch {
    #[serde(default, rename = "id")]
    ids: Vec<Uuid>,
}

#[derive(Deserialize, Default)]
struct NotificationPolicyForm {
    for_not_following: Option<NotificationPolicyAction>,
    for_not_followers: Option<NotificationPolicyAction>,
    for_new_accounts: Option<NotificationPolicyAction>,
    for_private_mentions: Option<NotificationPolicyAction>,
    for_limited_accounts: Option<NotificationPolicyAction>,
}

#[derive(Serialize)]
struct NotificationPolicyResponse {
    for_not_following: NotificationPolicyAction,
    for_not_followers: NotificationPolicyAction,
    for_new_accounts: NotificationPolicyAction,
    for_private_mentions: NotificationPolicyAction,
    for_limited_accounts: NotificationPolicyAction,
    summary: NotificationPolicySummaryResponse,
}

#[derive(Serialize)]
struct NotificationPolicySummaryResponse {
    pending_requests_count: u64,
    pending_notifications_count: u64,
}

#[derive(Serialize)]
struct NotificationRequestResponse {
    id: String,
    created_at: String,
    updated_at: String,
    notifications_count: String,
    account: NotificationAccountResponse,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_status: Option<StatusResponse>,
}

async fn show_notification_policy(
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
) -> ApiResult<Response> {
    let txn = database.begin_snapshot().await?;
    let response = policy_response(&txn, account.id).await?;
    txn.commit().await?;
    Ok(Json(response).into_response())
}

async fn update_notification_policy(
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Form(form): Form<NotificationPolicyForm>,
) -> ApiResult<Response> {
    let update = NotificationPolicyUpdate {
        for_not_following: form.for_not_following,
        for_not_followers: form.for_not_followers,
        for_new_accounts: form.for_new_accounts,
        for_private_mentions: form.for_private_mentions,
        for_limited_accounts: form.for_limited_accounts,
    };
    let txn = database.begin_write().await?;
    roosty_db::update_notification_policy(&txn, account.id, update).await?;
    let response = policy_response(&txn, account.id).await?;
    txn.commit().await?;
    Ok(Json(response).into_response())
}

async fn policy_response(
    db: &impl ConnectionTrait,
    account_id: AccountId,
) -> Result<NotificationPolicyResponse, RoostyError> {
    let policy = roosty_db::notification_policy(db, account_id).await?;
    let (pending_requests_count, pending_notifications_count) =
        roosty_db::notification_request_summary(db, account_id).await?;
    Ok(NotificationPolicyResponse {
        for_not_following: policy.for_not_following,
        for_not_followers: policy.for_not_followers,
        for_new_accounts: policy.for_new_accounts,
        for_private_mentions: policy.for_private_mentions,
        for_limited_accounts: policy.for_limited_accounts,
        summary: NotificationPolicySummaryResponse {
            pending_requests_count,
            pending_notifications_count,
        },
    })
}

async fn notification_requests(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    RawQuery(query): RawQuery,
) -> ApiResult<Response> {
    let params = match query.as_deref() {
        Some(query) => notification_request_params(query)?,
        None => NotificationRequestParams::default(),
    };
    let limit = params
        .limit
        .unwrap_or(DEFAULT_NOTIFICATION_LIMIT)
        .clamp(1, MAX_NOTIFICATION_LIMIT);
    let cursor = request_collection_cursor(&params)?;
    let txn = database.begin_snapshot().await?;
    let context = TransactionContext::new(&state, &txn);
    let page =
        roosty_db::notification_requests_for_account(&txn, account.id, limit, cursor).await?;
    let link = CollectionLink::new(
        limit,
        page.first_cursor,
        page.last_cursor,
        page.has_more,
        "/api/v1/notifications/requests",
    )
    .header_value();
    let mut responses = Vec::with_capacity(page.items.len());
    for request in page.items {
        if let Some(response) = notification_request_response(&context, account.id, request).await?
        {
            responses.push(response);
        }
    }
    txn.commit().await?;
    let mut response = Json(responses).into_response();
    if let Some(link) = link {
        response.headers_mut().insert(header::LINK, link);
    }
    Ok(response)
}

async fn show_notification_request(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<NotificationRequestPath>,
) -> ApiResult<Response> {
    let txn = database.begin_snapshot().await?;
    let context = TransactionContext::new(&state, &txn);
    let request =
        roosty_db::find_notification_request_for_account(&txn, account.id, path.request_id)
            .await?
            .ok_or_else(|| ApiError::NotFound("Record not found".into()))?;
    let response = notification_request_response(&context, account.id, request)
        .await?
        .ok_or_else(|| ApiError::NotFound("Record not found".into()))?;
    txn.commit().await?;
    Ok(Json(response).into_response())
}

async fn accept_notification_request(
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<NotificationRequestPath>,
) -> ApiResult<Response> {
    notification_request_action(&database, account.id, &[path.request_id], true).await
}

async fn dismiss_notification_request(
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<NotificationRequestPath>,
) -> ApiResult<Response> {
    notification_request_action(&database, account.id, &[path.request_id], false).await
}

async fn accept_notification_requests(
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    RawForm(body): RawForm,
) -> ApiResult<Response> {
    let batch = notification_request_batch(&body)?;
    if batch.ids.is_empty() {
        return Err(ApiError::BadRequest(
            "at least one notification request id is required".into(),
        ));
    }
    notification_request_action(&database, account.id, &batch.ids, true).await
}

async fn dismiss_notification_requests(
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    RawForm(body): RawForm,
) -> ApiResult<Response> {
    let batch = notification_request_batch(&body)?;
    if batch.ids.is_empty() {
        return Err(ApiError::BadRequest(
            "at least one notification request id is required".into(),
        ));
    }
    notification_request_action(&database, account.id, &batch.ids, false).await
}

async fn notification_request_action(
    database: &DatabaseContext,
    account_id: AccountId,
    request_ids: &[Uuid],
    accept: bool,
) -> ApiResult<Response> {
    let txn = database.begin_write().await?;
    let changed = if accept {
        roosty_db::accept_notification_requests(&txn, account_id, request_ids).await?
    } else {
        roosty_db::dismiss_notification_requests(&txn, account_id, request_ids).await?
    };
    if !changed {
        return Err(ApiError::NotFound("Record not found".into()));
    }
    txn.commit().await?;
    Ok(Json(json!({})).into_response())
}

async fn notification_requests_merged(
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
) -> ApiResult<Response> {
    let txn = database.begin_snapshot().await?;
    let merged = roosty_db::notification_requests_merged(&txn, account.id).await?;
    txn.commit().await?;
    Ok(Json(json!({ "merged": merged })).into_response())
}

async fn unread_count(
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    RawQuery(query): RawQuery,
) -> ApiResult<Response> {
    let params = notification_params(query.as_deref())?;
    let limit = params.limit.unwrap_or(100).clamp(1, 1_000);
    let mut filter = notification_filter(&params)?;
    filter.include_filtered = false;
    let txn = database.begin_snapshot().await?;
    let marker = roosty_db::local_timeline_markers_for_account(
        &txn,
        account.id,
        &[LocalTimeline::Notifications],
    )
    .await?
    .first()
    .map(|marker| marker.last_read_id);
    let cursor = CollectionCursor {
        max_id: None,
        since_id: marker,
        min_id: None,
    };
    let page =
        roosty_db::local_notifications_for_account(&txn, account.id, limit, cursor, filter).await?;
    txn.commit().await?;
    Ok(Json(json!({ "count": page.items.len() })).into_response())
}

async fn notification_request_response(
    state: &TransactionContext<'_, impl ConnectionTrait>,
    viewer_id: AccountId,
    request: NotificationRequest,
) -> Result<Option<NotificationRequestResponse>, RoostyError> {
    let actor_id = match request.actor {
        NotificationActor::Local(id) | NotificationActor::Remote(id) => id,
    };
    let Some(account) = notification_accounts(state, vec![actor_id]).await?.pop() else {
        return Ok(None);
    };
    let last_status = if let Some(status_id) = request.last_status_id {
        if let Some(status) = roosty_db::find_local_status_by_id(state.db, status_id).await?
            && crate::statuses::status_visible_to_viewer(state.db, &status, Some(viewer_id)).await?
        {
            Some(crate::statuses::status_with_author(state, status, Some(viewer_id)).await?)
        } else {
            None
        }
    } else if let Some(status_id) = request.last_remote_status_id {
        if let Some(status) = roosty_db::find_remote_status_by_id(state.db, status_id).await?
            && roosty_db::remote_status_visible_to_account(state.db, &status, viewer_id).await?
        {
            Some(remote_status_response(state, status).await?)
        } else {
            None
        }
    } else {
        None
    };
    Ok(Some(NotificationRequestResponse {
        id: request.id.to_string(),
        created_at: crate::statuses::format_timestamp(request.created_at),
        updated_at: crate::statuses::format_timestamp(request.updated_at),
        notifications_count: request.notifications_count.to_string(),
        account,
        last_status,
    }))
}

/// Return local notifications for the authenticated account.
async fn notifications(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    RawQuery(query): RawQuery,
) -> ApiResult<Response> {
    let params = notification_params(query.as_deref())?;
    let limit = params
        .limit
        .unwrap_or(DEFAULT_NOTIFICATION_LIMIT)
        .clamp(1, MAX_NOTIFICATION_LIMIT);
    let cursor = collection_cursor(&params)?;
    let filter = notification_filter(&params)?;
    if only_unsupported_types_requested(&params, &filter) {
        return Ok(Json(Vec::<NotificationResponse>::new()).into_response());
    }
    let txn = database.begin_snapshot().await?;
    let page =
        roosty_db::local_notifications_for_account(&txn, account.id, limit, cursor, filter).await?;
    let context = TransactionContext::new(&state, &txn);
    let response = notification_page_response(&context, account.id, page, limit).await?;
    txn.commit().await?;
    Ok(response)
}

async fn grouped_notifications(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    RawQuery(query): RawQuery,
) -> ApiResult<Response> {
    let params = notification_params(query.as_deref())?;
    let limit = params
        .limit
        .unwrap_or(DEFAULT_NOTIFICATION_LIMIT)
        .clamp(1, MAX_NOTIFICATION_LIMIT);
    let cursor = collection_cursor(&params)?;
    let filter = notification_filter(&params)?;
    if only_unsupported_types_requested(&params, &filter) {
        return Ok(Json(GroupedNotificationsResponse {
            accounts: Vec::new(),
            partial_accounts: None,
            statuses: Vec::new(),
            notification_groups: Vec::new(),
        })
        .into_response());
    }
    let grouped_types = grouped_notification_types(&params);
    let txn = database.begin_snapshot().await?;
    let page = roosty_db::notification_groups_for_account(
        &txn,
        account.id,
        limit,
        cursor,
        filter,
        &grouped_types,
    )
    .await?;
    let link = CollectionLink::new(
        limit,
        page.first_cursor,
        page.last_cursor,
        page.has_more,
        "/api/v2/notifications",
    )
    .header_value();
    let context = TransactionContext::new(&state, &txn);
    let body = grouped_response(
        &context,
        account.id,
        page.items,
        params.expand_accounts.unwrap_or_default(),
        true,
    )
    .await?;
    txn.commit().await?;
    let mut response = Json(body).into_response();
    if let Some(link) = link {
        response.headers_mut().insert(header::LINK, link);
    }
    Ok(response)
}

async fn show_notification_group(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<NotificationGroupPath>,
) -> ApiResult<Response> {
    let txn = database.begin_snapshot().await?;
    let rows = roosty_db::notifications_in_group(&txn, account.id, &path.group_key).await?;
    if rows.is_empty() {
        return Err(ApiError::NotFound("Record not found".into()));
    }
    let group = notification_group_from_rows(path.group_key, &rows);
    let context = TransactionContext::new(&state, &txn);
    let body = grouped_response(
        &context,
        account.id,
        vec![group],
        ExpandAccounts::Full,
        false,
    )
    .await?;
    txn.commit().await?;
    Ok(Json(body).into_response())
}

async fn dismiss_notification_group(
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<NotificationGroupPath>,
) -> ApiResult<Response> {
    let txn = database.begin_write().await?;
    if !roosty_db::dismiss_notification_group(&txn, account.id, &path.group_key).await? {
        return Err(ApiError::NotFound("Record not found".into()));
    }
    txn.commit().await?;
    Ok(Json(json!({})).into_response())
}

async fn notification_group_accounts(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<NotificationGroupPath>,
) -> ApiResult<Response> {
    let txn = database.begin_snapshot().await?;
    let rows = roosty_db::notifications_in_group(&txn, account.id, &path.group_key).await?;
    if rows.is_empty() {
        return Err(ApiError::NotFound("Record not found".into()));
    }
    let ids = rows
        .iter()
        .filter_map(notification_actor_id)
        .collect::<Vec<_>>();
    let context = TransactionContext::new(&state, &txn);
    let accounts = notification_accounts(&context, ids).await?;
    txn.commit().await?;
    Ok(Json(accounts).into_response())
}

async fn grouped_unread_count(
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    RawQuery(query): RawQuery,
) -> ApiResult<Response> {
    let params = notification_params(query.as_deref())?;
    let limit = params.limit.unwrap_or(100).clamp(1, 1_000);
    let filter = notification_filter(&params)?;
    let txn = database.begin_snapshot().await?;
    let marker = roosty_db::local_timeline_markers_for_account(
        &txn,
        account.id,
        &[LocalTimeline::Notifications],
    )
    .await?
    .first()
    .map(|marker| marker.last_read_id);
    let cursor = CollectionCursor {
        max_id: None,
        since_id: marker,
        min_id: None,
    };
    let page = roosty_db::notification_groups_for_account(
        &txn,
        account.id,
        limit,
        cursor,
        filter,
        &grouped_notification_types(&params),
    )
    .await?;
    txn.commit().await?;
    Ok(Json(json!({ "count": page.items.len() })).into_response())
}

fn grouped_notification_types(params: &NotificationParams) -> Vec<LocalNotificationType> {
    params
        .grouped_types
        .as_deref()
        .map(|types| parse_notification_types(Some(types)))
        .unwrap_or_else(|| {
            vec![
                LocalNotificationType::Favourite,
                LocalNotificationType::Follow,
                LocalNotificationType::Reblog,
            ]
        })
}

fn notification_group_from_rows(
    group_key: String,
    rows: &[LocalNotification],
) -> NotificationGroup {
    let first = &rows[0];
    let mut sample_account_ids = Vec::new();
    for row in rows {
        if let Some(id) = notification_actor_id(row)
            && !sample_account_ids.contains(&id)
            && sample_account_ids.len() < 8
        {
            sample_account_ids.push(id);
        }
    }
    NotificationGroup {
        group_key,
        notifications_count: rows.len() as u64,
        notification_type: first.notification_type,
        most_recent_notification_id: first.id,
        page_min_id: rows.last().map_or(first.id, |row| row.id),
        page_max_id: first.id,
        latest_page_notification_at: first.created_at,
        sample_account_ids,
        status_id: first.status_id.or(first.remote_status_id),
        remote_status: first.remote_status_id.is_some(),
    }
}

fn notification_actor_id(notification: &LocalNotification) -> Option<AccountId> {
    notification
        .actor_account_id
        .or(notification.remote_actor_id)
}

/// Return one local notification owned by the authenticated account.
async fn show_notification(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<NotificationPath>,
) -> ApiResult<Response> {
    let txn = database.begin_snapshot().await?;
    let notification =
        roosty_db::find_local_notification_for_account(&txn, account.id, path.notification_id)
            .await?
            .ok_or_else(|| ApiError::NotFound("Record not found".into()))?;
    let context = TransactionContext::new(&state, &txn);
    let notification = notification_response(&context, account.id, notification)
        .await?
        .ok_or_else(|| ApiError::NotFound("Record not found".into()))?;
    txn.commit().await?;
    Ok(Json(notification).into_response())
}

/// Dismiss a local notification owned by the authenticated account.
async fn dismiss_notification(
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<NotificationPath>,
) -> ApiResult<Response> {
    let txn = database.begin_write().await?;
    if !roosty_db::dismiss_local_notification(&txn, account.id, path.notification_id).await? {
        return Err(ApiError::NotFound("Record not found".into()));
    }
    txn.commit().await?;
    Ok(Json(json!({})).into_response())
}

/// Dismiss every local notification owned by the authenticated account.
async fn clear_notifications(
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
) -> ApiResult<Response> {
    let txn = database.begin_write().await?;
    roosty_db::clear_local_notifications(&txn, account.id).await?;
    txn.commit().await?;
    Ok(Json(json!({})).into_response())
}

/// Create a local notification and publish it to the recipient's user stream.
pub(crate) async fn create_and_stream_notification(
    state: &AppState,
    database: &DatabaseContext,
    account_id: AccountId,
    notification_type: LocalNotificationType,
    actor_account_id: AccountId,
    status_id: Option<StatusId>,
) -> Result<(), RoostyError> {
    if account_id == actor_account_id {
        return Ok(());
    }
    let txn = database.begin_write().await?;
    let Some(notification) = roosty_db::notify_local_account_with_policy(
        &txn,
        account_id,
        notification_type,
        actor_account_id,
        status_id,
    )
    .await?
    else {
        txn.commit().await?;
        return Ok(());
    };
    let response = if notification.filtered {
        None
    } else {
        let context = TransactionContext::new(state, &txn);
        notification_response(&context, account_id, notification).await?
    };
    txn.commit().await?;
    if let Some(response) = response {
        state
            .streaming_events
            .publish_notification(&response, account_id);
    }
    Ok(())
}

/// Publish a notification that was persisted by a caller-owned transaction.
pub(crate) fn publish_committed_notification<'a>(
    state: &'a AppState,
    database: &'a DatabaseContext,
    account_id: AccountId,
    notification: LocalNotification,
) -> Pin<Box<dyn Future<Output = Result<(), RoostyError>> + Send + 'a>> {
    Box::pin(async move {
        if notification.filtered {
            return Ok(());
        }
        let txn = database.begin_snapshot().await?;
        let context = TransactionContext::new(state, &txn);
        let response = notification_response(&context, account_id, notification).await?;
        txn.commit().await?;
        if let Some(response) = response {
            state
                .streaming_events
                .publish_notification(&response, account_id);
        }
        Ok(())
    })
}

async fn notification_page_response(
    state: &TransactionContext<'_, impl ConnectionTrait>,
    account_id: AccountId,
    page: CollectionPage<LocalNotification>,
    limit: u64,
) -> Result<Response, RoostyError> {
    let link_header = CollectionLink::new(
        limit,
        page.first_cursor,
        page.last_cursor,
        page.has_more,
        "/api/v1/notifications",
    )
    .header_value();
    let mut notifications = Vec::with_capacity(page.items.len());
    for notification in page.items {
        if let Some(notification) = notification_response(state, account_id, notification).await? {
            notifications.push(notification);
        }
    }
    let mut response = Json(notifications).into_response();
    if let Some(link_header) = link_header {
        response.headers_mut().insert(header::LINK, link_header);
    }
    Ok(response)
}

async fn grouped_response(
    state: &TransactionContext<'_, impl ConnectionTrait>,
    viewer_id: AccountId,
    groups: Vec<NotificationGroup>,
    expand_accounts: ExpandAccounts,
    paginated: bool,
) -> Result<GroupedNotificationsResponse, RoostyError> {
    let mut actor_ids = Vec::new();
    let mut full_ids = HashSet::new();
    for group in &groups {
        if let Some(id) = group.sample_account_ids.first() {
            full_ids.insert(id.0.to_string());
        }
        for id in &group.sample_account_ids {
            if !actor_ids.contains(id) {
                actor_ids.push(*id);
            }
        }
    }
    let actor_responses = notification_accounts(state, actor_ids).await?;
    let mut accounts = Vec::new();
    let mut partial_accounts = Vec::new();
    for account in actor_responses {
        if expand_accounts == ExpandAccounts::PartialAvatars {
            let value = serde_json::to_value(&account)?;
            let id = value
                .get("id")
                .and_then(JsonValue::as_str)
                .unwrap_or_default();
            if !full_ids.contains(id) {
                partial_accounts.push(partial_account_from_value(&value));
                continue;
            }
        }
        accounts.push(account);
    }
    let mut statuses = Vec::new();
    let mut status_ids = HashSet::new();
    for group in &groups {
        let Some(status_id) = group.status_id else {
            continue;
        };
        if !status_ids.insert((status_id, group.remote_status)) {
            continue;
        }
        if group.remote_status {
            if let Some(status) = roosty_db::find_remote_status_by_id(state.db, status_id).await?
                && roosty_db::remote_status_visible_to_account(state.db, &status, viewer_id).await?
            {
                statuses.push(remote_status_response(state, status).await?);
            }
        } else if let Some(status) = roosty_db::find_local_status_by_id(state.db, status_id).await?
            && crate::statuses::status_visible_to_viewer(state.db, &status, Some(viewer_id)).await?
        {
            statuses
                .push(crate::statuses::status_with_author(state, status, Some(viewer_id)).await?);
        }
    }
    let notification_groups = groups
        .into_iter()
        .map(|group| NotificationGroupResponse {
            group_key: group.group_key,
            notifications_count: group.notifications_count,
            notification_type: group.notification_type,
            most_recent_notification_id: group.most_recent_notification_id.to_string(),
            page_min_id: paginated.then(|| group.page_min_id.to_string()),
            page_max_id: paginated.then(|| group.page_max_id.to_string()),
            latest_page_notification_at: paginated
                .then(|| crate::statuses::format_timestamp(group.latest_page_notification_at)),
            sample_account_ids: group
                .sample_account_ids
                .into_iter()
                .map(|id| id.0.to_string())
                .collect(),
            status_id: group.status_id.map(|id| id.0.to_string()),
        })
        .collect();
    Ok(GroupedNotificationsResponse {
        accounts,
        partial_accounts: (expand_accounts == ExpandAccounts::PartialAvatars)
            .then_some(partial_accounts),
        statuses,
        notification_groups,
    })
}

async fn notification_accounts(
    state: &TransactionContext<'_, impl ConnectionTrait>,
    ids: Vec<AccountId>,
) -> Result<Vec<NotificationAccountResponse>, RoostyError> {
    let mut seen = HashSet::new();
    let mut accounts = Vec::new();
    for id in ids {
        if !seen.insert(id) {
            continue;
        }
        if let Some(account) = roosty_db::find_local_account_by_id(state.db, id).await? {
            accounts.push(NotificationAccountResponse::Local(Box::new(
                account_response(state, state.db, account).await?,
            )));
        } else if let Some(actor) = roosty_db::find_remote_actor_by_id(state.db, id).await? {
            let suspended = actor.suspended_at.is_some()
                || roosty_db::federation_domain_policy(state.db, &actor.domain)
                    .await?
                    .is_suspended();
            if !suspended {
                accounts.push(NotificationAccountResponse::Remote(Box::new(
                    crate::accounts::remote_account_response_on(state, state.db, actor).await?,
                )));
            }
        }
    }
    Ok(accounts)
}

fn partial_account_from_value(value: &JsonValue) -> PartialAccountResponse {
    let string = |name| {
        value
            .get(name)
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let boolean = |name| {
        value
            .get(name)
            .and_then(JsonValue::as_bool)
            .unwrap_or(false)
    };
    PartialAccountResponse {
        id: string("id"),
        acct: string("acct"),
        url: string("url"),
        avatar: string("avatar"),
        avatar_static: string("avatar_static"),
        avatar_description: String::new(),
        locked: boolean("locked"),
        bot: boolean("bot"),
    }
}

/// Build the Mastodon notification entity for a local notification row.
async fn notification_response(
    state: &TransactionContext<'_, impl ConnectionTrait>,
    viewer_id: AccountId,
    notification: LocalNotification,
) -> Result<Option<NotificationResponse>, RoostyError> {
    let actor = match (notification.actor_account_id, notification.remote_actor_id) {
        (Some(actor_id), None) => {
            let Some(actor) = roosty_db::find_local_account_by_id(state.db, actor_id).await? else {
                return Ok(None);
            };
            NotificationAccountResponse::Local(Box::new(
                account_response(state, state.db, actor).await?,
            ))
        }
        (None, Some(actor_id)) => {
            let Some(actor) = roosty_db::find_remote_actor_by_id(state.db, actor_id).await? else {
                return Ok(None);
            };
            NotificationAccountResponse::Remote(Box::new(
                crate::accounts::remote_account_response(state, actor).await?,
            ))
        }
        _ => return Ok(None),
    };
    let status = match (notification.status_id, notification.remote_status_id) {
        (Some(status_id), None) => {
            let Some(status) = roosty_db::find_local_status_by_id(state.db, status_id).await?
            else {
                return Ok(None);
            };
            if !crate::statuses::status_visible_to_viewer(state.db, &status, Some(viewer_id))
                .await?
            {
                return Ok(None);
            }
            Some(crate::statuses::status_with_author(state, status, Some(viewer_id)).await?)
        }
        (None, Some(status_id)) => {
            let Some(status) = roosty_db::find_remote_status_by_id(state.db, status_id).await?
            else {
                return Ok(None);
            };
            if !roosty_db::remote_status_visible_to_account(state.db, &status, viewer_id).await? {
                return Ok(None);
            }
            Some(remote_status_response(state, status).await?)
        }
        (None, None) => None,
        (Some(_), Some(_)) => return Ok(None),
    };
    let report = match notification.report_id {
        Some(report_id) => find_moderation_report(state.db, report_id)
            .await?
            .map(ReportNotificationResponse::from),
        None => None,
    };

    Ok(Some(NotificationResponse {
        id: notification.id.to_string(),
        notification_type: notification.notification_type,
        group_key: notification.group_key(),
        created_at: crate::statuses::format_timestamp(notification.created_at),
        account: actor,
        status,
        report,
    }))
}

/// Compact payload encrypted into a Mastodon-compatible Web Push message.
#[derive(Serialize)]
pub(crate) struct MastodonPushPayload {
    access_token: String,
    preferred_locale: String,
    notification_id: String,
    notification_type: LocalNotificationType,
    icon: String,
    title: String,
    body: String,
}

/// Build the compact Mastodon Web Push payload from typed domain records.
pub(crate) async fn push_payload(
    db: &DbConnection,
    public_base_url: &Url,
    notification: LocalNotification,
    access_token: String,
) -> Result<MastodonPushPayload, RoostyError> {
    let notification_id = notification.id.to_string();
    let notification_type = notification.notification_type;
    let recipient = roosty_db::find_local_account_by_id(db, notification.account_id)
        .await?
        .ok_or_else(|| {
            RoostyError::InvalidInput("push notification recipient is missing".to_owned())
        })?;
    let (actor, icon) = match (notification.actor_account_id, notification.remote_actor_id) {
        (Some(actor_id), None) => {
            let actor = roosty_db::find_local_account_by_id(db, actor_id)
                .await?
                .ok_or_else(|| {
                    RoostyError::InvalidInput("push notification actor is missing".to_owned())
                })?;
            let title = if actor.display_name.is_empty() {
                actor.username
            } else {
                actor.display_name
            };
            let icon = public_base_url
                .join("avatars/original/missing.png")
                .map_or_else(|_| String::new(), |url| url.to_string());
            (title, icon)
        }
        (None, Some(actor_id)) => {
            let actor = roosty_db::find_remote_actor_by_id(db, actor_id)
                .await?
                .ok_or_else(|| {
                    RoostyError::InvalidInput("push notification actor is missing".to_owned())
                })?;
            let title = if actor.display_name.is_empty() {
                actor.username
            } else {
                actor.display_name
            };
            (title, String::new())
        }
        _ => {
            return Err(RoostyError::InvalidInput(
                "push notification actor is invalid".to_owned(),
            ));
        }
    };
    let body = match notification_type {
        LocalNotificationType::Mention => format!("{actor} mentioned you"),
        LocalNotificationType::Favourite => format!("{actor} favourited your post"),
        LocalNotificationType::Reblog => format!("{actor} boosted your post"),
        LocalNotificationType::Follow => format!("{actor} followed you"),
        LocalNotificationType::FollowRequest => format!("{actor} requested to follow you"),
        LocalNotificationType::Status => format!("{actor} posted a new status"),
        LocalNotificationType::Update | LocalNotificationType::QuotedUpdate => {
            "A related post was edited".to_owned()
        }
        LocalNotificationType::Quote => format!("{actor} quoted your post"),
        LocalNotificationType::Poll => "A poll you participated in has ended".to_owned(),
        LocalNotificationType::AdminReport => "A new moderation report was filed".to_owned(),
    };
    Ok(MastodonPushPayload {
        access_token,
        preferred_locale: recipient
            .default_language
            .unwrap_or_else(|| "en".to_owned()),
        notification_id,
        notification_type,
        icon,
        title: actor,
        body,
    })
}

fn notification_params(query: Option<&str>) -> Result<NotificationParams, NotificationInputError> {
    let Some(query) = query else {
        return Ok(NotificationParams::default());
    };

    Ok(serde_qs::Config::new()
        .array_format(serde_qs::ArrayFormat::EmptyIndexed)
        .use_form_encoding(true)
        .deserialize_str(query)?)
}

fn notification_request_params(
    query: &str,
) -> Result<NotificationRequestParams, NotificationInputError> {
    Ok(serde_qs::Config::new()
        .use_form_encoding(true)
        .deserialize_str(query)?)
}

fn collection_cursor(
    params: &NotificationParams,
) -> Result<CollectionCursor, NotificationInputError> {
    Ok(CollectionCursor {
        max_id: parse_optional_uuid(params.max_id.as_deref())?,
        since_id: parse_optional_uuid(params.since_id.as_deref())?,
        min_id: parse_optional_uuid(params.min_id.as_deref())?,
    })
}

fn notification_filter(
    params: &NotificationParams,
) -> Result<NotificationFilter, NotificationInputError> {
    Ok(NotificationFilter {
        include_types: parse_notification_types(params.types.as_deref()),
        exclude_types: parse_notification_types(params.exclude_types.as_deref()),
        account_id: parse_optional_account_id(params.account_id.as_deref())?,
        include_filtered: params.include_filtered.unwrap_or(false),
    })
}

fn request_collection_cursor(
    params: &NotificationRequestParams,
) -> Result<CollectionCursor, NotificationInputError> {
    Ok(CollectionCursor {
        max_id: parse_optional_uuid(params.max_id.as_deref())?,
        since_id: parse_optional_uuid(params.since_id.as_deref())?,
        min_id: parse_optional_uuid(params.min_id.as_deref())?,
    })
}

fn notification_request_batch(
    body: &[u8],
) -> Result<NotificationRequestBatch, NotificationInputError> {
    let body = std::str::from_utf8(body)?;
    Ok(serde_qs::Config::new()
        .array_format(serde_qs::ArrayFormat::EmptyIndexed)
        .use_form_encoding(true)
        .deserialize_str(body)?)
}

fn only_unsupported_types_requested(
    params: &NotificationParams,
    filter: &NotificationFilter,
) -> bool {
    params
        .types
        .as_ref()
        .is_some_and(|types| !types.is_empty() && filter.include_types.is_empty())
}

fn parse_notification_types(values: Option<&[String]>) -> Vec<LocalNotificationType> {
    values
        .unwrap_or_default()
        .iter()
        .filter_map(|value| LocalNotificationType::from_str(value).ok())
        .collect()
}

fn parse_optional_uuid(value: Option<&str>) -> Result<Option<Uuid>, NotificationInputError> {
    Ok(value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::parse)
        .transpose()?)
}

fn parse_optional_account_id(
    value: Option<&str>,
) -> Result<Option<AccountId>, NotificationInputError> {
    parse_optional_uuid(value).map(|id| id.map(AccountId))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Given Mastodon's bracketed form encoding, when multiple request IDs are submitted, then all
    /// IDs are retained for the batch action.
    #[test]
    fn parses_notification_request_batch_ids() {
        let first = Uuid::now_v7();
        let second = Uuid::now_v7();
        let body = format!("id[]={first}&id[]={second}");

        let batch = notification_request_batch(body.as_bytes()).unwrap();

        assert_eq!(batch.ids, vec![first, second]);
    }
}
