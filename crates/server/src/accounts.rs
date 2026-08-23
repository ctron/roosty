use std::borrow::Cow;

use axum::{
    Error as AxumError, Extension, Json, Router,
    body::to_bytes,
    extract::{Path, Query, RawQuery, Request, State},
    http::{HeaderValue, header},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use roosty_core::{AccountId, AccountRelationshipError, Result as RoostyResult, StatusId};
use roosty_db::{
    AccountDirectoryOptions, AccountDirectoryOrder, AccountSearchResult,
    AccountStatusTimelineOptions, AccountSuggestionSource, CollectionCursor, CollectionPage,
    FollowCollectionAccount, LocalNotificationType, RemoteActor, RemoteFollowState,
    RemoteProfileMediaKind, TimelineCursor,
};
use sea_orm::ConnectionTrait;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Error as JsonError, Value, json};
use serde_qs::{ArrayFormat, Config as QueryStringConfig, Error as QueryStringError};
use serde_urlencoded::de::Error as FormError;
use thiserror::Error;
use time::OffsetDateTime;
use tracing::warn;
use uuid::{Error, Uuid};

use crate::{
    auth::{
        AccountResponse, AuthenticatedAccessToken, AuthenticatedAccount, OAuthScope,
        OAuthScopeResource, OptionalAuthenticatedAccount, account_response, account_response_on,
        account_response_with_stats, format_account_date,
    },
    federation::{self, discovery},
    http::{ApiError, ApiResult, AppState, DatabaseContext, TransactionContext},
    media::remote_profile_media_url,
    notifications::create_and_stream_notification,
    statuses::{
        CollectionLink, StatusRenderContext, format_timestamp, remote_timeline_response,
        timeline_limit, timeline_response,
    },
};

const DEFAULT_ACCOUNT_LIMIT: u64 = 40;
const MAX_ACCOUNT_LIMIT: u64 = 80;

/// Build routes for Mastodon-compatible account lookup and local follows.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/directory", get(directory))
        .route("/api/v1/suggestions", get(suggestions_v1))
        .route("/api/v2/suggestions", get(suggestions_v2))
        .route(
            "/api/v1/suggestions/{account_id}",
            delete(dismiss_suggestion),
        )
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
struct SuggestionPath {
    account_id: String,
}

#[derive(Deserialize)]
struct AccountStatusesParams {
    limit: Option<u64>,
    max_id: Option<String>,
    since_id: Option<String>,
    min_id: Option<String>,
    exclude_replies: Option<bool>,
    #[serde(rename = "exclude_reblogs")]
    _exclude_reblogs: Option<bool>,
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

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DirectoryOrder {
    #[default]
    Active,
    New,
}

impl DirectoryOrder {
    fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::New => "new",
        }
    }
}

impl From<DirectoryOrder> for AccountDirectoryOrder {
    fn from(order: DirectoryOrder) -> Self {
        match order {
            DirectoryOrder::Active => Self::Active,
            DirectoryOrder::New => Self::New,
        }
    }
}

#[derive(Deserialize)]
struct DirectoryParams {
    offset: Option<u64>,
    limit: Option<u64>,
    order: Option<DirectoryOrder>,
    local: Option<bool>,
}

#[derive(Deserialize)]
struct SuggestionParams {
    limit: Option<u64>,
    offset: Option<u64>,
}

