use axum::{
    Json, Router,
    body::to_bytes,
    extract::{Path, Query, RawQuery, Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use roosty_core::{AccountId, FederationDiscoveryError, RoostyError, StatusId};
use roosty_db::{LocalNotificationType, RemoteActor, RemoteProfileMediaKind};
use sea_orm::{AccessMode, TransactionTrait};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tracing::warn;
use uuid::Uuid;

use crate::{
    auth::{AccountResponse, AuthenticatedAccount, OptionalAuthenticatedAccount, account_response},
    http::AppState,
    statuses::CollectionLink,
};

const DEFAULT_ACCOUNT_LIMIT: u64 = 40;
const MAX_ACCOUNT_LIMIT: u64 = 80;

/// Build routes for Mastodon-compatible account lookup and local follows.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/accounts/relationships", get(relationships))
        .route("/api/v1/follow_requests", get(follow_requests))
        .route(
            "/api/v1/follow_requests/{account_id}/authorize",
            post(authorize_follow_request),
        )
        .route(
            "/api/v1/follow_requests/{account_id}/reject",
            post(reject_follow_request),
        )
        .route("/api/v1/accounts/lookup", get(lookup_account))
        .route("/api/v1/accounts/{account_id}", get(show_account))
        .route(
            "/api/v1/accounts/{account_id}/statuses",
            get(account_statuses),
        )
        .route("/api/v1/accounts/{account_id}/follow", post(follow))
        .route("/api/v1/accounts/{account_id}/unfollow", post(unfollow))
        .route("/api/v1/accounts/{account_id}/block", post(block))
        .route("/api/v1/accounts/{account_id}/unblock", post(unblock))
        .route("/api/v1/accounts/{account_id}/mute", post(mute))
        .route("/api/v1/accounts/{account_id}/unmute", post(unmute))
        .route("/api/v1/accounts/{account_id}/followers", get(followers))
        .route("/api/v1/accounts/{account_id}/following", get(following))
        .route("/api/v1/blocks", get(blocked_accounts))
        .route("/api/v1/mutes", get(muted_accounts))
}

#[derive(Deserialize)]
struct AccountPath {
    account_id: Uuid,
}

#[derive(Deserialize)]
struct AccountStatusesParams {
    limit: Option<u64>,
    max_id: Option<String>,
    since_id: Option<String>,
    min_id: Option<String>,
    exclude_replies: Option<bool>,
    exclude_reblogs: Option<bool>,
    only_media: Option<bool>,
    pinned: Option<bool>,
    tagged: Option<String>,
}

#[derive(Deserialize)]
struct AccountCollectionParams {
    limit: Option<u64>,
    max_id: Option<String>,
    since_id: Option<String>,
    min_id: Option<String>,
}

#[derive(Deserialize)]
struct LookupParams {
    acct: Option<String>,
    resolve: Option<bool>,
}

#[derive(Serialize)]
pub(crate) struct RemoteAccountResponse {
    id: String,
    username: String,
    acct: String,
    display_name: String,
    locked: bool,
    bot: bool,
    discoverable: Option<bool>,
    group: bool,
    created_at: String,
    note: String,
    url: String,
    avatar: String,
    avatar_static: String,
    header: String,
    header_static: String,
    fields: Vec<Value>,
    emojis: Vec<Value>,
    followers_count: u64,
    following_count: u64,
    statuses_count: u64,
    last_status_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    moved: Option<Box<RemoteAccountResponse>>,
}

/// Mastodon account projection used by collections containing local and remote actors.
#[derive(Serialize)]
#[serde(untagged)]
enum CollectionAccountResponse {
    Local(Box<AccountResponse>),
    Remote(Box<RemoteAccountResponse>),
}

#[derive(Default, Deserialize)]
struct FollowInput {
    reblogs: Option<bool>,
    notify: Option<bool>,
}

/// Mute settings accepted by Mastodon's account mute endpoint.
#[derive(Default, Deserialize)]
struct MuteInput {
    notifications: Option<bool>,
    duration: Option<u64>,
}

#[derive(Deserialize)]
struct RelationshipsParams {
    id: Vec<Uuid>,
}

#[derive(Clone, Copy)]
enum AccountCollection {
    Followers,
    Following,
    /// Accounts blocked by the authenticated account.
    Blocks,
    /// Accounts muted by the authenticated account.
    Mutes,
}

#[derive(Serialize)]
struct RelationshipResponse {
    id: String,
    following: bool,
    showing_reblogs: bool,
    notifying: bool,
    followed_by: bool,
    blocking: bool,
    blocked_by: bool,
    muting: bool,
    muting_notifications: bool,
    muting_expires_at: Option<String>,
    requested: bool,
    domain_blocking: bool,
    endorsed: bool,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

/// Return a public local account profile by local username or address.
async fn lookup_account(
    State(state): State<AppState>,
    Query(params): Query<LookupParams>,
) -> Response {
    if let Some(username) = local_lookup_username(&state, params.acct.as_deref()) {
        return match roosty_db::find_local_account_by_username(&state.db, &username).await {
            Ok(Some(account)) => match account_response(&state, account).await {
                Ok(response) => Json(response).into_response(),
                Err(error) => server_error(error),
            },
            Ok(None) => not_found(),
            Err(error) => server_error(error),
        };
    }

    if let Some((username, domain)) = params
        .acct
        .as_deref()
        .and_then(crate::federation::discovery::exact_remote_handle)
    {
        match roosty_db::find_remote_actor_by_handle(&state.db, &username, &domain).await {
            Ok(Some(actor))
                if actor.deleted_at.is_none()
                    && (!params.resolve.unwrap_or(false)
                        || actor.expires_at > time::OffsetDateTime::now_utc()) =>
            {
                return match remote_account_response(&state, actor).await {
                    Ok(response) => Json(response).into_response(),
                    Err(error) => server_error(error),
                };
            }
            Ok(_) => {}
            Err(error) => return server_error(error),
        }
    }

    if !params.resolve.unwrap_or(false) || !state.config.federation_enabled {
        return not_found();
    }
    let Some(acct) = params.acct.as_deref() else {
        return not_found();
    };
    match crate::federation::discovery::resolve_remote_actor(&state, acct).await {
        Ok(actor) if actor.deleted_at.is_some() => not_found(),
        Ok(actor) => match remote_account_response(&state, actor).await {
            Ok(response) => Json(response).into_response(),
            Err(error) => server_error(error),
        },
        Err(RoostyError::FederationDiscovery(FederationDiscoveryError::PolicyRejected(_))) => {
            not_found()
        }
        Err(RoostyError::InvalidInput(error)) => bad_request(&error),
        Err(error) => server_error(error),
    }
}

/// Convert a cached remote actor to the public Mastodon account projection.
pub(crate) async fn remote_account_response(
    state: &AppState,
    actor: RemoteActor,
) -> roosty_core::Result<RemoteAccountResponse> {
    let statuses_count = roosty_db::count_remote_statuses_by_account(&state.db, actor.id).await?;
    let last_status_at = roosty_db::last_remote_status_at(&state.db, actor.id)
        .await?
        .map(crate::statuses::format_timestamp);
    let txn = state
        .db
        .begin_with_config(None, Some(AccessMode::ReadOnly))
        .await?;
    let profile_media = roosty_db::remote_profile_media_for_actor(&txn, actor.id).await?;
    txn.commit().await?;
    let media_url = |kind| {
        profile_media
            .iter()
            .find(|media| media.kind == kind)
            .map(|media| crate::media::remote_profile_media_url(state, media.id))
            .unwrap_or_default()
    };
    let avatar = media_url(RemoteProfileMediaKind::Avatar);
    let header = media_url(RemoteProfileMediaKind::Header);
    let moved_to_remote_actor_id = actor.moved_to_remote_actor_id;
    let mut response = remote_account_response_from_media(actor, avatar, header);
    response.statuses_count = statuses_count;
    response.last_status_at = last_status_at;
    if let Some(moved_to_remote_actor_id) = moved_to_remote_actor_id
        && let Some(mut moved) =
            roosty_db::find_remote_actor_by_id(&state.db, moved_to_remote_actor_id).await?
    {
        // Mastodon exposes one replacement account; suppress nested moves to avoid cycles.
        moved.moved_to_remote_actor_id = None;
        response.moved = Some(Box::new(
            Box::pin(remote_account_response(state, moved)).await?,
        ));
    }
    Ok(response)
}

/// Project an unresolved direct-message participant without fetching its actor document.
pub(crate) fn unresolved_remote_account_response(
    activitypub_id: &str,
    mention_name: Option<&str>,
) -> RemoteAccountResponse {
    let acct = mention_name
        .and_then(|name| name.strip_prefix('@'))
        .unwrap_or(activitypub_id)
        .to_owned();
    let username = acct.split('@').next().unwrap_or(&acct).to_owned();
    RemoteAccountResponse {
        id: activitypub_id.to_owned(),
        username,
        acct,
        display_name: String::new(),
        locked: false,
        bot: false,
        discoverable: None,
        group: false,
        created_at: crate::statuses::format_timestamp(time::OffsetDateTime::now_utc()),
        note: String::new(),
        url: activitypub_id.to_owned(),
        avatar: String::new(),
        avatar_static: String::new(),
        header: String::new(),
        header_static: String::new(),
        fields: Vec::new(),
        emojis: Vec::new(),
        followers_count: 0,
        following_count: 0,
        statuses_count: 0,
        last_status_at: None,
        moved: None,
    }
}

fn remote_account_response_from_media(
    actor: RemoteActor,
    avatar: String,
    header: String,
) -> RemoteAccountResponse {
    RemoteAccountResponse {
        id: actor.id.0.to_string(),
        username: actor.username.clone(),
        acct: format!("{}@{}", actor.username, actor.domain),
        display_name: actor.display_name,
        locked: false,
        bot: false,
        discoverable: None,
        group: false,
        created_at: crate::statuses::format_timestamp(
            actor.profile_created_at.unwrap_or(actor.first_seen_at),
        ),
        note: actor.summary,
        url: actor.activitypub_id,
        avatar: avatar.clone(),
        avatar_static: avatar,
        header: header.clone(),
        header_static: header,
        fields: Vec::new(),
        emojis: remote_custom_emojis(&actor.emojis),
        followers_count: 0,
        following_count: 0,
        statuses_count: 0,
        last_status_at: None,
        moved: None,
    }
}

/// Project valid Mastodon ActivityPub Emoji tags into the REST custom-emoji shape.
pub(crate) fn remote_custom_emojis(tags: &Value) -> Vec<Value> {
    let Some(tags) = tags
        .as_array()
        .or_else(|| tags.get("tag").and_then(Value::as_array))
    else {
        return Vec::new();
    };
    tags.iter()
        .filter_map(|tag| {
            let kind = tag.get("type").and_then(Value::as_str)?;
            if kind != "Emoji" && kind != "http://joinmastodon.org/ns#Emoji" {
                return None;
            }
            let name = tag.get("name").and_then(Value::as_str)?;
            let shortcode = name.strip_prefix(':')?.strip_suffix(':')?;
            if shortcode.is_empty() || shortcode.chars().any(char::is_whitespace) {
                return None;
            }
            let icon = tag.get("icon")?;
            let url = match icon.get("url")? {
                Value::String(url) => url,
                Value::Object(url) => url.get("href")?.as_str()?,
                _ => return None,
            };
            (url.starts_with("https://")).then(|| {
                json!({
                    "shortcode": shortcode,
                    "url": url,
                    "static_url": url,
                    "visible_in_picker": false,
                    "category": null,
                })
            })
        })
        .collect()
}

/// Return a public local account profile by account id.
async fn show_account(State(state): State<AppState>, Path(path): Path<AccountPath>) -> Response {
    let account_id = AccountId(path.account_id);
    match roosty_db::find_local_account_by_id(&state.db, account_id).await {
        Ok(Some(account)) => match account_response(&state, account).await {
            Ok(response) => Json(response).into_response(),
            Err(error) => server_error(error),
        },
        Ok(None) => match roosty_db::find_remote_actor_by_id(&state.db, account_id).await {
            Ok(Some(actor)) if actor.deleted_at.is_none() => {
                match remote_account_response(&state, actor).await {
                    Ok(response) => Json(response).into_response(),
                    Err(error) => server_error(error),
                }
            }
            Ok(_) => not_found(),
            Err(error) => server_error(error),
        },
        Err(error) => server_error(error),
    }
}

/// Return statuses authored by one local account.
async fn account_statuses(
    State(state): State<AppState>,
    OptionalAuthenticatedAccount(viewer): OptionalAuthenticatedAccount,
    Path(path): Path<AccountPath>,
    Query(params): Query<AccountStatusesParams>,
) -> Response {
    let account_id = AccountId(path.account_id);
    if params.pinned.unwrap_or(false) {
        return Json(Vec::<serde_json::Value>::new()).into_response();
    }

    let cursor = match timeline_cursor(&params) {
        Ok(cursor) => cursor,
        Err(()) => return bad_request("status id is invalid"),
    };
    let limit = crate::statuses::timeline_limit(params.limit);
    let local = match roosty_db::find_local_account_by_id(&state.db, account_id).await {
        Ok(Some(_)) => true,
        Ok(None) => false,
        Err(error) => return server_error(error),
    };
    if !local {
        match roosty_db::find_remote_actor_by_id(&state.db, account_id).await {
            Ok(Some(actor)) if actor.deleted_at.is_none() => {}
            Ok(_) => return not_found(),
            Err(error) => return server_error(error),
        }
        return match roosty_db::remote_statuses_by_account(
            &state.db,
            account_id,
            viewer.as_ref().map(|account| account.id),
            limit,
            cursor,
            roosty_db::AccountStatusTimelineOptions {
                exclude_replies: params.exclude_replies.unwrap_or(false),
                only_media: params.only_media.unwrap_or(false),
                tagged: params.tagged.clone().filter(|tag| !tag.trim().is_empty()),
            },
        )
        .await
        {
            Ok(page) => {
                crate::statuses::remote_timeline_response(
                    &state,
                    page,
                    limit,
                    &format!("/api/v1/accounts/{}/statuses", account_id.0),
                    viewer.as_ref().map(|account| account.id),
                )
                .await
            }
            Err(error) => server_error(error),
        };
    }

    match roosty_db::local_statuses_by_account(
        &state.db,
        account_id,
        viewer.as_ref().map(|account| account.id),
        limit,
        cursor,
        roosty_db::AccountStatusTimelineOptions {
            exclude_replies: params.exclude_replies.unwrap_or(false),
            only_media: params.only_media.unwrap_or(false),
            tagged: params.tagged.clone().filter(|tag| !tag.trim().is_empty()),
        },
    )
    .await
    {
        Ok(page) => {
            if params.exclude_reblogs.unwrap_or(false) {
                // Account status collections currently return authored statuses only.
            }
            crate::statuses::timeline_response(
                &state,
                page,
                limit,
                &format!("/api/v1/accounts/{}/statuses", account_id.0),
                viewer.as_ref().map(|account| account.id),
            )
            .await
        }
        Err(error) => server_error(error),
    }
}

/// Follow a local account and return the resulting relationship.
async fn follow(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<AccountPath>,
    request: Request,
) -> Response {
    let input = match follow_input(request).await {
        Ok(input) => input,
        Err(error) => return bad_request(&error),
    };
    let target_id = AccountId(path.account_id);

    if roosty_db::find_remote_actor_by_id(&state.db, target_id)
        .await
        .ok()
        .flatten()
        .is_some()
    {
        let (activity_id, job) =
            match crate::federation::prepare_remote_follow(&state, account.id, target_id).await {
                Ok(prepared) => prepared,
                Err(error) => return server_error(error),
            };
        let txn = match state.db.begin().await {
            Ok(txn) => txn,
            Err(error) => return server_error(error.into()),
        };
        return match roosty_db::create_remote_following_with_job(
            &txn,
            account.id,
            target_id,
            &activity_id,
            job,
        )
        .await
        {
            Ok(_) => match txn.commit().await {
                Ok(()) => relationship_response(&state, account.id, target_id).await,
                Err(error) => server_error(error.into()),
            },
            Err(error) => server_error(error),
        };
    }

    match roosty_db::follow_local_account(
        &state.db,
        account.id,
        target_id,
        input.reblogs.unwrap_or(true),
        input.notify.unwrap_or(false),
    )
    .await
    {
        Ok(_) => {
            if let Err(error) = crate::notifications::create_and_stream_notification(
                &state,
                target_id,
                LocalNotificationType::Follow,
                account.id,
                None,
            )
            .await
            {
                warn!(%error, "failed to create follow notification");
            }
            relationship_response(&state, account.id, target_id).await
        }
        Err(RoostyError::InvalidInput(error)) if error == "followed account does not exist" => {
            not_found()
        }
        Err(RoostyError::InvalidInput(error))
            if error == "follow is blocked by an account relationship" =>
        {
            forbidden(&error)
        }
        Err(RoostyError::InvalidInput(error)) => bad_request(&error),
        Err(error) => server_error(error),
    }
}

/// Block a local account and return the resulting relationship.
async fn block(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<AccountPath>,
) -> Response {
    let target_id = AccountId(path.account_id);
    let txn = match state.db.begin().await {
        Ok(txn) => txn,
        Err(error) => return server_error(error.into()),
    };
    match roosty_db::block_local_account(&txn, account.id, target_id).await {
        Ok(()) => match txn.commit().await {
            Ok(()) => relationship_response(&state, account.id, target_id).await,
            Err(error) => server_error(error.into()),
        },
        Err(RoostyError::InvalidInput(error)) if error == "target account does not exist" => {
            not_found()
        }
        Err(RoostyError::InvalidInput(error)) => bad_request(&error),
        Err(error) => server_error(error),
    }
}

/// Remove a local block and return the resulting relationship.
async fn unblock(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<AccountPath>,
) -> Response {
    let target_id = AccountId(path.account_id);
    match roosty_db::unblock_local_account(&state.db, account.id, target_id).await {
        Ok(()) => relationship_response(&state, account.id, target_id).await,
        Err(error) => server_error(error),
    }
}

/// Mute a local account and return the resulting relationship.
async fn mute(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<AccountPath>,
    request: Request,
) -> Response {
    let input = match mute_input(request).await {
        Ok(input) => input,
        Err(error) => return bad_request(&error),
    };
    let target_id = AccountId(path.account_id);
    match roosty_db::mute_local_account(
        &state.db,
        account.id,
        target_id,
        input.notifications.unwrap_or(true),
        input.duration.unwrap_or(0),
    )
    .await
    {
        Ok(_) => relationship_response(&state, account.id, target_id).await,
        Err(RoostyError::InvalidInput(error)) if error == "target account does not exist" => {
            not_found()
        }
        Err(RoostyError::InvalidInput(error)) => bad_request(&error),
        Err(error) => server_error(error),
    }
}

/// Remove a local mute and return the resulting relationship.
async fn unmute(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<AccountPath>,
) -> Response {
    let target_id = AccountId(path.account_id);
    match roosty_db::unmute_local_account(&state.db, account.id, target_id).await {
        Ok(()) => relationship_response(&state, account.id, target_id).await,
        Err(error) => server_error(error),
    }
}

/// Unfollow a local account and return the resulting relationship.
async fn unfollow(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<AccountPath>,
) -> Response {
    let target_id = AccountId(path.account_id);
    let remote_following =
        match roosty_db::find_remote_following(&state.db, account.id, target_id).await {
            Ok(following) => following,
            Err(error) => return server_error(error),
        };
    if let Some(following) = remote_following {
        let job = match crate::federation::prepare_remote_unfollow(&state, following).await {
            Ok(job) => job,
            Err(error) => return server_error(error),
        };
        let txn = match state.db.begin().await {
            Ok(txn) => txn,
            Err(error) => return server_error(error.into()),
        };
        return match roosty_db::delete_remote_following_with_job(&txn, account.id, target_id, job)
            .await
        {
            Ok(_) => match txn.commit().await {
                Ok(()) => relationship_response(&state, account.id, target_id).await,
                Err(error) => server_error(error.into()),
            },
            Err(error) => server_error(error),
        };
    }
    match roosty_db::unfollow_local_account(&state.db, account.id, target_id).await {
        Ok(()) => relationship_response(&state, account.id, target_id).await,
        Err(error) => server_error(error),
    }
}

/// Return Mastodon relationship objects for requested account ids.
async fn relationships(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    RawQuery(query): RawQuery,
) -> Response {
    let ids = match relationship_ids(query.as_deref()) {
        Ok(ids) => ids,
        Err(()) => return bad_request("account id is invalid"),
    };
    let mut relationships = Vec::with_capacity(ids.len());
    for id in ids {
        match relationship_model(&state, account.id, AccountId(id)).await {
            Ok(relationship) => relationships.push(relationship),
            Err(error) => return server_error(error),
        }
    }

    Json(relationships).into_response()
}

/// List remote actors whose follow requests await this account's approval.
async fn follow_requests(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Query(params): Query<AccountCollectionParams>,
) -> Response {
    let limit = params
        .limit
        .unwrap_or(DEFAULT_ACCOUNT_LIMIT)
        .clamp(1, MAX_ACCOUNT_LIMIT);
    let cursor = match collection_cursor(&params) {
        Ok(cursor) => cursor,
        Err(()) => return bad_request("collection cursor is invalid"),
    };
    match roosty_db::pending_remote_follow_requests(&state.db, account.id, limit, cursor).await {
        Ok(page) => {
            let mut actors = Vec::with_capacity(page.items.len());
            for actor in page.items {
                match remote_account_response(&state, actor).await {
                    Ok(actor) => actors.push(actor),
                    Err(error) => return server_error(error),
                }
            }
            let link_header = CollectionLink::new(
                limit,
                page.first_cursor,
                page.last_cursor,
                page.has_more,
                "/api/v1/follow_requests",
            )
            .header_value();
            let mut response = Json(actors).into_response();
            if let Some(link_header) = link_header {
                response.headers_mut().insert(header::LINK, link_header);
            }
            response
        }
        Err(error) => server_error(error),
    }
}

/// Approve a pending remote follow request for the authenticated local account.
async fn authorize_follow_request(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<AccountPath>,
) -> Response {
    match crate::federation::accept_remote_follow_request(
        &state,
        account.id,
        AccountId(path.account_id),
    )
    .await
    {
        Ok(true) => relationship_response(&state, account.id, AccountId(path.account_id)).await,
        Ok(false) => not_found(),
        Err(error) => server_error(error),
    }
}

/// Reject a pending remote follow request for the authenticated local account.
async fn reject_follow_request(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<AccountPath>,
) -> Response {
    let remote_id = AccountId(path.account_id);
    match crate::federation::reject_remote_follow_request(&state, account.id, remote_id).await {
        Ok(true) => relationship_response(&state, account.id, remote_id).await,
        Ok(false) => not_found(),
        Err(error) => server_error(error),
    }
}

/// Return local followers for a local account.
async fn followers(
    State(state): State<AppState>,
    Path(path): Path<AccountPath>,
    Query(params): Query<AccountCollectionParams>,
) -> Response {
    account_collection(
        &state,
        AccountId(path.account_id),
        params,
        AccountCollection::Followers,
    )
    .await
}

/// Return local accounts followed by a local account.
async fn following(
    State(state): State<AppState>,
    Path(path): Path<AccountPath>,
    Query(params): Query<AccountCollectionParams>,
) -> Response {
    account_collection(
        &state,
        AccountId(path.account_id),
        params,
        AccountCollection::Following,
    )
    .await
}

/// Return local accounts blocked by the authenticated account.
async fn blocked_accounts(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Query(params): Query<AccountCollectionParams>,
) -> Response {
    account_collection(&state, account.id, params, AccountCollection::Blocks).await
}

/// Return local accounts muted by the authenticated account.
async fn muted_accounts(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Query(params): Query<AccountCollectionParams>,
) -> Response {
    account_collection(&state, account.id, params, AccountCollection::Mutes).await
}

/// Return a local follower/following account collection.
async fn account_collection(
    state: &AppState,
    account_id: AccountId,
    params: AccountCollectionParams,
    collection: AccountCollection,
) -> Response {
    if !matches!(
        collection,
        AccountCollection::Blocks | AccountCollection::Mutes
    ) {
        match roosty_db::find_local_account_by_id(&state.db, account_id).await {
            Ok(Some(_)) => {}
            Ok(None) => return not_found(),
            Err(error) => return server_error(error),
        }
    }

    let limit = params
        .limit
        .unwrap_or(DEFAULT_ACCOUNT_LIMIT)
        .clamp(1, MAX_ACCOUNT_LIMIT);
    let cursor = match collection_cursor(&params) {
        Ok(cursor) => cursor,
        Err(()) => return bad_request("collection cursor is invalid"),
    };
    let accounts = match collection {
        AccountCollection::Followers => {
            roosty_db::followers_for_local_account(&state.db, account_id, limit, cursor)
                .await
                .map(|page| roosty_db::CollectionPage {
                    items: page.items.into_iter().map(|entry| entry.account).collect(),
                    first_cursor: page.first_cursor,
                    last_cursor: page.last_cursor,
                    has_more: page.has_more,
                })
        }
        AccountCollection::Following => {
            roosty_db::following_for_local_account(&state.db, account_id, limit, cursor)
                .await
                .map(|page| roosty_db::CollectionPage {
                    items: page.items.into_iter().map(|entry| entry.account).collect(),
                    first_cursor: page.first_cursor,
                    last_cursor: page.last_cursor,
                    has_more: page.has_more,
                })
        }
        AccountCollection::Blocks => {
            roosty_db::blocked_local_accounts_for_account(&state.db, account_id, limit, cursor)
                .await
                .map(|page| roosty_db::CollectionPage {
                    items: page
                        .items
                        .into_iter()
                        .map(roosty_db::FollowCollectionAccount::Local)
                        .collect(),
                    first_cursor: page.first_cursor,
                    last_cursor: page.last_cursor,
                    has_more: page.has_more,
                })
        }
        AccountCollection::Mutes => {
            roosty_db::muted_local_accounts_for_account(&state.db, account_id, limit, cursor)
                .await
                .map(|page| roosty_db::CollectionPage {
                    items: page
                        .items
                        .into_iter()
                        .map(roosty_db::FollowCollectionAccount::Local)
                        .collect(),
                    first_cursor: page.first_cursor,
                    last_cursor: page.last_cursor,
                    has_more: page.has_more,
                })
        }
    };
    match accounts {
        Ok(page) => match account_responses(state, page.items).await {
            Ok(accounts) => {
                let path = match collection {
                    AccountCollection::Followers => {
                        format!("/api/v1/accounts/{}/followers", account_id.0)
                    }
                    AccountCollection::Following => {
                        format!("/api/v1/accounts/{}/following", account_id.0)
                    }
                    AccountCollection::Blocks => "/api/v1/blocks".to_owned(),
                    AccountCollection::Mutes => "/api/v1/mutes".to_owned(),
                };
                let link_header = CollectionLink::new(
                    limit,
                    page.first_cursor,
                    page.last_cursor,
                    page.has_more,
                    &path,
                )
                .header_value();
                let mut response = Json(accounts).into_response();
                if let Some(link_header) = link_header {
                    response.headers_mut().insert(header::LINK, link_header);
                }
                response
            }
            Err(error) => server_error(error),
        },
        Err(error) => server_error(error),
    }
}

/// Convert local account records into Mastodon account responses.
async fn account_responses(
    state: &AppState,
    accounts: Vec<roosty_db::FollowCollectionAccount>,
) -> roosty_core::Result<Vec<CollectionAccountResponse>> {
    let mut responses = Vec::with_capacity(accounts.len());
    for account in accounts {
        responses.push(match account {
            roosty_db::FollowCollectionAccount::Local(account) => {
                CollectionAccountResponse::Local(Box::new(account_response(state, account).await?))
            }
            roosty_db::FollowCollectionAccount::Remote(actor) => CollectionAccountResponse::Remote(
                Box::new(remote_account_response(state, actor).await?),
            ),
        });
    }

    Ok(responses)
}

async fn relationship_response(
    state: &AppState,
    source_id: AccountId,
    target_id: AccountId,
) -> Response {
    match relationship_model(state, source_id, target_id).await {
        Ok(relationship) => Json(relationship).into_response(),
        Err(error) => server_error(error),
    }
}

/// Build the local Mastodon relationship shape for two accounts.
async fn relationship_model(
    state: &AppState,
    source_id: AccountId,
    target_id: AccountId,
) -> roosty_core::Result<RelationshipResponse> {
    let following = roosty_db::local_follow_relationship(&state.db, source_id, target_id).await?;
    let remote_following =
        roosty_db::find_remote_following(&state.db, source_id, target_id).await?;
    let followed_by = roosty_db::local_follow_relationship(&state.db, target_id, source_id).await?;
    let remote_followed_by =
        roosty_db::remote_actor_follows_local_account(&state.db, target_id, source_id).await?;
    let blocking = roosty_db::local_account_blocks(&state.db, source_id, target_id).await?;
    let blocked_by = roosty_db::local_account_blocks(&state.db, target_id, source_id).await?;
    let mute = roosty_db::active_local_account_mute(&state.db, source_id, target_id).await?;

    Ok(RelationshipResponse {
        id: target_id.0.to_string(),
        following: following.is_some()
            || remote_following
                .as_ref()
                .is_some_and(|follow| follow.state == "accepted"),
        showing_reblogs: following.as_ref().is_some_and(|follow| follow.show_reblogs),
        notifying: following.as_ref().is_some_and(|follow| follow.notify),
        followed_by: followed_by.is_some() || remote_followed_by,
        blocking,
        blocked_by,
        muting: mute.is_some(),
        muting_notifications: mute.as_ref().is_some_and(|mute| mute.notifications),
        muting_expires_at: mute
            .and_then(|mute| mute.expires_at)
            .map(crate::statuses::format_timestamp),
        requested: remote_following
            .as_ref()
            .is_some_and(|follow| follow.state == "pending"),
        domain_blocking: false,
        endorsed: false,
    })
}

/// Parse optional follow settings from JSON, form, or empty request bodies.
async fn follow_input(request: Request) -> Result<FollowInput, String> {
    parse_account_action_input(request).await
}

/// Parse optional mute settings from JSON, form, or empty request bodies.
async fn mute_input(request: Request) -> Result<MuteInput, String> {
    parse_account_action_input(request).await
}

/// Parse a small Mastodon account action payload from JSON or URL-encoded form data.
async fn parse_account_action_input<T>(request: Request) -> Result<T, String>
where
    T: Default + DeserializeOwned,
{
    let content_type = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let body = to_bytes(request.into_body(), 1024 * 1024)
        .await
        .map_err(|error| format!("invalid request body: {error}"))?;
    if body.is_empty() {
        return Ok(T::default());
    }

    if content_type.contains("application/json") {
        serde_json::from_slice(&body).map_err(|error| format!("invalid request body: {error}"))
    } else {
        serde_urlencoded::from_bytes(&body)
            .map_err(|error| format!("invalid request body: {error}"))
    }
}

/// Parse Mastodon status cursor parameters from an account statuses request.
fn timeline_cursor(params: &AccountStatusesParams) -> Result<roosty_db::TimelineCursor, ()> {
    Ok(roosty_db::TimelineCursor {
        max_id: parse_optional_status_id(params.max_id.as_deref())?,
        since_id: parse_optional_status_id(params.since_id.as_deref())?,
        min_id: parse_optional_status_id(params.min_id.as_deref())?,
    })
}

/// Parse Mastodon cursor parameters from an account collection request.
fn collection_cursor(params: &AccountCollectionParams) -> Result<roosty_db::CollectionCursor, ()> {
    Ok(roosty_db::CollectionCursor {
        max_id: parse_optional_uuid(params.max_id.as_deref())?,
        since_id: parse_optional_uuid(params.since_id.as_deref())?,
        min_id: parse_optional_uuid(params.min_id.as_deref())?,
    })
}

/// Parse an optional status UUID from Mastodon cursor query parameters.
fn parse_optional_status_id(value: Option<&str>) -> Result<Option<StatusId>, ()> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.parse().map(StatusId).map_err(|_| ()))
        .transpose()
}

