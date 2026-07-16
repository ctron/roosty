use std::collections::HashSet;

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
    accounts::{
        RemoteAccountResponse, remote_account_response, unresolved_remote_account_response,
    },
    auth::{AccountResponse, AuthenticatedAccount, account_response},
    http::AppState,
    statuses::{
        CollectionLink, StatusResponse, remote_status_response, status_visible_to_viewer,
        status_with_author,
    },
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
    accounts: Vec<ConversationAccountResponse>,
    last_status: Option<StatusResponse>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum ConversationAccountResponse {
    Local(Box<AccountResponse>),
    Remote(Box<RemoteAccountResponse>),
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
        if conversation_is_hidden(state, account_id, &conversation.account).await {
            continue;
        }
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

async fn conversation_is_hidden(
    state: &AppState,
    account_id: AccountId,
    view: &roosty_db::LocalConversationAccount,
) -> bool {
    let Some(status_id) = view.last_remote_status_id else {
        return false;
    };
    let Ok(Some(status)) = roosty_db::find_remote_status_by_id(&state.db, status_id).await else {
        return false;
    };
    if roosty_db::remote_account_is_hidden_for_viewer(&state.db, account_id, status.remote_actor_id)
        .await
        .unwrap_or(true)
    {
        return true;
    }
    let Ok(Some(actor)) =
        roosty_db::find_remote_actor_by_id(&state.db, status.remote_actor_id).await
    else {
        return true;
    };
    state.config.federation_domain_is_blocked(&actor.domain)
}

async fn conversation_response(
    state: &AppState,
    account_id: AccountId,
    view: roosty_db::LocalConversationView,
) -> Result<ConversationResponse, RoostyError> {
    let accounts = conversation_accounts(state, account_id, &view.account).await?;
    let last_status = match (
        view.account.last_status_id,
        view.account.last_remote_status_id,
    ) {
        (Some(status_id), None) => conversation_status(state, account_id, status_id).await?,
        (None, Some(status_id)) => remote_conversation_status(state, status_id).await?,
        _ => None,
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

/// Publish only recipient views whose latest visible direct status changed.
pub(crate) async fn publish_conversation_updates(
    state: &AppState,
    conversation_id: Uuid,
    account_ids: &[AccountId],
) -> Result<(), RoostyError> {
    let account_ids = account_ids.iter().copied().collect::<HashSet<_>>();
    for view in roosty_db::local_conversation_views(&state.db, conversation_id).await? {
        let account_id = view.account.account_id;
        if account_ids.contains(&account_id) {
            let response = conversation_response(state, account_id, view).await?;
            state
                .streaming_events
                .publish_conversation(&response, account_id);
        }
    }

    Ok(())
}

async fn conversation_accounts(
    state: &AppState,
    account_id: AccountId,
    view: &roosty_db::LocalConversationAccount,
) -> Result<Vec<ConversationAccountResponse>, RoostyError> {
    let participants = roosty_db::direct_status_participants_for_view(&state.db, view).await?;
    let mut accounts = Vec::new();
    for participant in participants.local_accounts {
        if participant.id != account_id {
            accounts.push(ConversationAccountResponse::Local(Box::new(
                account_response(state, participant).await?,
            )));
        }
    }

    for participant in participants.remote_accounts {
        if let Some(id) = participant.remote_actor_id
            && (roosty_db::remote_account_is_hidden_for_viewer(&state.db, account_id, id).await?
                || roosty_db::find_remote_actor_by_id(&state.db, id)
                    .await?
                    .is_some_and(|actor| state.config.federation_domain_is_blocked(&actor.domain)))
        {
            continue;
        }
        let response = match participant.remote_actor_id {
            Some(id) => match roosty_db::find_remote_actor_by_id(&state.db, id).await? {
                Some(actor) => remote_account_response(state, actor).await?,
                None => unresolved_remote_account_response(
                    &participant.activitypub_id,
                    participant.mention_name.as_deref(),
                ),
            },
            None => unresolved_remote_account_response(
                &participant.activitypub_id,
                participant.mention_name.as_deref(),
            ),
        };
        accounts.push(ConversationAccountResponse::Remote(Box::new(response)));
    }

    Ok(accounts)
}

async fn remote_conversation_status(
    state: &AppState,
    status_id: StatusId,
) -> Result<Option<StatusResponse>, RoostyError> {
    let Some(status) = roosty_db::find_remote_status_by_id(&state.db, status_id).await? else {
        return Ok(None);
    };
    remote_status_response(state, status).await.map(Some)
}

async fn conversation_status(
    state: &AppState,
    account_id: AccountId,
    status_id: StatusId,
) -> Result<Option<StatusResponse>, RoostyError> {
    let Some(status) = roosty_db::find_local_status_by_id(&state.db, status_id).await? else {
        return Ok(None);
    };
    if !status_visible_to_viewer(state, &status, Some(account_id)).await? {
        return Ok(None);
    }

    status_with_author(state, status, Some(account_id))
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