#[derive(Serialize)]
pub(crate) struct RemoteAccountResponse {
    id: String,
    username: String,
    acct: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    invalid_handle: Option<bool>,
    display_name: String,
    locked: bool,
    bot: bool,
    discoverable: Option<bool>,
    limited: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    suspended: Option<bool>,
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

/// Mastodon account projection returned by the mixed profile directory.
#[derive(Serialize)]
#[serde(untagged)]
enum DirectoryAccountResponse {
    Local(Box<AccountResponse>),
    Remote(Box<RemoteAccountResponse>),
}

#[derive(Serialize)]
struct SuggestionResponse {
    source: &'static str,
    sources: Vec<&'static str>,
    account: DirectoryAccountResponse,
}

struct RenderedSuggestion {
    account: DirectoryAccountResponse,
    sources: Vec<AccountSuggestionSource>,
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

#[derive(Debug, Error)]
enum AccountInputError {
    #[error("invalid request body: {0}")]
    Body(#[from] AxumError),
    #[error("invalid request body: {0}")]
    Json(#[from] JsonError),
    #[error("invalid request body: {0}")]
    Form(#[from] FormError),
}

impl From<AccountInputError> for ApiError {
    fn from(error: AccountInputError) -> Self {
        Self::BadRequest(Cow::Owned(error.to_string()))
    }
}

#[derive(Debug, Error)]
enum AccountQueryError {
    #[error("invalid account identifier")]
    Identifier(#[from] Error),
    #[error("invalid account query")]
    Query(#[from] QueryStringError),
}

impl From<AccountQueryError> for ApiError {
    fn from(_: AccountQueryError) -> Self {
        Self::BadRequest(Cow::Borrowed("account id or cursor is invalid"))
    }
}

/// Return the public Mastodon-compatible profile directory.
async fn directory(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    OptionalAuthenticatedAccount(viewer): OptionalAuthenticatedAccount,
    Query(params): Query<DirectoryParams>,
) -> ApiResult<Response> {
    let limit = params
        .limit
        .unwrap_or(DEFAULT_ACCOUNT_LIMIT)
        .clamp(1, MAX_ACCOUNT_LIMIT);
    let offset = params.offset.unwrap_or_default();
    let order = params.order.unwrap_or_default();
    let local_only = params.local.unwrap_or(false);
    let txn = database.begin_snapshot().await?;
    let hidden_domains = roosty_db::hidden_federation_domains(&txn).await?;
    let page = roosty_db::account_directory(
        &txn,
        AccountDirectoryOptions {
            viewer_account_id: viewer.as_ref().map(|account| account.id),
            order: order.into(),
            local_only,
            limit,
            offset,
            blocked_remote_domains: &hidden_domains,
        },
    )
    .await?;
    let remote_ids = page
        .items
        .iter()
        .filter_map(|item| match &item.account {
            AccountSearchResult::Remote(actor) => Some(actor.id),
            AccountSearchResult::Local(_) => None,
        })
        .collect::<Vec<_>>();
    let remote_media = roosty_db::remote_profile_media_for_actors(&txn, &remote_ids).await?;
    let mut accounts = Vec::with_capacity(page.items.len());
    for item in page.items {
        match item.account {
            AccountSearchResult::Local(account) => {
                accounts.push(DirectoryAccountResponse::Local(Box::new(
                    account_response_with_stats(
                        &state,
                        account,
                        item.followers_count,
                        item.following_count,
                        item.statuses_count,
                        item.last_status_at,
                    ),
                )));
            }
            AccountSearchResult::Remote(actor) => {
                let media_url = |kind| {
                    remote_media
                        .iter()
                        .find(|media| media.remote_actor_id == actor.id && media.kind == kind)
                        .map(|media| remote_profile_media_url(&state, media.id))
                        .unwrap_or_default()
                };
                let avatar = media_url(RemoteProfileMediaKind::Avatar);
                let header = media_url(RemoteProfileMediaKind::Header);
                let mut response = remote_account_response_from_media(actor, avatar, header);
                response.followers_count = item.followers_count;
                response.following_count = item.following_count;
                response.statuses_count = item.statuses_count;
                response.last_status_at = item.last_status_at.map(format_account_date);
                accounts.push(DirectoryAccountResponse::Remote(Box::new(response)));
            }
        }
    }
    txn.commit().await?;

    let mut links = Vec::with_capacity(2);
    if page.has_more {
        links.push(directory_link(
            offset.saturating_add(limit),
            limit,
            order,
            local_only,
            "next",
        ));
    }
    if offset > 0 {
        links.push(directory_link(
            offset.saturating_sub(limit),
            limit,
            order,
            local_only,
            "prev",
        ));
    }
    let mut response = Json(accounts).into_response();
    if !links.is_empty() {
        let value = HeaderValue::from_str(&links.join(", "))?;
        response.headers_mut().insert(header::LINK, value);
    }
    Ok(response)
}

fn has_account_read_scope(scopes: &str) -> bool {
    scopes
        .split_ascii_whitespace()
        .map(OAuthScope::parse)
        .any(|scope| {
            matches!(
                scope,
                OAuthScope::Read(OAuthScopeResource::All | OAuthScopeResource::Accounts)
            )
        })
}

fn require_account_read_scope(token: &AuthenticatedAccessToken) -> ApiResult<()> {
    if has_account_read_scope(&token.grant.scopes) {
        Ok(())
    } else {
        Err(ApiError::Forbidden(
            "This action is outside the authorized scopes".into(),
        ))
    }
}

fn require_account_write_scope(token: &AuthenticatedAccessToken) -> ApiResult<()> {
    let allowed = token
        .grant
        .scopes
        .split_ascii_whitespace()
        .map(OAuthScope::parse)
        .any(|scope| {
            matches!(
                scope,
                OAuthScope::Write(OAuthScopeResource::All | OAuthScopeResource::Accounts)
            )
        });
    if allowed {
        Ok(())
    } else {
        Err(ApiError::Forbidden(
            "This action is outside the authorized scopes".into(),
        ))
    }
}

/// Return Mastodon v2 follow suggestions with modern and legacy source metadata.
async fn suggestions_v2(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    token: AuthenticatedAccessToken,
    Query(params): Query<SuggestionParams>,
) -> ApiResult<Response> {
    require_account_read_scope(&token)?;
    let accounts = suggestion_account_responses(
        &state,
        &database,
        token.grant.account.id,
        params.limit,
        params.offset,
    )
    .await?;
    Ok(Json(
        accounts
            .into_iter()
            .map(|suggestion| SuggestionResponse {
                source: match suggestion.sources.first() {
                    Some(AccountSuggestionSource::FriendsOfFriends) => "past_interactions",
                    _ => "global",
                },
                sources: suggestion
                    .sources
                    .iter()
                    .map(|source| source.as_str())
                    .collect(),
                account: suggestion.account,
            })
            .collect::<Vec<_>>(),
    )
    .into_response())
}

/// Return the deprecated Mastodon v1 plain-account suggestion projection.
async fn suggestions_v1(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    token: AuthenticatedAccessToken,
    Query(params): Query<SuggestionParams>,
) -> ApiResult<Response> {
    require_account_read_scope(&token)?;
    Ok(Json(
        suggestion_account_responses(
            &state,
            &database,
            token.grant.account.id,
            params.limit,
            params.offset,
        )
        .await?
        .into_iter()
        .map(|suggestion| suggestion.account)
        .collect::<Vec<_>>(),
    )
    .into_response())
}

async fn suggestion_account_responses(
    state: &AppState,
    database: &DatabaseContext,
    viewer_account_id: AccountId,
    limit: Option<u64>,
    offset: Option<u64>,
) -> ApiResult<Vec<RenderedSuggestion>> {
    let limit = limit
        .unwrap_or(DEFAULT_ACCOUNT_LIMIT)
        .clamp(1, MAX_ACCOUNT_LIMIT);
    let txn = database.begin_snapshot().await?;
    let hidden_domains = roosty_db::hidden_federation_domains(&txn).await?;
    let suggestions = roosty_db::account_suggestions(
        &txn,
        roosty_db::AccountSuggestionOptions {
            viewer_account_id,
            limit,
            offset: offset.unwrap_or_default(),
            blocked_remote_domains: &hidden_domains,
        },
    )
    .await?;
    let remote_ids = suggestions
        .iter()
        .filter_map(|suggestion| match &suggestion.account {
            AccountSearchResult::Remote(actor) => Some(actor.id),
            AccountSearchResult::Local(_) => None,
        })
        .collect::<Vec<_>>();
    let remote_media = roosty_db::remote_profile_media_for_actors(&txn, &remote_ids).await?;
    let mut responses = Vec::with_capacity(suggestions.len());
    for suggestion in suggestions {
        let account = match suggestion.account {
            AccountSearchResult::Local(account) => {
                DirectoryAccountResponse::Local(Box::new(account_response_with_stats(
                    state,
                    account,
                    suggestion.followers_count,
                    suggestion.following_count,
                    suggestion.statuses_count,
                    suggestion.last_status_at,
                )))
            }
            AccountSearchResult::Remote(actor) => {
                let media_url = |kind| {
                    remote_media
                        .iter()
                        .find(|media| media.remote_actor_id == actor.id && media.kind == kind)
                        .map(|media| remote_profile_media_url(state, media.id))
                        .unwrap_or_default()
                };
                let avatar = media_url(RemoteProfileMediaKind::Avatar);
                let header = media_url(RemoteProfileMediaKind::Header);
                let mut response = remote_account_response_from_media(actor, avatar, header);
                response.followers_count = suggestion.followers_count;
                response.following_count = suggestion.following_count;
                response.statuses_count = suggestion.statuses_count;
                response.last_status_at = suggestion.last_status_at.map(format_account_date);
                DirectoryAccountResponse::Remote(Box::new(response))
            }
        };
        responses.push(RenderedSuggestion {
            account,
            sources: suggestion.sources,
        });
    }
    txn.commit().await?;
    Ok(responses)
}

/// Idempotently remove an account from the authenticated viewer's suggestions.
async fn dismiss_suggestion(
    Extension(database): Extension<DatabaseContext>,
    token: AuthenticatedAccessToken,
    Path(path): Path<SuggestionPath>,
) -> ApiResult<Response> {
    require_account_write_scope(&token)?;
    if let Ok(target_id) = Uuid::parse_str(&path.account_id) {
        let txn = database.begin_write().await?;
        roosty_db::dismiss_account_suggestion(&txn, token.grant.account.id, AccountId(target_id))
            .await?;
        txn.commit().await?;
    }
    Ok(Json(json!({})).into_response())
}

fn directory_link(
    offset: u64,
    limit: u64,
    order: DirectoryOrder,
    local_only: bool,
    relation: &str,
) -> String {
    format!(
        "</api/v1/directory?offset={offset}&limit={limit}&order={}&local={local_only}>; rel=\"{relation}\"",
        order.as_str()
    )
}

/// Return a public local account profile by local username or address.
async fn lookup_account(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    Query(params): Query<LookupParams>,
) -> ApiResult<Response> {
    let txn = database.begin_snapshot().await?;
    let context = TransactionContext::new(&state, &txn);
    if let Some(username) = local_lookup_username(&state, params.acct.as_deref()) {
        let account = roosty_db::find_local_account_by_username(&txn, &username)
            .await?
            .ok_or_else(|| ApiError::NotFound("Record not found".into()))?;
        let response = account_response(&state, &txn, account).await?;
        return Ok(Json(response).into_response());
    }

    let mut cached_actor = None;
    if let Some((username, domain)) = params
        .acct
        .as_deref()
        .and_then(discovery::exact_remote_handle)
    {
        cached_actor = roosty_db::find_remote_actor_by_handle(&txn, &username, &domain).await?;
    }
    if let Some(actor) = cached_actor {
        let unavailable = remote_actor_is_suspended_on(&txn, &actor).await?;
        let stale =
            params.resolve.unwrap_or(false) && actor.expires_at <= OffsetDateTime::now_utc();
        if !unavailable && !stale {
            let response = remote_account_response(&context, actor).await?;
            return Ok(Json(response).into_response());
        }
    }

    if !params.resolve.unwrap_or(false) || !state.config.federation_enabled {
        return Err(ApiError::NotFound("Record not found".into()));
    }
    let acct = params
        .acct
        .as_deref()
        .ok_or_else(|| ApiError::NotFound("Record not found".into()))?;
    txn.commit().await?;
    let actor = discovery::resolve_remote_actor(&state, &database, acct).await?;
    if actor.deleted_at.is_some() {
        return Err(ApiError::NotFound("Record not found".into()));
    }
    let txn = database.begin_snapshot().await?;
    let context = TransactionContext::new(&state, &txn);
    let response = remote_account_response(&context, actor).await?;
    txn.commit().await?;
    Ok(Json(response).into_response())
}

/// Convert a cached remote actor to the public Mastodon account projection.
pub(crate) async fn remote_account_response(
    state: &TransactionContext<'_, impl ConnectionTrait>,
    actor: RemoteActor,
) -> RoostyResult<RemoteAccountResponse> {
    remote_account_response_on(state, state.db, actor).await
}

/// Build a remote account response within a containing database snapshot.
pub(crate) async fn remote_account_response_on(
    state: &AppState,
    db: &impl ConnectionTrait,
    actor: RemoteActor,
) -> RoostyResult<RemoteAccountResponse> {
    let statuses_count = roosty_db::count_remote_statuses_by_account(db, actor.id).await?;
    let last_status_at = roosty_db::last_remote_status_at(db, actor.id)
        .await?
        .map(format_account_date);
    let followers_count =
        roosty_db::count_remote_actor_followers_known_locally(db, actor.id).await?;
    let following_count =
        roosty_db::count_remote_actor_following_known_locally(db, actor.id).await?;
    let profile_media = roosty_db::remote_profile_media_for_actor(db, actor.id).await?;
    let media_url = |kind| {
        profile_media
            .iter()
            .find(|media| media.kind == kind)
            .map(|media| remote_profile_media_url(state, media.id))
            .unwrap_or_default()
    };
    let avatar = media_url(RemoteProfileMediaKind::Avatar);
    let header = media_url(RemoteProfileMediaKind::Header);
    let moved_to_remote_actor_id = actor.moved_to_remote_actor_id;
    let mut response = remote_account_response_from_media(actor, avatar, header);
    response.followers_count = followers_count;
    response.following_count = following_count;
    response.statuses_count = statuses_count;
    response.last_status_at = last_status_at;
    if let Some(moved_to_remote_actor_id) = moved_to_remote_actor_id
        && let Some(mut moved) =
            roosty_db::find_remote_actor_by_id(db, moved_to_remote_actor_id).await?
    {
        // Mastodon exposes one replacement account; suppress nested moves to avoid cycles.
        moved.moved_to_remote_actor_id = None;
        response.moved = Some(Box::new(
            Box::pin(remote_account_response_on(state, db, moved)).await?,
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
        invalid_handle: None,
        display_name: String::new(),
        locked: false,
        bot: false,
        discoverable: None,
        limited: false,
        suspended: None,
        group: false,
        created_at: format_timestamp(OffsetDateTime::now_utc()),
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
    let suspended = actor.suspended_at.is_some();
    RemoteAccountResponse {
        id: actor.id.0.to_string(),
        username: actor.username.clone(),
        acct: if actor.invalid_handle {
            format!("invalid-{}@invalid.invalid", actor.id.0)
        } else {
            format!("{}@{}", actor.username, actor.domain)
        },
        invalid_handle: actor.invalid_handle.then_some(true),
        display_name: if suspended {
            String::new()
        } else {
            actor.display_name
        },
        locked: false,
        bot: false,
        discoverable: actor.discoverable,
        limited: actor.limited_at.is_some(),
        suspended: suspended.then_some(true),
        group: false,
        created_at: format_timestamp(actor.profile_created_at.unwrap_or(actor.first_seen_at)),
        note: if suspended {
            String::new()
        } else {
            actor.summary
        },
        url: actor.activitypub_id,
        avatar: if suspended {
            String::new()
        } else {
            avatar.clone()
        },
        avatar_static: if suspended { String::new() } else { avatar },
        header: if suspended {
            String::new()
        } else {
            header.clone()
        },
        header_static: if suspended { String::new() } else { header },
        fields: Vec::new(),
        emojis: if suspended {
            Vec::new()
        } else {
            remote_custom_emojis(&actor.emojis)
        },
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
            let kind = serde_json::from_value::<RemoteEmojiType>(tag.get("type")?.clone()).ok()?;
            if kind == RemoteEmojiType::Other {
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

#[derive(Deserialize, Eq, PartialEq)]
enum RemoteEmojiType {
    Emoji,
    #[serde(rename = "http://joinmastodon.org/ns#Emoji")]
    MastodonEmoji,
    #[serde(other)]
    Other,
}

/// Return a public local account profile by account id.
async fn show_account(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    Path(path): Path<AccountPath>,
) -> ApiResult<Response> {
    let account_id = AccountId(path.account_id);
    let txn = database.begin_snapshot().await?;
    let response =
        if let Some(account) = roosty_db::find_local_account_by_id(&txn, account_id).await? {
            Json(account_response_on(&state, &txn, account).await?).into_response()
        } else {
            let actor = roosty_db::find_remote_actor_by_id(&txn, account_id)
                .await?
                .ok_or_else(|| ApiError::NotFound("Record not found".into()))?;
            if actor.deleted_at.is_some()
                || roosty_db::federation_domain_policy(&txn, &actor.domain)
                    .await?
                    .is_suspended()
            {
                return Err(ApiError::NotFound("Record not found".into()));
            }
            Json(remote_account_response_on(&state, &txn, actor).await?).into_response()
        };
    txn.commit().await?;
    Ok(response)
}

/// Return statuses authored by one local account.
async fn account_statuses(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    OptionalAuthenticatedAccount(viewer): OptionalAuthenticatedAccount,
    Path(path): Path<AccountPath>,
    Query(params): Query<AccountStatusesParams>,
) -> ApiResult<Response> {
    let txn = database.begin_snapshot().await?;
    let context = StatusRenderContext::new(&state, &txn);
    let account_id = AccountId(path.account_id);
    let cursor = timeline_cursor(&params)?;
    let limit = timeline_limit(params.limit);
    let local_account = roosty_db::find_local_account_by_id(&txn, account_id).await?;
    if local_account
        .as_ref()
        .is_some_and(|account| account.suspended_at.is_some())
    {
        return Ok(Json(Vec::<Value>::new()).into_response());
    }
    let local = local_account.is_some();
    if !local {
        let actor = roosty_db::find_remote_actor_by_id(&txn, account_id)
            .await?
            .ok_or_else(|| ApiError::NotFound("Record not found".into()))?;
        if remote_actor_is_suspended_on(&txn, &actor).await? {
            return Err(ApiError::NotFound("Record not found".into()));
        }
        let page = if params.pinned.unwrap_or(false) {
            roosty_db::pinned_remote_statuses_by_account(&txn, account_id, limit, cursor).await?
        } else {
            roosty_db::remote_statuses_by_account(
                &txn,
                account_id,
                viewer.as_ref().map(|account| account.id),
                limit,
                cursor,
                AccountStatusTimelineOptions {
                    exclude_replies: params.exclude_replies.unwrap_or(false),
                    only_media: params.only_media.unwrap_or(false),
                    tagged: params.tagged.clone().filter(|tag| !tag.trim().is_empty()),
                },
            )
            .await?
        };
        let suffix = if params.pinned.unwrap_or(false) {
            "?pinned=true"
        } else {
            ""
        };
        return Ok(remote_timeline_response(
            &context,
            page,
            limit,
            &format!("/api/v1/accounts/{}/statuses{suffix}", account_id.0),
            viewer.as_ref().map(|account| account.id),
        )
        .await);
    };
    let page = if params.pinned.unwrap_or(false) {
        roosty_db::pinned_local_statuses_by_account(&txn, account_id, limit, cursor).await?
    } else {
        roosty_db::local_statuses_by_account(
            &txn,
            account_id,
            viewer.as_ref().map(|account| account.id),
            limit,
            cursor,
            AccountStatusTimelineOptions {
                exclude_replies: params.exclude_replies.unwrap_or(false),
                only_media: params.only_media.unwrap_or(false),
                tagged: params.tagged.clone().filter(|tag| !tag.trim().is_empty()),
            },
        )
        .await?
    };
    let suffix = if params.pinned.unwrap_or(false) {
        "?pinned=true"
    } else {
        ""
    };
    Ok(timeline_response(
        &context,
        page,
        limit,
        &format!("/api/v1/accounts/{}/statuses{suffix}", account_id.0),
        viewer.as_ref().map(|account| account.id),
    )
    .await)
}

/// Follow a local account and return the resulting relationship.
async fn follow(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<AccountPath>,
    request: Request,
) -> ApiResult<Response> {
    let input = follow_input(request).await?;
    let target_id = AccountId(path.account_id);
    let txn = database.begin_write().await?;
    let remote_target = roosty_db::find_remote_actor_by_id(&txn, target_id).await?;
    let remote_allowed = if let Some(actor) = remote_target.as_ref() {
        state.config.federation_domain_is_allowed(&actor.domain)
            && !remote_actor_is_suspended_on(&txn, actor).await?
    } else {
        false
    };
    if remote_allowed {
        if roosty_db::local_remote_accounts_are_blocked(&txn, account.id, target_id).await? {
            return Err(AccountRelationshipError::FollowBlocked.into());
        }
        let (activity_id, job) =
            federation::prepare_remote_follow(&state, &txn, account.id, target_id).await?;
        roosty_db::create_remote_following_with_job(
            &txn,
            account.id,
            target_id,
            &activity_id,
            input.reblogs.unwrap_or(true),
            input.notify.unwrap_or(false),
            job,
        )
        .await?;
        txn.commit().await?;
        return relationship_response(&state, &database, account.id, target_id).await;
    }

    roosty_db::follow_local_account(
        &txn,
        account.id,
        target_id,
        input.reblogs.unwrap_or(true),
        input.notify.unwrap_or(false),
    )
    .await?;
    txn.commit().await?;
    create_and_stream_notification(
        &state,
        &database,
        target_id,
        LocalNotificationType::Follow,
        account.id,
        None,
    )
    .await
    .inspect_err(|error| warn!(%error, "failed to create follow notification"))
    .ok();
    relationship_response(&state, &database, account.id, target_id).await
}

/// Block a local account and return the resulting relationship.
async fn block(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<AccountPath>,
) -> ApiResult<Response> {
    let target_id = AccountId(path.account_id);
    let txn = database.begin_write().await?;
    if let Some(actor) = roosty_db::find_remote_actor_by_id(&txn, target_id).await? {
        if remote_actor_is_suspended_on(&txn, &actor).await? {
            return Err(ApiError::NotFound("Record not found".into()));
        }
        let (activity_id, job) =
            federation::prepare_remote_block(&state, &txn, account.id, target_id).await?;
        roosty_db::block_remote_account(&txn, account.id, target_id, &activity_id, job).await?;
        txn.commit().await?;
        return relationship_response(&state, &database, account.id, target_id).await;
    }
    roosty_db::block_local_account(&txn, account.id, target_id).await?;
    txn.commit().await?;
    relationship_response(&state, &database, account.id, target_id).await
}

/// Remove a local block and return the resulting relationship.
async fn unblock(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<AccountPath>,
) -> ApiResult<Response> {
    let target_id = AccountId(path.account_id);
    let txn = database.begin_write().await?;
    if let Some(block) =
        roosty_db::find_local_remote_account_block(&txn, account.id, target_id).await?
    {
        let job = federation::prepare_remote_unblock(&state, &txn, &block).await?;
        roosty_db::unblock_remote_account(&txn, account.id, target_id, job).await?;
        txn.commit().await?;
        return relationship_response(&state, &database, account.id, target_id).await;
    }
    roosty_db::unblock_local_account(&txn, account.id, target_id).await?;
    txn.commit().await?;
    relationship_response(&state, &database, account.id, target_id).await
}

/// Mute a local account and return the resulting relationship.
async fn mute(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<AccountPath>,
    request: Request,
) -> ApiResult<Response> {
    let input = mute_input(request).await?;
    let target_id = AccountId(path.account_id);
    let txn = database.begin_write().await?;
    if let Some(actor) = roosty_db::find_remote_actor_by_id(&txn, target_id).await? {
        if remote_actor_is_suspended_on(&txn, &actor).await? {
            return Err(ApiError::NotFound("Record not found".into()));
        }
        roosty_db::mute_remote_account(
            &txn,
            account.id,
            target_id,
            input.notifications.unwrap_or(true),
            input.duration.unwrap_or(0),
        )
        .await?;
        txn.commit().await?;
        return relationship_response(&state, &database, account.id, target_id).await;
    }
    roosty_db::mute_local_account(
        &txn,
        account.id,
        target_id,
        input.notifications.unwrap_or(true),
        input.duration.unwrap_or(0),
    )
    .await?;
    txn.commit().await?;
    relationship_response(&state, &database, account.id, target_id).await
}

/// Remove a local mute and return the resulting relationship.
async fn unmute(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<AccountPath>,
) -> ApiResult<Response> {
    let target_id = AccountId(path.account_id);
    let txn = database.begin_write().await?;
    if roosty_db::find_remote_actor_by_id(&txn, target_id)
        .await?
        .is_some()
    {
        roosty_db::unmute_remote_account(&txn, account.id, target_id).await?;
        txn.commit().await?;
        return relationship_response(&state, &database, account.id, target_id).await;
    }
    roosty_db::unmute_local_account(&txn, account.id, target_id).await?;
    txn.commit().await?;
    relationship_response(&state, &database, account.id, target_id).await
}

/// Unfollow a local account and return the resulting relationship.
async fn unfollow(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<AccountPath>,
) -> ApiResult<Response> {
    let target_id = AccountId(path.account_id);
    let txn = database.begin_write().await?;
    let remote_following = roosty_db::find_remote_following(&txn, account.id, target_id).await?;
    if let Some(following) = remote_following {
        let job = federation::prepare_remote_unfollow(&state, &txn, following).await?;
        roosty_db::delete_remote_following_with_job(&txn, account.id, target_id, job).await?;
    } else {
        roosty_db::unfollow_local_account(&txn, account.id, target_id).await?;
    }
    txn.commit().await?;
    relationship_response(&state, &database, account.id, target_id).await
}

/// Return Mastodon relationship objects for requested account ids.
async fn relationships(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    RawQuery(query): RawQuery,
) -> ApiResult<Response> {
    let txn = database.begin_snapshot().await?;
    let context = TransactionContext::new(&state, &txn);
    let ids = relationship_ids(query.as_deref())?;
    let mut relationships = Vec::with_capacity(ids.len());
    for id in ids {
        relationships.push(relationship_model(&context, account.id, AccountId(id)).await?);
    }
    txn.commit().await?;
    Ok(Json(relationships).into_response())
}

/// List remote actors whose follow requests await this account's approval.
async fn follow_requests(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Query(params): Query<AccountCollectionParams>,
) -> ApiResult<Response> {
    let txn = database.begin_snapshot().await?;
    let context = TransactionContext::new(&state, &txn);
    let limit = params
        .limit
        .unwrap_or(DEFAULT_ACCOUNT_LIMIT)
        .clamp(1, MAX_ACCOUNT_LIMIT);
    let cursor = collection_cursor(&params)?;
    let page = roosty_db::pending_remote_follow_requests(&txn, account.id, limit, cursor).await?;
    let mut actors = Vec::with_capacity(page.items.len());
    for actor in page.items {
        actors.push(remote_account_response(&context, actor).await?);
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
    txn.commit().await?;
    Ok(response)
}

/// Approve a pending remote follow request for the authenticated local account.
async fn authorize_follow_request(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<AccountPath>,
) -> ApiResult<Response> {
    let accepted =
        federation::accept_remote_follow_request(&database, account.id, AccountId(path.account_id))
            .await?;
    if !accepted {
        return Err(ApiError::NotFound("Record not found".into()));
    }
    relationship_response(&state, &database, account.id, AccountId(path.account_id)).await
}

/// Reject a pending remote follow request for the authenticated local account.
async fn reject_follow_request(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<AccountPath>,
) -> ApiResult<Response> {
    let remote_id = AccountId(path.account_id);
    let rejected =
        federation::reject_remote_follow_request(&database, account.id, remote_id).await?;
    if !rejected {
        return Err(ApiError::NotFound("Record not found".into()));
    }
    relationship_response(&state, &database, account.id, remote_id).await
}

/// Return locally known followers for a local or cached remote account.
async fn followers(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    Path(path): Path<AccountPath>,
    Query(params): Query<AccountCollectionParams>,
) -> ApiResult<Response> {
    account_collection(
        &state,
        &database,
        AccountId(path.account_id),
        params,
        AccountCollection::Followers,
    )
    .await
}

/// Return locally known accounts followed by a local or cached remote account.
async fn following(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    Path(path): Path<AccountPath>,
    Query(params): Query<AccountCollectionParams>,
) -> ApiResult<Response> {
    account_collection(
        &state,
        &database,
        AccountId(path.account_id),
        params,
        AccountCollection::Following,
    )
    .await
}

/// Return local accounts blocked by the authenticated account.
async fn blocked_accounts(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Query(params): Query<AccountCollectionParams>,
) -> ApiResult<Response> {
    account_collection(
        &state,
        &database,
        account.id,
        params,
        AccountCollection::Blocks,
    )
    .await
}

/// Return local accounts muted by the authenticated account.
async fn muted_accounts(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Query(params): Query<AccountCollectionParams>,
) -> ApiResult<Response> {
    account_collection(
        &state,
        &database,
        account.id,
        params,
        AccountCollection::Mutes,
    )
    .await
}

/// Return a Mastodon account collection.
async fn account_collection(
    state: &AppState,
    database: &DatabaseContext,
    account_id: AccountId,
    params: AccountCollectionParams,
    collection: AccountCollection,
) -> ApiResult<Response> {
    let limit = params
        .limit
        .unwrap_or(DEFAULT_ACCOUNT_LIMIT)
        .clamp(1, MAX_ACCOUNT_LIMIT);
    let cursor = collection_cursor(&params)?;
    let txn = database.begin_snapshot().await?;
    let context = TransactionContext::new(state, &txn);
    let page = match collection {
        AccountCollection::Followers | AccountCollection::Following => {
            follow_account_collection(&context, account_id, limit, cursor, collection)
                .await?
                .ok_or_else(|| ApiError::NotFound("Record not found".into()))?
        }
        AccountCollection::Blocks => {
            let page =
                roosty_db::blocked_accounts_for_account(&txn, account_id, limit, cursor).await?;
            CollectionPage {
                items: page.items.into_iter().map(|entry| entry.account).collect(),
                first_cursor: page.first_cursor,
                last_cursor: page.last_cursor,
                has_more: page.has_more,
            }
        }
        AccountCollection::Mutes => {
            let page =
                roosty_db::muted_accounts_for_account(&txn, account_id, limit, cursor).await?;
            CollectionPage {
                items: page.items.into_iter().map(|entry| entry.account).collect(),
                first_cursor: page.first_cursor,
                last_cursor: page.last_cursor,
                has_more: page.has_more,
            }
        }
    };
    let accounts = account_responses(&context, page.items).await?;
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
    txn.commit().await?;
    Ok(response)
}

/// Read one local or cached-remote follow collection from a consistent snapshot.
async fn follow_account_collection(
    state: &TransactionContext<'_, impl ConnectionTrait>,
    account_id: AccountId,
    limit: u64,
    cursor: CollectionCursor,
    collection: AccountCollection,
) -> RoostyResult<Option<CollectionPage<FollowCollectionAccount>>> {
    let local = roosty_db::find_local_account_by_id(state.db, account_id)
        .await?
        .is_some();
    let page = if local {
        match collection {
            AccountCollection::Followers => {
                roosty_db::followers_for_local_account(state.db, account_id, limit, cursor).await?
            }
            AccountCollection::Following => {
                roosty_db::following_for_local_account(state.db, account_id, limit, cursor).await?
            }
            AccountCollection::Blocks | AccountCollection::Mutes => unreachable!(),
        }
    } else {
        let Some(actor) = roosty_db::find_remote_actor_by_id(state.db, account_id).await? else {
            return Ok(None);
        };
        if remote_actor_is_suspended_on(state.db, &actor).await? {
            return Ok(None);
        }
        match collection {
            AccountCollection::Followers => {
                roosty_db::followers_for_remote_account(state.db, account_id, limit, cursor).await?
            }
            AccountCollection::Following => {
                roosty_db::following_for_remote_account(state.db, account_id, limit, cursor).await?
            }
            AccountCollection::Blocks | AccountCollection::Mutes => unreachable!(),
        }
    };
    Ok(Some(CollectionPage {
        items: page.items.into_iter().map(|entry| entry.account).collect(),
        first_cursor: page.first_cursor,
        last_cursor: page.last_cursor,
        has_more: page.has_more,
    }))
}

/// Convert local account records into Mastodon account responses.
async fn account_responses(
    state: &TransactionContext<'_, impl ConnectionTrait>,
    accounts: Vec<FollowCollectionAccount>,
) -> RoostyResult<Vec<CollectionAccountResponse>> {
    let mut responses = Vec::with_capacity(accounts.len());
    for account in accounts {
        if let FollowCollectionAccount::Remote(actor) = &account
            && remote_actor_is_suspended_on(state.db, actor).await?
        {
            continue;
        }
        responses.push(match account {
            FollowCollectionAccount::Local(account) => CollectionAccountResponse::Local(Box::new(
                account_response_on(state, state.db, account).await?,
            )),
            FollowCollectionAccount::Remote(actor) => CollectionAccountResponse::Remote(Box::new(
                remote_account_response_on(state, state.db, actor).await?,
            )),
        });
    }
    Ok(responses)
}

async fn remote_actor_is_suspended_on(
    db: &impl ConnectionTrait,
    actor: &RemoteActor,
) -> RoostyResult<bool> {
    if actor.deleted_at.is_some() || actor.suspended_at.is_some() {
        return Ok(true);
    }
    Ok(roosty_db::federation_domain_policy(db, &actor.domain)
        .await?
        .is_suspended())
}

async fn relationship_response(
    state: &AppState,
    database: &DatabaseContext,
    source_id: AccountId,
    target_id: AccountId,
) -> ApiResult<Response> {
    let txn = database.begin_snapshot().await?;
    let context = TransactionContext::new(state, &txn);
    let relationship = relationship_model(&context, source_id, target_id).await?;
    txn.commit().await?;
    Ok(Json(relationship).into_response())
}

/// Build the local Mastodon relationship shape for two accounts.
async fn relationship_model(
    state: &TransactionContext<'_, impl ConnectionTrait>,
    source_id: AccountId,
    target_id: AccountId,
) -> RoostyResult<RelationshipResponse> {
    relationship_model_on(state.db, source_id, target_id).await
}

async fn relationship_model_on(
    db: &impl ConnectionTrait,
    source_id: AccountId,
    target_id: AccountId,
) -> RoostyResult<RelationshipResponse> {
    let following = roosty_db::local_follow_relationship(db, source_id, target_id).await?;
    let remote_following = roosty_db::find_remote_following(db, source_id, target_id).await?;
    let followed_by = roosty_db::local_follow_relationship(db, target_id, source_id).await?;
    let remote_followed_by =
        roosty_db::remote_actor_follows_local_account(db, target_id, source_id).await?;
    let remote_target = roosty_db::find_remote_actor_by_id(db, target_id)
        .await?
        .is_some();
    let (blocking, blocked_by, mute, remote_mute) = if remote_target {
        (
            roosty_db::find_local_remote_account_block(db, source_id, target_id)
                .await?
                .is_some(),
            roosty_db::remote_actor_blocks_local_account(db, target_id, source_id).await?,
            None,
            roosty_db::active_local_remote_account_mute(db, source_id, target_id).await?,
        )
    } else {
        (
            roosty_db::local_account_blocks(db, source_id, target_id).await?,
            roosty_db::local_account_blocks(db, target_id, source_id).await?,
            roosty_db::active_local_account_mute(db, source_id, target_id).await?,
            None,
        )
    };

    Ok(RelationshipResponse {
        id: target_id.0.to_string(),
        following: following.is_some()
            || remote_following
                .as_ref()
                .is_some_and(|follow| follow.state == RemoteFollowState::Accepted),
        showing_reblogs: following.as_ref().is_some_and(|follow| follow.show_reblogs)
            || remote_following
                .as_ref()
                .is_some_and(|follow| follow.show_reblogs),
        notifying: following.as_ref().is_some_and(|follow| follow.notify)
            || remote_following
                .as_ref()
                .is_some_and(|follow| follow.notify),
        followed_by: followed_by.is_some() || remote_followed_by,
        blocking,
        blocked_by,
        muting: mute.is_some() || remote_mute.is_some(),
        muting_notifications: mute.as_ref().is_some_and(|mute| mute.notifications)
            || remote_mute.as_ref().is_some_and(|mute| mute.notifications),
        muting_expires_at: mute
            .and_then(|mute| mute.expires_at)
            .or_else(|| remote_mute.and_then(|mute| mute.expires_at))
            .map(format_timestamp),
        requested: remote_following
            .as_ref()
            .is_some_and(|follow| follow.state == RemoteFollowState::Pending),
        domain_blocking: false,
        endorsed: false,
    })
}

/// Parse optional follow settings from JSON, form, or empty request bodies.
async fn follow_input(request: Request) -> Result<FollowInput, AccountInputError> {
    parse_account_action_input(request).await
}

/// Parse optional mute settings from JSON, form, or empty request bodies.
async fn mute_input(request: Request) -> Result<MuteInput, AccountInputError> {
    parse_account_action_input(request).await
}

/// Parse a small Mastodon account action payload from JSON or URL-encoded form data.
async fn parse_account_action_input<T>(request: Request) -> Result<T, AccountInputError>
where
    T: Default + DeserializeOwned,
{
    let content_type = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let body = to_bytes(request.into_body(), 1024 * 1024).await?;
    if body.is_empty() {
        return Ok(T::default());
    }

    if content_type.contains("application/json") {
        Ok(serde_json::from_slice(&body)?)
    } else {
        Ok(serde_urlencoded::from_bytes(&body)?)
    }
}

/// Parse Mastodon status cursor parameters from an account statuses request.
fn timeline_cursor(params: &AccountStatusesParams) -> Result<TimelineCursor, AccountQueryError> {
    Ok(TimelineCursor {
        max_id: parse_optional_status_id(params.max_id.as_deref())?,
        since_id: parse_optional_status_id(params.since_id.as_deref())?,
        min_id: parse_optional_status_id(params.min_id.as_deref())?,
    })
}

/// Parse Mastodon cursor parameters from an account collection request.
fn collection_cursor(
    params: &AccountCollectionParams,
) -> Result<CollectionCursor, AccountQueryError> {
    Ok(CollectionCursor {
        max_id: parse_optional_uuid(params.max_id.as_deref())?,
        since_id: parse_optional_uuid(params.since_id.as_deref())?,
        min_id: parse_optional_uuid(params.min_id.as_deref())?,
    })
}

/// Parse an optional status UUID from Mastodon cursor query parameters.
fn parse_optional_status_id(value: Option<&str>) -> Result<Option<StatusId>, AccountQueryError> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    Ok(Some(StatusId(value.parse()?)))
}

/// Parse an optional UUID cursor from Mastodon collection query parameters.
fn parse_optional_uuid(value: Option<&str>) -> Result<Option<Uuid>, AccountQueryError> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    Ok(Some(value.parse()?))
}

/// Parse repeated relationship id query parameters.
fn relationship_ids(query: Option<&str>) -> Result<Vec<Uuid>, AccountQueryError> {
    let Some(query) = query else {
        return Ok(Vec::new());
    };

    let params = QueryStringConfig::new()
        .array_format(ArrayFormat::EmptyIndexed)
        .use_form_encoding(true)
        .deserialize_str::<RelationshipsParams>(query)?;
    Ok(params.id)
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

#[cfg(test)]
mod tests {
    use std::{
        fs,
        net::{IpAddr, Ipv4Addr, SocketAddr},
        process,
        time::{SystemTime, UNIX_EPOCH},
    };

    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{Request, Response, StatusCode, header},
    };
    use postgresql_embedded::PostgreSQL;
    use roosty_core::AccountId;
    use roosty_migration::Migrator;
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement, TransactionTrait};
    use sea_orm_migration::MigratorTrait;
    use serde_json::{Value, json};
    use tempfile::TempDir;
    use test_context::{AsyncTestContext, test_context};
    use tokio::time::{Duration, timeout};
    use tower::ServiceExt;

    use super::{format_timestamp, remote_account_response_from_media, remote_custom_emojis};
    use crate::{
        config::{
            Config, ObjectStorageBackend, RegistrationMode, ScheduledStatusConfig, StreamingConfig,
        },
        http::{AppState, DatabaseContext, app_router},
        password,
        test_postgres::settings,
    };

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
            invalid_handle: false,
            display_name: "Alice".to_owned(),
            summary: String::new(),
            emojis: json!([]),
            inbox_url: "https://remote.test/users/alice/inbox".to_owned(),
            shared_inbox_url: None,
            followers_url: None,
            featured_url: None,
            featured_tags_url: None,
            public_key_id: "https://remote.test/users/alice#main-key".to_owned(),
            public_key_pem: "test-public-key".to_owned(),
            expires_at: time::OffsetDateTime::UNIX_EPOCH + time::Duration::days(30),
            profile_created_at: Some(profile_created_at),
            first_seen_at,
            deleted_at: None,
            moved_to_remote_actor_id: None,
            limited_at: None,
            suspended_at: None,
            data_purged_at: None,
            discoverable: Some(true),
        };

        let response = serde_json::to_value(remote_account_response_from_media(
            actor,
            String::new(),
            String::new(),
        ))
        .unwrap();

        assert_eq!(response["created_at"], format_timestamp(profile_created_at));
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
            invalid_handle: false,
            display_name: "Alice".to_owned(),
            summary: String::new(),
            emojis: json!([]),
            inbox_url: "https://remote.test/users/alice/inbox".to_owned(),
            shared_inbox_url: None,
            followers_url: None,
            featured_url: None,
            featured_tags_url: None,
            public_key_id: "https://remote.test/users/alice#main-key".to_owned(),
            public_key_pem: "test-public-key".to_owned(),
            expires_at: time::OffsetDateTime::UNIX_EPOCH + time::Duration::days(30),
            profile_created_at: None,
            first_seen_at,
            deleted_at: None,
            moved_to_remote_actor_id: None,
            limited_at: None,
            suspended_at: None,
            data_purged_at: None,
            discoverable: Some(true),
        };

        let response = serde_json::to_value(remote_account_response_from_media(
            actor,
            String::new(),
            String::new(),
        ))
        .unwrap();

        assert_eq!(response["created_at"], format_timestamp(first_seen_at));
    }

    #[test]
    /// Given a conflicting remote handle, the API emits Mastodon's optional validity flag and a
    /// server-unique placeholder without changing the stable account ID.
    fn invalid_remote_handle_uses_placeholder_acct() {
        let actor_id = AccountId(uuid::Uuid::now_v7());
        let actor = roosty_db::RemoteActor {
            id: actor_id,
            activitypub_id: "https://remote.test/users/alice".to_owned(),
            username: "alice".to_owned(),
            domain: "remote.test".to_owned(),
            invalid_handle: true,
            display_name: "Alice".to_owned(),
            summary: String::new(),
            emojis: json!([]),
            inbox_url: "https://remote.test/users/alice/inbox".to_owned(),
            shared_inbox_url: None,
            followers_url: None,
            featured_url: None,
            featured_tags_url: None,
            public_key_id: "https://remote.test/users/alice#main-key".to_owned(),
            public_key_pem: String::new(),
            expires_at: time::OffsetDateTime::UNIX_EPOCH,
            profile_created_at: None,
            first_seen_at: time::OffsetDateTime::UNIX_EPOCH,
            deleted_at: None,
            moved_to_remote_actor_id: None,
            limited_at: None,
            suspended_at: None,
            data_purged_at: None,
            discoverable: None,
        };

        let value = serde_json::to_value(remote_account_response_from_media(
            actor,
            String::new(),
            String::new(),
        ))
        .unwrap();
        assert_eq!(value["id"], actor_id.0.to_string());
        assert_eq!(value["invalid_handle"], true);
        assert_eq!(
            value["acct"],
            format!("invalid-{}@invalid.invalid", actor_id.0)
        );
    }

    #[test_context(AccountContext)]
    #[tokio::test]
    /// Given mixed discoverable profiles, the directory filters, orders, and paginates them compatibly.
    async fn directory_lists_discoverable_accounts(context: &mut AccountContext) {
        let (_alice_id, alice_token) = context.create_account("alice", "alice@example.com").await;
        let (bob_id, bob_token) = context.create_account("bob", "bob@example.com").await;
        let bob_status = context
            .create_status(&bob_token, "Bob was active", Some("public"))
            .await;
        context.create_remote_actor("carol", "remote.test").await;
        context.create_remote_actor("blocked", "blocked.test").await;
        context.suspend_domain("blocked.test").await;

        let first_page = context.get("/api/v1/directory?order=active&limit=1").await;
        assert_eq!(first_page.status(), StatusCode::OK);
        let link = first_page
            .headers()
            .get(header::LINK)
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        assert!(link.contains("offset=1"));
        assert!(link.contains("rel=\"next\""));
        let first_page = json_body(first_page).await;
        assert_eq!(account_usernames(&first_page), ["bob"]);
        assert_eq!(first_page[0]["last_status_at"].as_str().unwrap().len(), 10);

        let local = context.get("/api/v1/directory?order=new&local=true").await;
        assert_eq!(account_usernames(&json_body(local).await), ["bob", "alice"]);

        let personalized = context
            .authenticated_get("/api/v1/directory", &alice_token)
            .await;
        let personalized = json_body(personalized).await;
        let usernames = account_usernames(&personalized);
        assert!(!usernames.contains(&"alice"));
        assert!(usernames.contains(&"bob"));
        assert!(usernames.contains(&"carol"));
        assert!(!usernames.contains(&"blocked"));

        let deleted = context
            .authenticated_empty(
                "DELETE",
                &format!("/api/v1/statuses/{}", bob_status["id"].as_str().unwrap()),
                &bob_token,
            )
            .await;
        assert_eq!(deleted.status(), StatusCode::OK);
        let row = context
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT last_status_at IS NULL AS cleared FROM local_account WHERE id = $1",
                vec![bob_id.0.into()],
            ))
            .await
            .unwrap()
            .unwrap();
        assert!(row.try_get::<bool>("", "cleared").unwrap());
    }

    #[test_context(AccountContext)]
    #[tokio::test]
    /// Given eligible known accounts, suggestions rank social proof, exclude follows, and persist dismissals.
    async fn suggestions_rank_filter_and_dismiss_accounts(context: &mut AccountContext) {
        let (alice_id, alice_token) = context.create_account("alice", "alice@example.com").await;
        let (bob_id, _bob_token) = context.create_account("bob", "bob@example.com").await;
        let (dave_id, dave_token) = context.create_account("dave", "dave@example.com").await;
        let remote_id = context.create_remote_actor("carol", "remote.test").await;

        let followed = context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/accounts/{}/follow", bob_id.0),
                &dave_token,
            )
            .await;
        assert_eq!(followed.status(), StatusCode::OK);
        let followed = context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/accounts/{}/follow", dave_id.0),
                &alice_token,
            )
            .await;
        assert_eq!(followed.status(), StatusCode::OK);
        roosty_db::refresh_account_suggestion_scores(&context.db)
            .await
            .unwrap();

        let suggestions = context
            .authenticated_get("/api/v2/suggestions?limit=80", &alice_token)
            .await;
        assert_eq!(suggestions.status(), StatusCode::OK);
        let suggestions = json_body(suggestions).await;
        assert_eq!(suggestions[0]["source"], "past_interactions");
        assert_eq!(
            suggestions[0]["sources"],
            json!(["friends_of_friends", "most_followed"])
        );
        assert_eq!(suggestions[0]["account"]["username"], "bob");
        let usernames = suggestions
            .as_array()
            .unwrap()
            .iter()
            .map(|suggestion| suggestion["account"]["username"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert!(!usernames.contains(&"alice"));
        assert!(usernames.contains(&"carol"));
        let second = json_body(
            context
                .authenticated_get("/api/v2/suggestions?limit=1&offset=1", &alice_token)
                .await,
        )
        .await;
        assert_ne!(suggestions[0]["account"]["id"], second[0]["account"]["id"]);

        let followed = context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/accounts/{}/follow", bob_id.0),
                &alice_token,
            )
            .await;
        assert_eq!(followed.status(), StatusCode::OK);
        let suggestions = json_body(
            context
                .authenticated_get("/api/v2/suggestions", &alice_token)
                .await,
        )
        .await;
        assert!(suggestions.as_array().unwrap().iter().all(|suggestion| {
            suggestion["account"]["id"] != bob_id.0.to_string()
                && suggestion["account"]["id"] != alice_id.0.to_string()
        }));

        for _ in 0..2 {
            let dismissed = context
                .authenticated_empty(
                    "DELETE",
                    &format!("/api/v1/suggestions/{}", remote_id.0),
                    &alice_token,
                )
                .await;
            assert_eq!(dismissed.status(), StatusCode::OK);
            assert_eq!(json_body(dismissed).await, json!({}));
        }
        let v1 = json_body(
            context
                .authenticated_get("/api/v1/suggestions", &alice_token)
                .await,
        )
        .await;
        assert!(v1.as_array().unwrap().iter().all(|account| {
            account["id"] != remote_id.0.to_string()
                && account.get("account").is_none()
                && account.get("sources").is_none()
        }));
        let invalid = context
            .authenticated_empty("DELETE", "/api/v1/suggestions/not-an-id", &alice_token)
            .await;
        assert_eq!(invalid.status(), StatusCode::OK);
    }

    #[test_context(AccountContext)]
    #[tokio::test]
    /// A locally known remote account followed by a friend is recommended without cache refresh.
    async fn suggestions_include_live_mixed_friends_of_friends(context: &mut AccountContext) {
        let (_alice_id, alice_token) = context.create_account("alice", "alice@example.com").await;
        let (dave_id, _dave_token) = context.create_account("dave", "dave@example.com").await;
        let remote_id = context.create_remote_actor("carol", "remote.test").await;

        let followed = context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/accounts/{}/follow", dave_id.0),
                &alice_token,
            )
            .await;
        assert_eq!(followed.status(), StatusCode::OK);
        let activity_id = "https://localhost:4000/follows/dave-carol";
        roosty_db::create_remote_following(
            &context.db,
            dave_id,
            remote_id,
            activity_id,
            true,
            false,
        )
        .await
        .unwrap();
        assert!(
            roosty_db::accept_remote_following(&context.db, remote_id, activity_id)
                .await
                .unwrap()
        );

        let suggestions = json_body(
            context
                .authenticated_get("/api/v2/suggestions", &alice_token)
                .await,
        )
        .await;
        let carol = suggestions
            .as_array()
            .unwrap()
            .iter()
            .find(|suggestion| suggestion["account"]["id"] == remote_id.0.to_string())
            .unwrap();
        assert_eq!(carol["source"], "past_interactions");
        assert_eq!(
            carol["sources"],
            json!(["friends_of_friends", "most_followed"])
        );
    }

    #[test_context(AccountContext)]
    #[tokio::test]
    /// Suggestion APIs require a user token with umbrella or account-specific read permission.
    async fn suggestions_enforce_account_read_scope(context: &mut AccountContext) {
        let (_alice_id, write_token) = context
            .create_account_with_scope("alice", "alice@example.com", "write")
            .await;
        let (_bob_id, account_read_token) = context
            .create_account_with_scope("bob", "bob@example.com", "read:accounts")
            .await;

        assert_eq!(
            context.get("/api/v2/suggestions").await.status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            context
                .authenticated_get("/api/v2/suggestions", &write_token)
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            context
                .authenticated_get("/api/v2/suggestions", &account_read_token)
                .await
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            context
                .authenticated_empty(
                    "DELETE",
                    "/api/v1/suggestions/not-an-id",
                    &account_read_token,
                )
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            context
                .authenticated_empty("DELETE", "/api/v1/suggestions/not-an-id", &write_token,)
                .await
                .status(),
            StatusCode::OK
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
    /// Given an active remote follow, posting follow settings again updates the relationship
    /// without enqueueing a second ActivityPub Follow delivery.
    async fn remote_follow_updates_delivery_preferences_idempotently(context: &mut AccountContext) {
        context.config.federation_allowed_domains = vec!["*".to_owned()];
        context.state = AppState::new(context.config.clone(), context.db.clone());
        let (alice_id, alice_token) = context
            .create_account("alice_remote_follow", "alice-remote-follow@example.com")
            .await;
        let remote_id = context
            .create_remote_follow_request(
                alice_id,
                "remote_follow_target",
                roosty_db::RemoteFollowState::Accepted,
            )
            .await;

        let first = context
            .authenticated_json(
                "POST",
                &format!("/api/v1/accounts/{}/follow", remote_id.0),
                &alice_token,
                serde_json::json!({"reblogs": false, "notify": true}),
            )
            .await;
        let first = json_body(first).await;
        assert_eq!(first["requested"], true);
        assert_eq!(first["showing_reblogs"], false);
        assert_eq!(first["notifying"], true);
        let first_relationship = roosty_db::find_remote_following(&context.db, alice_id, remote_id)
            .await
            .unwrap()
            .unwrap();

        let second = context
            .authenticated_json(
                "POST",
                &format!("/api/v1/accounts/{}/follow", remote_id.0),
                &alice_token,
                serde_json::json!({"reblogs": true, "notify": false}),
            )
            .await;
        let second = json_body(second).await;
        assert_eq!(second["showing_reblogs"], true);
        assert_eq!(second["notifying"], false);
        let second_relationship =
            roosty_db::find_remote_following(&context.db, alice_id, remote_id)
                .await
                .unwrap()
                .unwrap();
        assert_eq!(
            second_relationship.activity_id,
            first_relationship.activity_id
        );
        assert!(second_relationship.show_reblogs);
        assert!(!second_relationship.notify);

        let row = context
            .db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT count(*)::bigint AS count FROM job WHERE kind = 'federation_follow_delivery'",
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.try_get::<i64>("", "count").unwrap(), 1);

        context
            .authenticated_json(
                "POST",
                &format!("/api/v1/accounts/{}/follow", remote_id.0),
                &alice_token,
                serde_json::json!({"reblogs": false, "notify": true}),
            )
            .await;
        roosty_db::accept_remote_following(&context.db, remote_id, &first_relationship.activity_id)
            .await
            .unwrap();
        assert!(
            roosty_db::accepted_local_reblog_followers_of_remote_actor(&context.db, remote_id,)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            roosty_db::accepted_local_notified_followers_of_remote_actor(&context.db, remote_id,)
                .await
                .unwrap(),
            [alice_id]
        );
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

    /// Given only locally observed relationships for a cached remote actor, when its graph is
    /// requested, then accepted rows are paginated without fetching its ActivityPub collections.
    #[test_context(AccountContext)]
    #[tokio::test]
    async fn remote_account_collections_expose_only_known_accepted_relationships(
        context: &mut AccountContext,
    ) {
        let remote_id = context
            .create_remote_actor("remote_graph", "remote.test")
            .await;
        let (first_id, _first_token) = context
            .create_account("remote_first", "remote-first@example.com")
            .await;
        let (second_id, _second_token) = context
            .create_account("remote_second", "remote-second@example.com")
            .await;
        let (third_id, _third_token) = context
            .create_account("remote_third", "remote-third@example.com")
            .await;
        let (pending_id, _pending_token) = context
            .create_account("remote_pending", "remote-pending@example.com")
            .await;
        let (inactive_id, _inactive_token) = context
            .create_account("remote_inactive", "remote-inactive@example.com")
            .await;

        for (account_id, name, accepted) in [
            (first_id, "first", true),
            (second_id, "second", true),
            (third_id, "third", true),
            (pending_id, "pending", false),
            (inactive_id, "inactive", true),
        ] {
            let activity_id = format!("https://localhost:4000/follows/{name}");
            roosty_db::create_remote_following(
                &context.db,
                account_id,
                remote_id,
                &activity_id,
                true,
                false,
            )
            .await
            .unwrap();
            if accepted {
                assert!(
                    roosty_db::accept_remote_following(&context.db, remote_id, &activity_id)
                        .await
                        .unwrap()
                );
            }
        }
        context
            .db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE remote_following SET deactivated_at = now() WHERE activity_id = $1",
                vec!["https://localhost:4000/follows/inactive".into()],
            ))
            .await
            .unwrap();

        for (account_id, name, state) in [
            (
                first_id,
                "followed-first",
                roosty_db::RemoteFollowState::Accepted,
            ),
            (
                second_id,
                "followed-second",
                roosty_db::RemoteFollowState::Accepted,
            ),
            (
                pending_id,
                "followed-pending",
                roosty_db::RemoteFollowState::Pending,
            ),
        ] {
            let activity_id = format!("https://remote.test/follows/{name}");
            roosty_db::upsert_remote_follow(
                &context.db,
                remote_id,
                account_id,
                &activity_id,
                json!({"id": activity_id}),
                state,
            )
            .await
            .unwrap();
        }

        let followers = context
            .get(&format!(
                "/api/v1/accounts/{}/followers?limit=2",
                remote_id.0
            ))
            .await;
        assert_eq!(followers.status(), StatusCode::OK);
        let next_cursor = link_cursor(&followers, "next", "max_id");
        assert_eq!(
            account_usernames(&json_body(followers).await),
            ["remote_third", "remote_second"]
        );
        let next = context
            .get(&format!(
                "/api/v1/accounts/{}/followers?limit=2&max_id={next_cursor}",
                remote_id.0
            ))
            .await;
        assert_eq!(next.status(), StatusCode::OK);
        assert!(next.headers().get(header::LINK).is_none());
        assert_eq!(account_usernames(&json_body(next).await), ["remote_first"]);

        let following = context
            .get(&format!(
                "/api/v1/accounts/{}/following?limit=1",
                remote_id.0
            ))
            .await;
        assert_eq!(following.status(), StatusCode::OK);
        let next_cursor = link_cursor(&following, "next", "max_id");
        assert_eq!(
            account_usernames(&json_body(following).await),
            ["remote_second"]
        );
        let next = context
            .get(&format!(
                "/api/v1/accounts/{}/following?limit=1&max_id={next_cursor}",
                remote_id.0
            ))
            .await;
        assert_eq!(next.status(), StatusCode::OK);
        assert_eq!(account_usernames(&json_body(next).await), ["remote_first"]);

        let remote = json_body(
            context
                .get(&format!("/api/v1/accounts/{}", remote_id.0))
                .await,
        )
        .await;
        assert_eq!(remote["followers_count"], 3);
        assert_eq!(remote["following_count"], 2);

        let local = json_body(
            context
                .get(&format!("/api/v1/accounts/{}", first_id.0))
                .await,
        )
        .await;
        assert_eq!(local["following_count"], 1);
    }

    /// Cached remote graph endpoints use the same target visibility rules as account lookup.
    #[test_context(AccountContext)]
    #[tokio::test]
    async fn remote_account_collections_hide_unavailable_targets(context: &mut AccountContext) {
        let deleted_id = context
            .create_remote_actor("deleted_graph", "remote.test")
            .await;
        context
            .db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE remote_actor SET deleted_at = now() WHERE id = $1",
                vec![deleted_id.0.into()],
            ))
            .await
            .unwrap();
        let blocked_id = context
            .create_remote_actor("blocked_graph", "blocked.test")
            .await;
        context.suspend_domain("blocked.test").await;

        for path in [
            format!("/api/v1/accounts/{}/followers", uuid::Uuid::now_v7()),
            format!("/api/v1/accounts/{}/followers", deleted_id.0),
            format!("/api/v1/accounts/{}/following", blocked_id.0),
        ] {
            assert_eq!(context.get(&path).await.status(), StatusCode::NOT_FOUND);
        }
    }

