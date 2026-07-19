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
use std::{future::Future, pin::Pin, str::FromStr};
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
}

#[derive(Deserialize)]
struct NotificationPath {
    notification_id: Uuid,
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
        group_key: format!("ungrouped-{}", notification.id),
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