/// Parse an optional UUID cursor from Mastodon collection query parameters.
fn parse_optional_uuid(value: Option<&str>) -> Result<Option<Uuid>, ()> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.parse().map_err(|_| ()))
        .transpose()
}

/// Parse repeated relationship id query parameters.
fn relationship_ids(query: Option<&str>) -> Result<Vec<Uuid>, ()> {
    let Some(query) = query else {
        return Ok(Vec::new());
    };

    serde_qs::Config::new()
        .array_format(serde_qs::ArrayFormat::EmptyIndexed)
        .use_form_encoding(true)
        .deserialize_str::<RelationshipsParams>(query)
        .map(|params| params.id)
        .map_err(|_| ())
}

/// Normalize a local account lookup query and reject remote addresses.
fn local_lookup_username(state: &AppState, acct: Option<&str>) -> Option<String> {
    let trimmed = acct?.trim().trim_start_matches('@');
    if trimmed.is_empty() {
        return None;
    }

    if let Some((username, domain)) = trimmed.split_once('@') {
        let host = state.config.public_base_url.host_str()?;
        let authority = state.config.public_base_url.authority();
        if domain != host && domain != authority {
            return None;
        }
        return non_empty(username);
    }

    non_empty(trimmed)
}

fn non_empty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

/// Return a Mastodon-style bad request response with a compact error string.
fn bad_request(description: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ErrorResponse {
            error: description.to_owned(),
        }),
    )
        .into_response()
}

