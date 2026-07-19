use axum::{
    Json, Router,
    extract::{Path, RawQuery, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use roosty_core::{AccountId, RoostyError, StatusId};
use roosty_db::{
    CollectionCursor, CollectionPage, LocalNotification, LocalNotificationType, NotificationFilter,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{collections::HashSet, future::Future, pin::Pin, str::FromStr};
use uuid::Uuid;

use crate::{
    accounts::RemoteAccountResponse,
    auth::{AccountResponse, AuthenticatedAccount, account_response},
    http::AppState,
    statuses::{CollectionLink, StatusResponse, remote_status_response},
};

const DEFAULT_NOTIFICATION_LIMIT: u64 = 40;
const MAX_NOTIFICATION_LIMIT: u64 = 80;

/// Build routes for Mastodon-compatible notification collections.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/notifications", get(notifications))
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
    _include_filtered: Option<bool>,
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

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

/// Return local notifications for the authenticated account.
async fn notifications(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    RawQuery(query): RawQuery,
) -> Response {
    let params = match notification_params(query.as_deref()) {
        Ok(params) => params,
        Err(()) => return bad_request("notification query is invalid"),
    };
    let limit = params
        .limit
        .unwrap_or(DEFAULT_NOTIFICATION_LIMIT)
        .clamp(1, MAX_NOTIFICATION_LIMIT);
    let cursor = match collection_cursor(&params) {
        Ok(cursor) => cursor,
        Err(()) => return bad_request("notification cursor is invalid"),
    };
    let filter = match notification_filter(&params) {
        Ok(filter) => filter,
        Err(()) => return bad_request("notification account id is invalid"),
    };
    if only_unsupported_types_requested(&params, &filter) {
        return Json(Vec::<NotificationResponse>::new()).into_response();
    }

    match roosty_db::local_notifications_for_account(&state.db, account.id, limit, cursor, filter)
        .await
    {
        Ok(page) => notification_page_response(&state, account.id, page, limit).await,
        Err(error) => server_error(error),
    }
}

async fn grouped_notifications(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    RawQuery(query): RawQuery,
) -> Response {
    let params = match notification_params(query.as_deref()) {
        Ok(params) => params,
        Err(()) => return bad_request("notification query is invalid"),
    };
    let limit = params
        .limit
        .unwrap_or(DEFAULT_NOTIFICATION_LIMIT)
        .clamp(1, MAX_NOTIFICATION_LIMIT);
    let cursor = match collection_cursor(&params) {
        Ok(cursor) => cursor,
        Err(()) => return bad_request("notification cursor is invalid"),
    };
    let filter = match notification_filter(&params) {
        Ok(filter) => filter,
        Err(()) => return bad_request("notification account id is invalid"),
    };
    if only_unsupported_types_requested(&params, &filter) {
        return Json(GroupedNotificationsResponse {
            accounts: Vec::new(),
            partial_accounts: None,
            statuses: Vec::new(),
            notification_groups: Vec::new(),
        })
        .into_response();
    }
    let grouped_types = grouped_notification_types(&params);
    match roosty_db::notification_groups_for_account(
        &state.db,
        account.id,
        limit,
        cursor,
        filter,
        &grouped_types,
    )
    .await
    {
        Ok(page) => {
            let link = CollectionLink::new(
                limit,
                page.first_cursor,
                page.last_cursor,
                page.has_more,
                "/api/v2/notifications",
            )
            .header_value();
            match grouped_response(
                &state,
                account.id,
                page.items,
                params.expand_accounts.unwrap_or_default(),
                true,
            )
            .await
            {
                Ok(body) => {
                    let mut response = Json(body).into_response();
                    if let Some(link) = link {
                        response.headers_mut().insert(header::LINK, link);
                    }
                    response
                }
                Err(error) => server_error(error),
            }
        }
        Err(error) => server_error(error),
    }
}

async fn show_notification_group(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<NotificationGroupPath>,
) -> Response {
    match roosty_db::notifications_in_group(&state.db, account.id, &path.group_key).await {
        Ok(rows) if !rows.is_empty() => {
            let group = notification_group_from_rows(path.group_key, &rows);
            match grouped_response(&state, account.id, vec![group], ExpandAccounts::Full, false)
                .await
            {
                Ok(body) => Json(body).into_response(),
                Err(error) => server_error(error),
            }
        }
        Ok(_) => not_found(),
        Err(error) => server_error(error),
    }
}

async fn dismiss_notification_group(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<NotificationGroupPath>,
) -> Response {
    match roosty_db::dismiss_notification_group(&state.db, account.id, &path.group_key).await {
        Ok(true) => Json(json!({})).into_response(),
        Ok(false) => not_found(),
        Err(error) => server_error(error),
    }
}

async fn notification_group_accounts(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<NotificationGroupPath>,
) -> Response {
    match roosty_db::notifications_in_group(&state.db, account.id, &path.group_key).await {
        Ok(rows) if !rows.is_empty() => {
            let ids = rows
                .iter()
                .filter_map(notification_actor_id)
                .collect::<Vec<_>>();
            match notification_accounts(&state, ids).await {
                Ok(accounts) => Json(accounts).into_response(),
                Err(error) => server_error(error),
            }
        }
        Ok(_) => not_found(),
        Err(error) => server_error(error),
    }
}

async fn grouped_unread_count(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    RawQuery(query): RawQuery,
) -> Response {
    let params = match notification_params(query.as_deref()) {
        Ok(params) => params,
        Err(()) => return bad_request("notification query is invalid"),
    };
    let limit = params.limit.unwrap_or(100).clamp(1, 1_000);
    let filter = match notification_filter(&params) {
        Ok(filter) => filter,
        Err(()) => return bad_request("notification account id is invalid"),
    };
    let marker = match roosty_db::local_timeline_markers_for_account(
        &state.db,
        account.id,
        &[roosty_db::LocalTimeline::Notifications],
    )
    .await
    {
        Ok(markers) => markers.first().map(|marker| marker.last_read_id),
        Err(error) => return server_error(error),
    };
    let cursor = CollectionCursor {
        max_id: None,
        since_id: marker,
        min_id: None,
    };
    match roosty_db::notification_groups_for_account(
        &state.db,
        account.id,
        limit,
        cursor,
        filter,
        &grouped_notification_types(&params),
    )
    .await
    {
        Ok(page) => Json(json!({ "count": page.items.len() })).into_response(),
        Err(error) => server_error(error),
    }
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
) -> roosty_db::NotificationGroup {
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
    roosty_db::NotificationGroup {
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
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<NotificationPath>,
) -> Response {
    match roosty_db::find_local_notification_for_account(
        &state.db,
        account.id,
        path.notification_id,
    )
    .await
    {
        Ok(Some(notification)) => {
            match notification_response(&state, account.id, notification).await {
                Ok(Some(notification)) => Json(notification).into_response(),
                Ok(None) => not_found(),
                Err(error) => server_error(error),
            }
        }
        Ok(None) => not_found(),
        Err(error) => server_error(error),
    }
}

/// Dismiss a local notification owned by the authenticated account.
async fn dismiss_notification(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<NotificationPath>,
) -> Response {
    match roosty_db::dismiss_local_notification(&state.db, account.id, path.notification_id).await {
        Ok(true) => Json(json!({})).into_response(),
        Ok(false) => not_found(),
        Err(error) => server_error(error),
    }
}

/// Dismiss every local notification owned by the authenticated account.
async fn clear_notifications(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
) -> Response {
    match roosty_db::clear_local_notifications(&state.db, account.id).await {
        Ok(()) => Json(json!({})).into_response(),
        Err(error) => server_error(error),
    }
}

/// Create a local notification and publish it to the recipient's user stream.
pub(crate) async fn create_and_stream_notification(
    state: &AppState,
    account_id: AccountId,
    notification_type: LocalNotificationType,
    actor_account_id: AccountId,
    status_id: Option<StatusId>,
) -> Result<(), RoostyError> {
    if account_id == actor_account_id {
        return Ok(());
    }
    if !roosty_db::local_account_allows_notification(&state.db, account_id, actor_account_id)
        .await?
    {
        return Ok(());
    }
    let notification = roosty_db::notify_local_account(
        &state.db,
        account_id,
        notification_type,
        actor_account_id,
        status_id,
    )
    .await?;
    if let Some(response) = notification_response(state, account_id, notification).await? {
        state
            .streaming_events
            .publish_notification(&response, account_id);
    }
    Ok(())
}

/// Publish a notification that was persisted by a caller-owned transaction.
pub(crate) fn publish_committed_notification(
    state: &AppState,
    account_id: AccountId,
    notification: LocalNotification,
) -> Pin<Box<dyn Future<Output = Result<(), RoostyError>> + Send + '_>> {
    Box::pin(async move {
        if let Some(response) = notification_response(state, account_id, notification).await? {
            state
                .streaming_events
                .publish_notification(&response, account_id);
        }
        Ok(())
    })
}

async fn notification_page_response(
    state: &AppState,
    account_id: AccountId,
    page: CollectionPage<LocalNotification>,
    limit: u64,
) -> Response {
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
        match notification_response(state, account_id, notification).await {
            Ok(Some(notification)) => notifications.push(notification),
            Ok(None) => {}
            Err(error) => return server_error(error),
        }
    }
    let mut response = Json(notifications).into_response();
    if let Some(link_header) = link_header {
        response.headers_mut().insert(header::LINK, link_header);
    }
    response
}

async fn grouped_response(
    state: &AppState,
    viewer_id: AccountId,
    groups: Vec<roosty_db::NotificationGroup>,
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
            let value = serde_json::to_value(&account)
                .map_err(|error| RoostyError::InvalidInput(error.to_string()))?;
            let id = value
                .get("id")
                .and_then(serde_json::Value::as_str)
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
            if let Some(status) = roosty_db::find_remote_status_by_id(&state.db, status_id).await?
                && roosty_db::remote_status_visible_to_account(&state.db, &status, viewer_id)
                    .await?
            {
                statuses.push(remote_status_response(state, status).await?);
            }
        } else if let Some(status) =
            roosty_db::find_local_status_by_id(&state.db, status_id).await?
            && crate::statuses::status_visible_to_viewer(state, &status, Some(viewer_id)).await?
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
    state: &AppState,
    ids: Vec<AccountId>,
) -> Result<Vec<NotificationAccountResponse>, RoostyError> {
    let mut seen = HashSet::new();
    let mut accounts = Vec::new();
    for id in ids {
        if !seen.insert(id) {
            continue;
        }
        if let Some(account) = roosty_db::find_local_account_by_id(&state.db, id).await? {
            accounts.push(NotificationAccountResponse::Local(Box::new(
                account_response(state, account).await?,
            )));
        } else if let Some(actor) = roosty_db::find_remote_actor_by_id(&state.db, id).await?
            && !state.config.federation_domain_is_blocked(&actor.domain)
        {
            accounts.push(NotificationAccountResponse::Remote(Box::new(
                crate::accounts::remote_account_response(state, actor).await?,
            )));
        }
    }
    Ok(accounts)
}

fn partial_account_from_value(value: &serde_json::Value) -> PartialAccountResponse {
    let string = |name| {
        value
            .get(name)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let boolean = |name| {
        value
            .get(name)
            .and_then(serde_json::Value::as_bool)
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
    state: &AppState,
    viewer_id: AccountId,
    notification: LocalNotification,
) -> Result<Option<NotificationResponse>, RoostyError> {
    let actor = match (notification.actor_account_id, notification.remote_actor_id) {
        (Some(actor_id), None) => {
            let Some(actor) = roosty_db::find_local_account_by_id(&state.db, actor_id).await?
            else {
                return Ok(None);
            };
            NotificationAccountResponse::Local(Box::new(account_response(state, actor).await?))
        }
        (None, Some(actor_id)) => {
            let Some(actor) = roosty_db::find_remote_actor_by_id(&state.db, actor_id).await? else {
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
            let Some(status) = roosty_db::find_local_status_by_id(&state.db, status_id).await?
            else {
                return Ok(None);
            };
            if !crate::statuses::status_visible_to_viewer(state, &status, Some(viewer_id)).await? {
                return Ok(None);
            }
            Some(crate::statuses::status_with_author(state, status, Some(viewer_id)).await?)
        }
        (None, Some(status_id)) => {
            let Some(status) = roosty_db::find_remote_status_by_id(&state.db, status_id).await?
            else {
                return Ok(None);
            };
            if !roosty_db::remote_status_visible_to_account(&state.db, &status, viewer_id).await? {
                return Ok(None);
            }
            Some(remote_status_response(state, status).await?)
        }
        (None, None) => None,
        (Some(_), Some(_)) => return Ok(None),
    };

    Ok(Some(NotificationResponse {
        id: notification.id.to_string(),
        notification_type: notification.notification_type,
        group_key: notification.group_key(),
        created_at: crate::statuses::format_timestamp(notification.created_at),
        account: actor,
        status,
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
    db: &roosty_db::DbConnection,
    public_base_url: &url::Url,
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

fn notification_params(query: Option<&str>) -> Result<NotificationParams, ()> {
    let Some(query) = query else {
        return Ok(NotificationParams::default());
    };

    serde_qs::Config::new()
        .array_format(serde_qs::ArrayFormat::EmptyIndexed)
        .use_form_encoding(true)
        .deserialize_str(query)
        .map_err(|_| ())
}

fn collection_cursor(params: &NotificationParams) -> Result<CollectionCursor, ()> {
    Ok(CollectionCursor {
        max_id: parse_optional_uuid(params.max_id.as_deref())?,
        since_id: parse_optional_uuid(params.since_id.as_deref())?,
        min_id: parse_optional_uuid(params.min_id.as_deref())?,
    })
}

fn notification_filter(params: &NotificationParams) -> Result<NotificationFilter, ()> {
    Ok(NotificationFilter {
        include_types: parse_notification_types(params.types.as_deref()),
        exclude_types: parse_notification_types(params.exclude_types.as_deref()),
        account_id: parse_optional_account_id(params.account_id.as_deref())?,
    })
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

fn parse_optional_uuid(value: Option<&str>) -> Result<Option<Uuid>, ()> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.parse().map_err(|_| ()))
        .transpose()
}

fn parse_optional_account_id(value: Option<&str>) -> Result<Option<AccountId>, ()> {
    parse_optional_uuid(value).map(|id| id.map(AccountId))
}

fn bad_request(description: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ErrorResponse {
            error: description.to_owned(),
        }),
    )
        .into_response()
}

fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorResponse {
            error: "Record not found".to_owned(),
        }),
    )
        .into_response()
}

fn server_error(error: RoostyError) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse {
            error: error.to_string(),
        }),
    )
        .into_response()
}