    /// Given pending remote follows, when the owner pages follow requests, then only pending
    /// requests for that owner are returned with Mastodon cursor links.
    #[test_context(AccountContext)]
    #[tokio::test]
    async fn follow_requests_use_cursor_pagination(context: &mut AccountContext) {
        let (owner_id, owner_token) = context.create_account("owner", "owner@example.com").await;
        let (other_id, _other_token) = context.create_account("other", "other@example.com").await;
        context
            .create_remote_follow_request(owner_id, "first", roosty_db::RemoteFollowState::Pending)
            .await;
        context
            .create_remote_follow_request(owner_id, "second", roosty_db::RemoteFollowState::Pending)
            .await;
        context
            .create_remote_follow_request(owner_id, "third", roosty_db::RemoteFollowState::Pending)
            .await;
        context
            .create_remote_follow_request(
                owner_id,
                "accepted",
                roosty_db::RemoteFollowState::Accepted,
            )
            .await;
        context
            .create_remote_follow_request(
                other_id,
                "other-request",
                roosty_db::RemoteFollowState::Pending,
            )
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

    #[test_context(AccountContext)]
    #[tokio::test]
    /// Given a cached remote actor, block and mute endpoints preserve Mastodon response shapes and idempotency.
    async fn remote_block_and_mute_lifecycle(context: &mut AccountContext) {
        context.config.federation_allowed_domains = vec!["*".to_owned()];
        context.state = AppState::new(context.config.clone(), context.db.clone());
        let (alice_id, alice_token) = context
            .create_account("alice_remote_mod", "alice-remote-mod@example.com")
            .await;
        let remote_id = context
            .create_remote_follow_request(
                alice_id,
                "remote_mod",
                roosty_db::RemoteFollowState::Accepted,
            )
            .await;

        let block = context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/accounts/{}/block", remote_id.0),
                &alice_token,
            )
            .await;
        assert_eq!(block.status(), StatusCode::OK);
        assert_eq!(json_body(block).await["blocking"], true);

        let repeated = context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/accounts/{}/block", remote_id.0),
                &alice_token,
            )
            .await;
        assert_eq!(repeated.status(), StatusCode::OK);
        let row = context
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT count(*) AS count FROM job WHERE kind = 'federation_moderation_delivery'",
                Vec::new(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.try_get::<i64>("", "count").unwrap(), 1);

