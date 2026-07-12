use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use roosty_core::{AccountId, RoostyError, StatusId};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::{
    auth::{AccountResponse, AuthenticatedAccount, account_response},
    http::AppState,
    statuses::{CollectionLink, StatusResponse},
};

const DEFAULT_CONVERSATION_LIMIT: u64 = 20;
const MAX_CONVERSATION_LIMIT: u64 = 40;

/// Build routes for Mastodon-compatible direct-message conversations.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/conversations", get(conversations))
        .route(
            "/api/v1/conversations/{conversation_id}",
            delete(delete_conversation),
        )
        .route(
            "/api/v1/conversations/{conversation_id}/read",
            post(read_conversation),
        )
}

#[derive(Deserialize)]
struct ConversationPath {
    conversation_id: Uuid,
}

#[derive(Deserialize)]
struct ConversationParams {
    limit: Option<u64>,
    max_id: Option<String>,
    since_id: Option<String>,
    min_id: Option<String>,
}

#[derive(Serialize)]
struct ConversationResponse {
    id: String,
    unread: bool,
    accounts: Vec<AccountResponse>,
    last_status: Option<StatusResponse>,
}

#[derive(Serialize)]
struct ErrorResponse<'a> {
    error: &'a str,
    error_description: &'a str,
}

/// Return direct-message conversations visible to the authenticated account.
async fn conversations(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Query(params): Query<ConversationParams>,
) -> Response {
    let limit = conversation_limit(params.limit);
    let cursor = match collection_cursor(&params) {
        Ok(cursor) => cursor,
        Err(()) => return bad_request("conversation cursor is invalid"),
    };

    match roosty_db::local_conversations_for_account(&state.db, account.id, limit, cursor).await {
        Ok(page) => conversation_page_response(&state, account.id, page, limit).await,
        Err(error) => server_error(error),
    }
}

/// Hide one direct-message conversation for the authenticated account.
async fn delete_conversation(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<ConversationPath>,
) -> Response {
    match roosty_db::hide_local_conversation(&state.db, account.id, path.conversation_id).await {
        Ok(true) => Json(json!({})).into_response(),
        Ok(false) => not_found(),
        Err(error) => server_error(error),
    }
}

/// Mark one direct-message conversation as read for the authenticated account.
async fn read_conversation(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<ConversationPath>,
) -> Response {
    match roosty_db::mark_local_conversation_read(&state.db, account.id, path.conversation_id).await
    {
        Ok(Some(conversation)) => {
            match conversation_response(&state, account.id, conversation).await {
                Ok(response) => Json(response).into_response(),
                Err(error) => server_error(error),
            }
        }
        Ok(None) => not_found(),
        Err(error) => server_error(error),
    }
}

async fn conversation_page_response(
    state: &AppState,
    account_id: AccountId,
    page: roosty_db::CollectionPage<roosty_db::LocalConversationView>,
    limit: u64,
) -> Response {
    let link_header = CollectionLink::new(
        limit,
        page.first_cursor,
        page.last_cursor,
        page.has_more,
        "/api/v1/conversations",
    )
    .header_value();
    let mut conversations = Vec::with_capacity(page.items.len());
    for conversation in page.items {
        match conversation_response(state, account_id, conversation).await {
            Ok(response) => conversations.push(response),
            Err(error) => return server_error(error),
        }
    }
    let mut response = Json(conversations).into_response();
    if let Some(link_header) = link_header {
        response.headers_mut().insert(header::LINK, link_header);
    }
    response
}

async fn conversation_response(
    state: &AppState,
    account_id: AccountId,
    view: roosty_db::LocalConversationView,
) -> Result<ConversationResponse, RoostyError> {
    let accounts = conversation_accounts(state, account_id, view.conversation.id).await?;
    let last_status = match view.conversation.last_status_id {
        Some(status_id) => conversation_status(state, account_id, status_id).await?,
        None => None,
    };

    Ok(ConversationResponse {
        id: view.account.id.to_string(),
        unread: view.account.unread,
        accounts,
        last_status,
    })
}

/// Publish updated conversation payloads to each local participant's direct stream.
pub(crate) async fn publish_conversation_update(
    state: &AppState,
    conversation_id: Uuid,
) -> Result<(), RoostyError> {
    for view in roosty_db::local_conversation_views(&state.db, conversation_id).await? {
        let account_id = view.account.account_id;
        let response = conversation_response(state, account_id, view).await?;
        state
            .streaming_events
            .publish_conversation(&response, account_id);
    }

    Ok(())
}

async fn conversation_accounts(
    state: &AppState,
    account_id: AccountId,
    conversation_id: Uuid,
) -> Result<Vec<AccountResponse>, RoostyError> {
    let participants =
        roosty_db::local_conversation_participants(&state.db, conversation_id).await?;
    let mut accounts = Vec::new();
    for participant in participants {
        if participant.id != account_id {
            accounts.push(account_response(state, participant).await?);
        }
    }

    Ok(accounts)
}

async fn conversation_status(
    state: &AppState,
    account_id: AccountId,
    status_id: StatusId,
) -> Result<Option<StatusResponse>, RoostyError> {
    let Some(status) = roosty_db::find_local_status_by_id(&state.db, status_id).await? else {
        return Ok(None);
    };
    if !crate::statuses::status_visible_to_viewer(state, &status, Some(account_id)).await? {
        return Ok(None);
    }

    crate::statuses::status_with_author(state, status, Some(account_id))
        .await
        .map(Some)
}

fn conversation_limit(limit: Option<u64>) -> u64 {
    limit
        .unwrap_or(DEFAULT_CONVERSATION_LIMIT)
        .clamp(1, MAX_CONVERSATION_LIMIT)
}

fn collection_cursor(params: &ConversationParams) -> Result<roosty_db::CollectionCursor, ()> {
    Ok(roosty_db::CollectionCursor {
        max_id: parse_optional_uuid(params.max_id.as_deref())?,
        since_id: parse_optional_uuid(params.since_id.as_deref())?,
        min_id: parse_optional_uuid(params.min_id.as_deref())?,
    })
}

fn parse_optional_uuid(value: Option<&str>) -> Result<Option<Uuid>, ()> {
    value.map(Uuid::parse_str).transpose().map_err(|_| ())
}

fn bad_request(message: &'static str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ErrorResponse {
            error: "bad_request",
            error_description: message,
        }),
    )
        .into_response()
}

fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorResponse {
            error: "not_found",
            error_description: "conversation was not found",
        }),
    )
        .into_response()
}

fn server_error(error: RoostyError) -> Response {
    let description = error.to_string();
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({
            "error": "server_error",
            "error_description": description,
        })),
    )
        .into_response()
}
