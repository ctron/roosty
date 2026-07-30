use std::collections::HashSet;

use axum::{
    Extension, Json, Router,
    extract::{Path, Query, State},
    http::header,
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use roosty_core::{AccountId, RoostyError, StatusId};
use sea_orm::ConnectionTrait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::{
    accounts::{
        RemoteAccountResponse, remote_account_response_on, unresolved_remote_account_response,
    },
    auth::{AccountResponse, AuthenticatedAccount, account_response},
    http::{ApiError, ApiResult, AppState, DatabaseContext},
    statuses::{
        CollectionLink, StatusRenderContext, StatusResponse, remote_status_response,
        status_visible_to_viewer, status_with_author,
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

/// Return direct-message conversations visible to the authenticated account.
async fn conversations(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Query(params): Query<ConversationParams>,
) -> ApiResult<Response> {
    let limit = conversation_limit(params.limit);
    let cursor = collection_cursor(&params)
        .map_err(|()| ApiError::BadRequest("conversation cursor is invalid".into()))?;
    let txn = database.begin_snapshot().await?;
    let page = roosty_db::local_conversations_for_account(&txn, account.id, limit, cursor).await?;
    let response = conversation_page_response(&state, &txn, account.id, page, limit).await?;
    txn.commit().await?;
    Ok(response)
}

/// Hide one direct-message conversation for the authenticated account.
async fn delete_conversation(
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<ConversationPath>,
) -> ApiResult<Json<serde_json::Value>> {
    let txn = database.begin_write().await?;
    if !roosty_db::hide_local_conversation(&txn, account.id, path.conversation_id).await? {
        return Err(ApiError::NotFound("conversation was not found".into()));
    }
    txn.commit().await?;
    Ok(Json(json!({})))
}

/// Mark one direct-message conversation as read for the authenticated account.
async fn read_conversation(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<ConversationPath>,
) -> ApiResult<Json<ConversationResponse>> {
    let txn = database.begin_write().await?;
    let conversation =
        roosty_db::mark_local_conversation_read(&txn, account.id, path.conversation_id)
            .await?
            .ok_or_else(|| ApiError::NotFound("conversation was not found".into()))?;
    let response = conversation_response(&state, &txn, account.id, conversation).await?;
    txn.commit().await?;
    Ok(Json(response))
}

async fn conversation_page_response(
    state: &AppState,
    db: &impl ConnectionTrait,
    account_id: AccountId,
    page: roosty_db::CollectionPage<roosty_db::LocalConversationView>,
    limit: u64,
) -> Result<Response, RoostyError> {
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
        if conversation_is_hidden(db, account_id, &conversation.account).await? {
            continue;
        }
        conversations.push(conversation_response(state, db, account_id, conversation).await?);
    }
    let mut response = Json(conversations).into_response();
    if let Some(link_header) = link_header {
        response.headers_mut().insert(header::LINK, link_header);
    }
    Ok(response)
}

async fn conversation_is_hidden(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    view: &roosty_db::LocalConversationAccount,
) -> Result<bool, RoostyError> {
    let Some(status_id) = view.last_remote_status_id else {
        return Ok(false);
    };
    let Some(status) = roosty_db::find_remote_status_by_id(db, status_id).await? else {
        return Ok(false);
    };
    if roosty_db::remote_account_is_hidden_for_viewer(db, account_id, status.remote_actor_id)
        .await?
    {
        return Ok(true);
    }
    let Some(actor) = roosty_db::find_remote_actor_by_id(db, status.remote_actor_id).await? else {
        return Ok(true);
    };
    Ok(actor.suspended_at.is_some()
        || roosty_db::federation_domain_policy(db, &actor.domain)
            .await?
            .is_suspended())
}

async fn conversation_response(
    state: &AppState,
    db: &impl ConnectionTrait,
    account_id: AccountId,
    view: roosty_db::LocalConversationView,
) -> Result<ConversationResponse, RoostyError> {
    let accounts = conversation_accounts(state, db, account_id, &view.account).await?;
    let last_status = match (
        view.account.last_status_id,
        view.account.last_remote_status_id,
    ) {
        (Some(status_id), None) => conversation_status(state, db, account_id, status_id).await?,
        (None, Some(status_id)) => remote_conversation_status(state, db, status_id).await?,
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
    db: &impl ConnectionTrait,
    conversation_id: Uuid,
) -> Result<(), RoostyError> {
    for view in roosty_db::local_conversation_views(db, conversation_id).await? {
        let account_id = view.account.account_id;
        let response = conversation_response(state, db, account_id, view).await?;
        state
            .streaming_events
            .publish_conversation(&response, account_id);
    }

    Ok(())
}

/// Publish only recipient views whose latest visible direct status changed.
pub(crate) async fn publish_conversation_updates(
    state: &AppState,
    db: &impl ConnectionTrait,
    conversation_id: Uuid,
    account_ids: &[AccountId],
) -> Result<(), RoostyError> {
    let account_ids = account_ids.iter().copied().collect::<HashSet<_>>();
    for view in roosty_db::local_conversation_views(db, conversation_id).await? {
        let account_id = view.account.account_id;
        if account_ids.contains(&account_id) {
            let response = conversation_response(state, db, account_id, view).await?;
            state
                .streaming_events
                .publish_conversation(&response, account_id);
        }
    }

    Ok(())
}

async fn conversation_accounts(
    state: &AppState,
    db: &impl ConnectionTrait,
    account_id: AccountId,
    view: &roosty_db::LocalConversationAccount,
) -> Result<Vec<ConversationAccountResponse>, RoostyError> {
    let participants = roosty_db::direct_status_participants_for_view(db, view).await?;
    let mut accounts = Vec::new();
    for participant in participants.local_accounts {
        if participant.id != account_id {
            accounts.push(ConversationAccountResponse::Local(Box::new(
                account_response(state, db, participant).await?,
            )));
        }
    }

    for participant in participants.remote_accounts {
        if let Some(id) = participant.remote_actor_id {
            let actor = roosty_db::find_remote_actor_by_id(db, id).await?;
            let domain_suspended = match actor {
                Some(actor) => {
                    actor.suspended_at.is_some()
                        || roosty_db::federation_domain_policy(db, &actor.domain)
                            .await?
                            .is_suspended()
                }
                None => true,
            };
            if roosty_db::remote_account_is_hidden_for_viewer(db, account_id, id).await?
                || domain_suspended
            {
                continue;
            }
        }
        let response = match participant.remote_actor_id {
            Some(id) => match roosty_db::find_remote_actor_by_id(db, id).await? {
                Some(actor) => remote_account_response_on(state, db, actor).await?,
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
    db: &impl ConnectionTrait,
    status_id: StatusId,
) -> Result<Option<StatusResponse>, RoostyError> {
    let Some(status) = roosty_db::find_remote_status_by_id(db, status_id).await? else {
        return Ok(None);
    };
    let context = StatusRenderContext::new(state, db);
    remote_status_response(&context, status).await.map(Some)
}

async fn conversation_status(
    state: &AppState,
    db: &impl ConnectionTrait,
    account_id: AccountId,
    status_id: StatusId,
) -> Result<Option<StatusResponse>, RoostyError> {
    let Some(status) = roosty_db::find_local_status_by_id(db, status_id).await? else {
        return Ok(None);
    };
    if !status_visible_to_viewer(db, &status, Some(account_id)).await? {
        return Ok(None);
    }

    let context = StatusRenderContext::new(state, db);
    status_with_author(&context, status, Some(account_id))
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