        let blocks = context
            .authenticated_get("/api/v1/blocks", &alice_token)
            .await;
        assert_eq!(account_usernames(&json_body(blocks).await), ["remote_mod"]);

        let unblock = context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/accounts/{}/unblock", remote_id.0),
                &alice_token,
            )
            .await;
        assert_eq!(json_body(unblock).await["blocking"], false);

        let mute = context
            .authenticated_json(
                "POST",
                &format!("/api/v1/accounts/{}/mute", remote_id.0),
                &alice_token,
                json!({"notifications": false, "duration": 0}),
            )
            .await;
        let mute = json_body(mute).await;
        assert_eq!(mute["muting"], true);
        assert_eq!(mute["muting_notifications"], false);
        assert_eq!(mute["muting_expires_at"], Value::Null);
        let mutes = context
            .authenticated_get("/api/v1/mutes", &alice_token)
            .await;
        assert_eq!(account_usernames(&json_body(mutes).await), ["remote_mod"]);
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
    fn link_cursor(response: &Response<Body>, rel: &str, param: &str) -> String {
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
                fs::create_dir_all(parent).unwrap();
            }

            let settings = settings(&data_dir, password_file);
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
                vapid_private_key: None,
                object_storage_backend: ObjectStorageBackend::Local,
                media_root: "./media".to_owned(),
                registration_mode: RegistrationMode::Closed,
                search_indexing_enabled: true.into(),
                federation_enabled: false,
                federation_key_encryption_secret: None,
                federation_allowed_domains: Vec::new(),
                federation_delivery_max_age: time::Duration::days(7),
                federation_key_rotation_interval: time::Duration::days(90),
                federation_key_overlap: time::Duration::days(7),
                remote_media_cache_ttl: time::Duration::days(30),
                remote_media_max_bytes: 40 * 1024 * 1024,
                remote_media_fetch_concurrency: 5,
                preview_card_fetch_concurrency: 5,
                worker_concurrency: 4,
                successful_job_retention: time::Duration::hours(24),
                permanently_failed_job_retention: time::Duration::days(30),
                trends_refresh_interval: time::Duration::minutes(5),
                account_suggestions_refresh_interval: time::Duration::hours(24),
                scheduled_statuses: ScheduledStatusConfig::default(),
                streaming: StreamingConfig::default(),
                instance_name: "Roosty Test".to_owned(),
                instance_description: Some("Endpoint test instance".to_owned()),
            };