/// Return a Mastodon-style forbidden response.
fn forbidden(description: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(ErrorResponse {
            error: description.to_owned(),
        }),
    )
        .into_response()
}

/// Return a Mastodon-style not found response.
fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorResponse {
            error: "Record not found".to_owned(),
        }),
    )
        .into_response()
}

/// Return a Mastodon-style internal error response.
fn server_error(error: RoostyError) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse {
            error: error.to_string(),
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use std::{
        net::{IpAddr, Ipv4Addr, SocketAddr},
        time::{SystemTime, UNIX_EPOCH},
    };

    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{Request, StatusCode, header},
    };
    use postgresql_embedded::PostgreSQL;
    use roosty_core::AccountId;
    use roosty_migration::Migrator;
    use sea_orm_migration::MigratorTrait;
    use serde_json::{Value, json};
    use tempfile::TempDir;
    use test_context::{AsyncTestContext, test_context};
    use tokio::time::{Duration, timeout};
    use tower::ServiceExt;

    use super::{remote_account_response_from_media, remote_custom_emojis};
    use crate::{config::Config, http::AppState, password};

    #[test]
    /// Prefers a remote actor's declared profile creation time over local cache metadata.
    fn remote_account_response_uses_profile_creation_time() {
        let profile_created_at = time::OffsetDateTime::UNIX_EPOCH + time::Duration::days(10);
        let first_seen_at = time::OffsetDateTime::UNIX_EPOCH + time::Duration::days(20);
        let actor = roosty_db::RemoteActor {
            id: AccountId(uuid::Uuid::now_v7()),
            activitypub_id: "https://remote.test/users/alice".to_owned(),
            username: "alice".to_owned(),
            domain: "remote.test".to_owned(),
            display_name: "Alice".to_owned(),
            summary: String::new(),
            emojis: json!([]),
            inbox_url: "https://remote.test/users/alice/inbox".to_owned(),
            shared_inbox_url: None,
            followers_url: None,
            public_key_id: "https://remote.test/users/alice#main-key".to_owned(),
            public_key_pem: "test-public-key".to_owned(),
            expires_at: time::OffsetDateTime::UNIX_EPOCH + time::Duration::days(30),
            profile_created_at: Some(profile_created_at),
            first_seen_at,
            deleted_at: None,
            moved_to_remote_actor_id: None,
        };

        let response = serde_json::to_value(remote_account_response_from_media(
            actor,
            String::new(),
            String::new(),
        ))
        .unwrap();

        assert_eq!(
            response["created_at"],
            crate::statuses::format_timestamp(profile_created_at)
        );
    }

    #[test]
    /// ActivityPub Emoji tags become Mastodon custom emoji metadata for remote projections.
    fn projects_remote_activitypub_emoji_tags() {
        let emojis = remote_custom_emojis(&json!({
            "tag": [{
                "type": "Emoji",
                "name": ":wave:",
                "icon": {"url": "https://remote.example/emoji/wave.png"}
            }]
        }));
        assert_eq!(emojis[0]["shortcode"], "wave");
        assert_eq!(emojis[0]["visible_in_picker"], false);
    }

    #[test]
    /// Falls back to first-seen time rather than the cache expiry for actors without `published`.
    fn remote_account_response_falls_back_to_first_seen_time() {
        let first_seen_at = time::OffsetDateTime::UNIX_EPOCH + time::Duration::days(20);
        let actor = roosty_db::RemoteActor {
            id: AccountId(uuid::Uuid::now_v7()),
            activitypub_id: "https://remote.test/users/alice".to_owned(),
            username: "alice".to_owned(),
            domain: "remote.test".to_owned(),
            display_name: "Alice".to_owned(),
            summary: String::new(),
            emojis: json!([]),
            inbox_url: "https://remote.test/users/alice/inbox".to_owned(),
            shared_inbox_url: None,
            followers_url: None,
            public_key_id: "https://remote.test/users/alice#main-key".to_owned(),
            public_key_pem: "test-public-key".to_owned(),
            expires_at: time::OffsetDateTime::UNIX_EPOCH + time::Duration::days(30),
            profile_created_at: None,
            first_seen_at,
            deleted_at: None,
            moved_to_remote_actor_id: None,
        };

        let response = serde_json::to_value(remote_account_response_from_media(
            actor,
            String::new(),
            String::new(),
        ))
        .unwrap();

        assert_eq!(
            response["created_at"],
            crate::statuses::format_timestamp(first_seen_at)
        );
    }

    #[test_context(AccountContext)]
    #[tokio::test]
    /// Verifies account pages expose profile data and public status collections.
    async fn account_lookup_and_statuses_return_local_profile(context: &mut AccountContext) {
        let (alice_id, alice_token) = context.create_account("alice", "alice@example.com").await;
        context
            .create_status(&alice_token, "public", Some("public"))
            .await;
        context
            .create_status(&alice_token, "private", Some("private"))
            .await;

        let account = context
            .get(&format!("/api/v1/accounts/{}", alice_id.0))
            .await;
        let statuses = context
            .get(&format!("/api/v1/accounts/{}/statuses", alice_id.0))
            .await;

        assert_eq!(account.status(), StatusCode::OK);
        assert_eq!(json_body(account).await["username"], "alice");
        let statuses = json_body(statuses).await;
        assert_eq!(statuses.as_array().unwrap().len(), 1);
        assert_eq!(statuses[0]["content"], "<p>public</p>");
    }

    #[test_context(AccountContext)]
    #[tokio::test]
    /// Verifies account lookup is routed before dynamic UUID account routes.
    async fn account_lookup_resolves_local_username(context: &mut AccountContext) {
        let (alice_id, _alice_token) = context.create_account("alice", "alice@example.com").await;

        let lookup = context.get("/api/v1/accounts/lookup?acct=alice").await;
        let local_address = context
            .get("/api/v1/accounts/lookup?acct=alice@localhost")
            .await;
        let remote_address = context
            .get("/api/v1/accounts/lookup?acct=alice@example.org")
            .await;

        assert_eq!(lookup.status(), StatusCode::OK);
        assert_eq!(json_body(lookup).await["id"], alice_id.0.to_string());
        assert_eq!(local_address.status(), StatusCode::OK);
        assert_eq!(remote_address.status(), StatusCode::NOT_FOUND);
    }

    #[test_context(AccountContext)]
    #[tokio::test]
    /// Verifies local follows update relationships, counts, and the home timeline.
    async fn follow_unfollow_updates_relationships_and_home_timeline(context: &mut AccountContext) {
        let (alice_id, alice_token) = context.create_account("alice", "alice@example.com").await;
        let (bob_id, bob_token) = context.create_account("bob", "bob@example.com").await;
        let bob_status = context
            .create_status(&bob_token, "bob public", Some("public"))
            .await;

        let follow = context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/accounts/{}/follow", bob_id.0),
                &alice_token,
            )
            .await;
        assert_eq!(follow.status(), StatusCode::OK);
        let follow = json_body(follow).await;
        assert_eq!(follow["id"], bob_id.0.to_string());
        assert_eq!(follow["following"], true);

        let relationships = context
            .authenticated_get(
                &format!("/api/v1/accounts/relationships?id%5B%5D={}", bob_id.0),
                &alice_token,
            )
            .await;
        assert_eq!(relationships.status(), StatusCode::OK);
        assert_eq!(json_body(relationships).await[0]["following"], true);

        let bob_account =
            json_body(context.get(&format!("/api/v1/accounts/{}", bob_id.0)).await).await;
        let alice_account = json_body(
            context
                .get(&format!("/api/v1/accounts/{}", alice_id.0))
                .await,
        )
        .await;
        assert_eq!(bob_account["followers_count"], 1);
        assert_eq!(alice_account["following_count"], 1);

        let home = json_body(
            context
                .authenticated_get("/api/v1/timelines/home?limit=30", &alice_token)
                .await,
        )
        .await;
        assert_eq!(home.as_array().unwrap().len(), 1);
        assert_eq!(home[0]["id"], bob_status["id"]);

        let unfollow = context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/accounts/{}/unfollow", bob_id.0),
                &alice_token,
            )
            .await;
        assert_eq!(json_body(unfollow).await["following"], false);
        let home = json_body(
            context
                .authenticated_get("/api/v1/timelines/home?limit=30", &alice_token)
                .await,
        )
        .await;
        assert_eq!(home, serde_json::json!([]));
    }

    #[test_context(AccountContext)]
    #[tokio::test]
    /// Given a local follow, when the target has user streams open, then a status-less follow notification is emitted.
    async fn follow_emits_streaming_notification(context: &mut AccountContext) {
        let (alice_id, alice_token) = context
            .create_account("alice", "alice-stream@example.com")
            .await;
        let (bob_id, _bob_token) = context
            .create_account("bob", "bob-stream@example.com")
            .await;
        let mut receiver = context.state.streaming_events.subscribe();

        let follow = context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/accounts/{}/follow", bob_id.0),
                &alice_token,
            )
            .await;
        assert_eq!(follow.status(), StatusCode::OK);

        let event = timeout(Duration::from_secs(1), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        let user_message = event
            .to_socket_message(bob_id, &["user".to_owned()])
            .unwrap()
            .unwrap();
        let notification_message = event
            .to_socket_message(bob_id, &["user:notification".to_owned()])
            .unwrap()
            .unwrap();
        let other_user_message = event
            .to_socket_message(alice_id, &["user:notification".to_owned()])
            .unwrap();
        let user_value: Value = serde_json::from_str(&user_message).unwrap();
        let notification_value: Value = serde_json::from_str(&notification_message).unwrap();
        let payload: Value = serde_json::from_str(user_value["payload"].as_str().unwrap()).unwrap();

        assert_eq!(other_user_message, None);
        assert_eq!(
            user_value,
            serde_json::json!({
                "stream": ["user"],
                "event": "notification",
                "payload": user_value["payload"],
            })
        );
        assert_eq!(
            notification_value,
            serde_json::json!({
                "stream": ["user:notification"],
                "event": "notification",
                "payload": user_value["payload"],
            })
        );
        assert_eq!(payload["type"], "follow");
        assert_eq!(payload["account"]["id"], alice_id.0.to_string());
        assert!(payload.get("status").is_none());
    }

    #[test_context(AccountContext)]
    #[tokio::test]
    /// Verifies follower collections expose Mastodon cursor pagination through Link headers.
    async fn followers_collection_uses_cursor_pagination(context: &mut AccountContext) {
        let (target_id, _target_token) =
            context.create_account("target", "target@example.com").await;
        let (_first_id, first_token) = context.create_account("first", "first@example.com").await;
        let (_second_id, second_token) =
            context.create_account("second", "second@example.com").await;
        let (_third_id, third_token) = context.create_account("third", "third@example.com").await;
        for token in [&first_token, &second_token, &third_token] {
            let response = context
                .authenticated_empty(
                    "POST",
                    &format!("/api/v1/accounts/{}/follow", target_id.0),
                    token,
                )
                .await;
            assert_eq!(response.status(), StatusCode::OK);
        }

        let page = context
            .get(&format!(
                "/api/v1/accounts/{}/followers?limit=2",
                target_id.0
            ))
            .await;
        assert_eq!(page.status(), StatusCode::OK);
        let next_cursor = link_cursor(&page, "next", "max_id");
        let body = json_body(page).await;
        assert_eq!(account_usernames(&body), ["third", "second"]);

        let next = context
            .get(&format!(
                "/api/v1/accounts/{}/followers?limit=2&max_id={next_cursor}",
                target_id.0
            ))
            .await;
        assert_eq!(next.status(), StatusCode::OK);
        assert!(next.headers().get(header::LINK).is_none());
        let body = json_body(next).await;
        assert_eq!(account_usernames(&body), ["first"]);
    }

    #[test_context(AccountContext)]
    #[tokio::test]
    /// Verifies following collections expose Mastodon cursor pagination through Link headers.
    async fn following_collection_uses_cursor_pagination(context: &mut AccountContext) {
        let (_first_id, first_token) = context.create_account("first", "first@example.com").await;
        let (target_one, _token_one) = context.create_account("one", "one@example.com").await;
        let (target_two, _token_two) = context.create_account("two", "two@example.com").await;
        let (target_three, _token_three) =
            context.create_account("three", "three@example.com").await;
        for target in [target_one, target_two, target_three] {
            let response = context
                .authenticated_empty(
                    "POST",
                    &format!("/api/v1/accounts/{}/follow", target.0),
                    &first_token,
                )
                .await;
            assert_eq!(response.status(), StatusCode::OK);
        }

        let first_account = context.get("/api/v1/accounts/lookup?acct=first").await;
        let first_account = json_body(first_account).await;
        let first_id = first_account["id"].as_str().unwrap();
        let page = context
            .get(&format!("/api/v1/accounts/{first_id}/following?limit=2"))
            .await;
        assert_eq!(page.status(), StatusCode::OK);
        let next_cursor = link_cursor(&page, "next", "max_id");
        let body = json_body(page).await;
        assert_eq!(account_usernames(&body), ["three", "two"]);

        let next = context
            .get(&format!(
                "/api/v1/accounts/{first_id}/following?limit=2&max_id={next_cursor}"
            ))
            .await;
        assert_eq!(next.status(), StatusCode::OK);
        assert!(next.headers().get(header::LINK).is_none());
        let body = json_body(next).await;
        assert_eq!(account_usernames(&body), ["one"]);
    }

    /// Given pending remote follows, when the owner pages follow requests, then only pending
    /// requests for that owner are returned with Mastodon cursor links.
    #[test_context(AccountContext)]
    #[tokio::test]
    async fn follow_requests_use_cursor_pagination(context: &mut AccountContext) {
        let (owner_id, owner_token) = context.create_account("owner", "owner@example.com").await;
        let (other_id, _other_token) = context.create_account("other", "other@example.com").await;
        context
            .create_remote_follow_request(owner_id, "first", "pending")
            .await;
        context
            .create_remote_follow_request(owner_id, "second", "pending")
            .await;
        context
            .create_remote_follow_request(owner_id, "third", "pending")
            .await;
        context
            .create_remote_follow_request(owner_id, "accepted", "accepted")
            .await;
        context
            .create_remote_follow_request(other_id, "other-request", "pending")
            .await;

        let page = context
            .authenticated_get("/api/v1/follow_requests?limit=2", &owner_token)
            .await;
        assert_eq!(page.status(), StatusCode::OK);
        let next_cursor = link_cursor(&page, "next", "max_id");
        let body = json_body(page).await;
        assert_eq!(account_usernames(&body), ["third", "second"]);

        let next = context
            .authenticated_get(
                &format!("/api/v1/follow_requests?limit=2&max_id={next_cursor}"),
                &owner_token,
            )
            .await;
        assert_eq!(next.status(), StatusCode::OK);
        assert!(next.headers().get(header::LINK).is_none());
        assert_eq!(account_usernames(&json_body(next).await), ["first"]);

        let invalid = context
            .authenticated_get("/api/v1/follow_requests?max_id=not-a-uuid", &owner_token)
            .await;
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    }

    #[test_context(AccountContext)]
    #[tokio::test]
    /// Verifies malformed account collection cursors are rejected.
    async fn account_collections_reject_invalid_cursors(context: &mut AccountContext) {
        let (account_id, _token) = context.create_account("alice", "alice@example.com").await;
        let response = context
            .get(&format!(
                "/api/v1/accounts/{}/followers?max_id=not-a-uuid",
                account_id.0
            ))
            .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[test_context(AccountContext)]
    #[tokio::test]
    /// Verifies local follow edge cases use Mastodon-style status codes.
    async fn follow_rejects_self_and_missing_accounts(context: &mut AccountContext) {
        let (alice_id, alice_token) = context.create_account("alice", "alice@example.com").await;

        let self_follow = context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/accounts/{}/follow", alice_id.0),
                &alice_token,
            )
            .await;
        let missing_follow = context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/accounts/{}/follow", uuid::Uuid::now_v7()),
                &alice_token,
            )
            .await;

        assert_eq!(self_follow.status(), StatusCode::BAD_REQUEST);
        assert_eq!(missing_follow.status(), StatusCode::NOT_FOUND);
    }

    #[test_context(AccountContext)]
    #[tokio::test]
    /// Given local follow relationships, when one account blocks the other, then follows are severed and discovery excludes the blocked account.
    async fn blocks_sever_follows_and_filter_personalized_results(context: &mut AccountContext) {
        let (_alice_id, alice_token) = context.create_account("alice", "alice@example.com").await;
        let (bob_id, bob_token) = context.create_account("bob", "bob@example.com").await;
        context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/accounts/{}/follow", bob_id.0),
                &alice_token,
            )
            .await;

        let block = context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/accounts/{}/block", bob_id.0),
                &alice_token,
            )
            .await;
        assert_eq!(block.status(), StatusCode::OK);
        assert_eq!(
            json_body(block).await,
            serde_json::json!({
                "id": bob_id.0.to_string(),
                "following": false,
                "showing_reblogs": false,
                "notifying": false,
                "followed_by": false,
                "blocking": true,
                "blocked_by": false,
                "muting": false,
                "muting_notifications": false,
                "muting_expires_at": null,
                "requested": false,
                "domain_blocking": false,
                "endorsed": false,
            })
        );

        let blocked = context
            .authenticated_get("/api/v1/blocks", &alice_token)
            .await;
        assert_eq!(blocked.status(), StatusCode::OK);
        assert_eq!(account_usernames(&json_body(blocked).await), ["bob"]);

        let follow = context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/accounts/{}/follow", bob_id.0),
                &alice_token,
            )
            .await;
        assert_eq!(follow.status(), StatusCode::FORBIDDEN);

        let search = context
            .authenticated_get("/api/v2/search?type=accounts&q=bob", &alice_token)
            .await;
        assert_eq!(json_body(search).await["accounts"], serde_json::json!([]));
        assert_ne!(bob_token, alice_token);
    }

    #[test_context(AccountContext)]
    #[tokio::test]
    /// Given a followed author, when the follower mutes them, then the home timeline and notifications honor the mute settings.
    async fn mutes_filter_home_timeline_and_optionally_notifications(context: &mut AccountContext) {
        let (_alice_id, alice_token) = context.create_account("alice", "alice@example.com").await;
        let (bob_id, bob_token) = context.create_account("bob", "bob@example.com").await;
        context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/accounts/{}/follow", bob_id.0),
                &alice_token,
            )
            .await;
        context
            .create_status(&bob_token, "before mute", Some("public"))
            .await;

        let mute = context
            .authenticated_json(
                "POST",
                &format!("/api/v1/accounts/{}/mute", bob_id.0),
                &alice_token,
                serde_json::json!({ "notifications": true, "duration": 0 }),
            )
            .await;
        assert_eq!(mute.status(), StatusCode::OK);
        assert_eq!(
            json_body(mute).await,
            serde_json::json!({
                "id": bob_id.0.to_string(),
                "following": true,
                "showing_reblogs": true,
                "notifying": false,
                "followed_by": false,
                "blocking": false,
                "blocked_by": false,
                "muting": true,
                "muting_notifications": true,
                "muting_expires_at": null,
                "requested": false,
                "domain_blocking": false,
                "endorsed": false,
            })
        );

        let muted = context
            .authenticated_get("/api/v1/mutes", &alice_token)
            .await;
        assert_eq!(muted.status(), StatusCode::OK);
        assert_eq!(account_usernames(&json_body(muted).await), ["bob"]);
        let home = context
            .authenticated_get("/api/v1/timelines/home?limit=30", &alice_token)
            .await;
        assert_eq!(json_body(home).await, serde_json::json!([]));

        context
            .create_status(&bob_token, "hello @alice", Some("public"))
            .await;
        let notifications = context
            .authenticated_get("/api/v1/notifications?limit=30", &alice_token)
            .await;
        assert_eq!(json_body(notifications).await, serde_json::json!([]));

        let unmute = context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/accounts/{}/unmute", bob_id.0),
                &alice_token,
            )
            .await;
        assert_eq!(json_body(unmute).await["muting"], false);

        let temporary_mute = context
            .authenticated_json(
                "POST",
                &format!("/api/v1/accounts/{}/mute", bob_id.0),
                &alice_token,
                serde_json::json!({ "notifications": false, "duration": 60 }),
            )
            .await;
        let temporary_mute = json_body(temporary_mute).await;
        assert_eq!(
            temporary_mute,
            serde_json::json!({
                "id": bob_id.0.to_string(),
                "following": true,
                "showing_reblogs": true,
                "notifying": false,
                "followed_by": false,
                "blocking": false,
                "blocked_by": false,
                "muting": true,
                "muting_notifications": false,
                "muting_expires_at": temporary_mute["muting_expires_at"].clone(),
                "requested": false,
                "domain_blocking": false,
                "endorsed": false,
            })
        );
        assert!(temporary_mute["muting_expires_at"].is_string());

        context
            .create_status(&bob_token, "notification allowed @alice", Some("public"))
            .await;
        let notifications = context
            .authenticated_get("/api/v1/notifications?limit=30", &alice_token)
            .await;
        assert_eq!(json_body(notifications).await[0]["type"], "mention");
    }

    /// Extract account usernames from a Mastodon account collection response.
    fn account_usernames(body: &Value) -> Vec<&str> {
        body.as_array()
            .unwrap()
            .iter()
            .map(|account| account["username"].as_str().unwrap())
            .collect()
    }

    /// Extract a cursor query parameter from a Mastodon Link header.
    fn link_cursor(response: &axum::http::Response<Body>, rel: &str, param: &str) -> String {
        let link = response
            .headers()
            .get(header::LINK)
            .unwrap()
            .to_str()
            .unwrap();
        let segment = link
            .split(',')
            .find(|segment| segment.contains(&format!(r#"rel="{rel}""#)))
            .unwrap();
        let start = segment.find(&format!("{param}=")).unwrap() + param.len() + 1;
        segment[start..]
            .split(['&', '>'])
            .next()
            .unwrap()
            .to_owned()
    }

    struct AccountContext {
        postgresql: PostgreSQL,
        db: roosty_db::DbConnection,
        database_name: String,
        config: Config,
        state: AppState,
        application_id: uuid::Uuid,
        _temp_dir: TempDir,
    }

    impl AsyncTestContext for AccountContext {
        async fn setup() -> Self {
            let temp_dir = tempfile::Builder::new()
                .prefix("roosty-accounts-")
                .tempdir()
                .unwrap();
            let database_name = unique_name();
            let data_dir = temp_dir.path().join("data").join(&database_name);
            let password_file = temp_dir
                .path()
                .join("passwords")
                .join(format!("{database_name}.pgpass"));

            if let Some(parent) = password_file.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }

            let settings = crate::test_postgres::settings(&data_dir, password_file);
            let mut postgresql = PostgreSQL::new(settings);

            postgresql.setup().await.unwrap();
            postgresql.start().await.unwrap();
            postgresql.create_database(&database_name).await.unwrap();

            let database_url = postgresql.settings().url(&database_name);
            let db = roosty_db::connect(&database_url).await.unwrap();
            Migrator::up(&db, None).await.unwrap();

            let (application, _secret) = roosty_db::create_oauth_application(
                &db,
                "Elk",
                "https://localhost:4001/oauth",
                "read write follow push",
                Some("https://localhost:4001"),
                "test-token-pepper-change-me-0000",
            )
            .await
            .unwrap();

            let config = Config {
                database_url,
                public_base_url: "https://localhost:4000".parse().unwrap(),
                listen_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4000),
                infra_listen_addr: None,
                session_secret: "test-session-secret-change-me-000".to_owned(),
                token_pepper: "test-token-pepper-change-me-0000".to_owned(),
                object_storage_backend: "local".to_owned(),
                media_root: "./media".to_owned(),
                registration_mode: "closed".to_owned(),
                federation_enabled: false,
                federation_key_encryption_secret: None,
                federation_allowed_domains: Vec::new(),
                federation_blocked_domains: Vec::new(),
                federation_delivery_max_age: time::Duration::days(7),
                remote_media_cache_ttl: time::Duration::days(30),
                remote_media_max_bytes: 40 * 1024 * 1024,
                remote_media_fetch_concurrency: 5,
                worker_concurrency: 4,
                streaming: crate::config::StreamingConfig::default(),
                instance_name: "Roosty Test".to_owned(),
                instance_description: Some("Endpoint test instance".to_owned()),
            };

            Self {
                postgresql,
                state: AppState::new(config.clone(), db.clone()),
                db,
                database_name,
                config,
                application_id: application.id,
                _temp_dir: temp_dir,
            }
        }

        async fn teardown(self) {
            let AccountContext {
                postgresql,
                db,
                database_name,
                state,
                ..
            } = self;
            let AppState { db: state_db, .. } = state;

            state_db.close().await.unwrap();
            db.close().await.unwrap();
            postgresql.drop_database(&database_name).await.unwrap();
            postgresql.stop().await.unwrap();
        }
    }

    impl AccountContext {
        /// Build an app router backed by this test database.
        fn app(&self) -> Router {
            crate::http::app_router(self.state.clone(), false)
        }

        /// Send a raw request through the test router.
        async fn request(&self, request: Request<Body>) -> axum::http::Response<Body> {
            self.app().oneshot(request).await.unwrap()
        }

        /// Send an anonymous GET request.
        async fn get(&self, uri: &str) -> axum::http::Response<Body> {
            self.request(
                Request::builder()
                    .method("GET")
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
        }

        /// Send an authenticated GET request.
        async fn authenticated_get(&self, uri: &str, token: &str) -> axum::http::Response<Body> {
            self.request(
                Request::builder()
                    .method("GET")
                    .uri(uri)
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
        }

        /// Send an authenticated request without a body.
        async fn authenticated_empty(
            &self,
            method: &str,
            uri: &str,
            token: &str,
        ) -> axum::http::Response<Body> {
            self.request(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
        }

        /// Send an authenticated JSON request.
        async fn authenticated_json(
            &self,
            method: &str,
            uri: &str,
            token: &str,
            body: serde_json::Value,
        ) -> axum::http::Response<Body> {
            self.request(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
        }

        /// Create a local account with an access token for endpoint tests.
        async fn create_account(&self, username: &str, email: &str) -> (AccountId, String) {
            let password_hash = password::hash_password("password").unwrap();
            let account_id = AccountId(
                roosty_db::create_local_account(&self.db, username, email, &password_hash)
                    .await
                    .unwrap(),
            );
            let token = roosty_db::create_access_token(
                &self.db,
                &self.config.token_pepper,
                account_id,
                self.application_id,
                "read write follow push",
            )
            .await
            .unwrap()
            .token;

            (account_id, token)
        }

        /// Cache a remote actor and create an inbound follow relationship for endpoint tests.
        async fn create_remote_follow_request(
            &self,
            local_account_id: AccountId,
            username: &str,
            state: &str,
        ) {
            let actor = roosty_db::RemoteActor {
                id: AccountId(uuid::Uuid::now_v7()),
                activitypub_id: format!("https://remote.test/users/{username}"),
                username: username.to_owned(),
                domain: "remote.test".to_owned(),
                display_name: username.to_owned(),
                summary: String::new(),
                emojis: json!([]),
                inbox_url: format!("https://remote.test/users/{username}/inbox"),
                shared_inbox_url: None,
                followers_url: None,
                public_key_id: format!("https://remote.test/users/{username}#main-key"),
                public_key_pem: "test-public-key".to_owned(),
                expires_at: time::OffsetDateTime::now_utc() + time::Duration::hours(1),
                profile_created_at: None,
                first_seen_at: time::OffsetDateTime::now_utc(),
                deleted_at: None,
                moved_to_remote_actor_id: None,
            };
            let actor = roosty_db::upsert_remote_actor(&self.db, &actor)
                .await
                .unwrap();
            roosty_db::upsert_remote_follow(
                &self.db,
                actor.id,
                local_account_id,
                &format!("https://remote.test/follows/{username}"),
                serde_json::json!({ "id": format!("https://remote.test/follows/{username}") }),
                state,
            )
            .await
            .unwrap();
        }

        /// Create a local status through the HTTP API and return its JSON response.
        async fn create_status(
            &self,
            token: &str,
            status: &str,
            visibility: Option<&str>,
        ) -> Value {
            let mut body = serde_json::json!({ "status": status });
            if let Some(visibility) = visibility {
                body["visibility"] = serde_json::json!(visibility);
            }

            json_body(
                self.authenticated_json("POST", "/api/v1/statuses", token, body)
                    .await,
            )
            .await
        }
    }

    /// Decode a JSON response body.
    async fn json_body(response: axum::http::Response<Body>) -> Value {
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    /// Build a unique database name for parallel embedded PostgreSQL tests.
    fn unique_name() -> String {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();

        format!("roosty_accounts_{}_{}", std::process::id(), timestamp)
    }
}