            Self {
                postgresql,
                state: AppState::new(config.clone(), db.clone()),
                db,
                config,
                application_id: application.id,
                _temp_dir: temp_dir,
            }
        }

        async fn teardown(self) {
            let AccountContext {
                postgresql,
                db,
                state,
                ..
            } = self;
            drop(state);
            db.close().await.unwrap();
            postgresql.stop().await.unwrap();
        }
    }

    impl AccountContext {
        /// Build an app router backed by this test database.
        fn app(&self) -> Router {
            app_router(
                self.state.clone(),
                DatabaseContext::new(self.db.clone()),
                false,
            )
        }

        /// Send a raw request through the test router.
        async fn request(&self, request: Request<Body>) -> Response<Body> {
            self.app().oneshot(request).await.unwrap()
        }

        /// Send an anonymous GET request.
        async fn get(&self, uri: &str) -> Response<Body> {
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
        async fn authenticated_get(&self, uri: &str, token: &str) -> Response<Body> {
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
        ) -> Response<Body> {
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
        ) -> Response<Body> {
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
            self.create_account_with_scope(username, email, "read write follow push")
                .await
        }

        /// Create a local account with a specifically scoped access token.
        async fn create_account_with_scope(
            &self,
            username: &str,
            email: &str,
            scopes: &str,
        ) -> (AccountId, String) {
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
                scopes,
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
            state: roosty_db::RemoteFollowState,
        ) -> AccountId {
            let actor_id = self.create_remote_actor(username, "remote.test").await;
            roosty_db::upsert_remote_follow(
                &self.db,
                actor_id,
                local_account_id,
                &format!("https://remote.test/follows/{username}"),
                serde_json::json!({ "id": format!("https://remote.test/follows/{username}") }),
                state,
            )
            .await
            .unwrap();
            actor_id
        }

        /// Cache a remote actor with a deliberately unfetched followers collection.
        async fn create_remote_actor(&self, username: &str, domain: &str) -> AccountId {
            let actor = roosty_db::RemoteActor {
                id: AccountId(uuid::Uuid::now_v7()),
                activitypub_id: format!("https://{domain}/users/{username}"),
                username: username.to_owned(),
                domain: domain.to_owned(),
                invalid_handle: false,
                display_name: username.to_owned(),
                summary: String::new(),
                emojis: json!([]),
                inbox_url: format!("https://{domain}/users/{username}/inbox"),
                shared_inbox_url: None,
                followers_url: Some(format!("https://{domain}/users/{username}/followers")),
                featured_url: None,
                featured_tags_url: None,
                public_key_id: format!("https://{domain}/users/{username}#main-key"),
                public_key_pem: "test-public-key".to_owned(),
                expires_at: time::OffsetDateTime::now_utc() + time::Duration::hours(1),
                profile_created_at: None,
                first_seen_at: time::OffsetDateTime::now_utc(),
                deleted_at: None,
                moved_to_remote_actor_id: None,
                limited_at: None,
                suspended_at: None,
                data_purged_at: None,
                discoverable: Some(true),
            };
            let actor = roosty_db::upsert_remote_actor(&self.db, &actor)
                .await
                .unwrap();
            actor.id
        }

        /// Persist one suspend-level federation rule for policy-sensitive API tests.
        async fn suspend_domain(&self, domain: &str) {
            let txn = self.db.begin().await.unwrap();
            roosty_db::create_federation_domain_block(
                &txn,
                roosty_db::NewFederationDomainBlock {
                    domain: domain.to_owned(),
                    severity: roosty_db::DomainBlockSeverity::Suspend,
                    reject_media: false,
                    reject_reports: false,
                    private_comment: None,
                    public_comment: None,
                    obfuscate: false,
                },
            )
            .await
            .unwrap();
            txn.commit().await.unwrap();
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
    async fn json_body(response: Response<Body>) -> Value {
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    /// Build a unique database name for parallel embedded PostgreSQL tests.
    fn unique_name() -> String {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();

        format!("roosty_accounts_{}_{}", process::id(), timestamp)
    }
}
