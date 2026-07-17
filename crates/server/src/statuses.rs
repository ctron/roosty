use std::collections::{HashMap, HashSet, VecDeque};

use axum::{
    Json, Router,
    body::to_bytes,
    extract::{Path, Query, RawQuery, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use roosty_core::{AccountId, RoostyError, StatusId};
use roosty_db::{
    LocalNotificationType, RemoteConversationParticipant, RemoteStatus, StatusContextItem,
    StatusContextParent, StatusVisibility,
};
use sea_orm::{AccessMode, ConnectionTrait, DatabaseTransaction, TransactionTrait};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;
use tracing::warn;
use uuid::Uuid;

use crate::{
    accounts::{RemoteAccountResponse, remote_account_response, remote_custom_emojis},
    auth::{AccountResponse, AuthenticatedAccount, OptionalAuthenticatedAccount, account_response},
    conversations::{publish_conversation_update, publish_conversation_updates},
    federation::{
        StatusActivityKind, enqueue_status_activity_in_transaction, prepare_remote_favourite,
        prepare_remote_reblog, prepare_remote_unfavourite, prepare_remote_unreblog,
        resolve_remote_mentions,
    },
    http::AppState,
    media::{MediaAttachmentResponse, media_response, remote_media_attachment_response},
    notifications::{create_and_stream_notification, publish_committed_notification},
};

const DEFAULT_LIMIT: u64 = 20;
const MAX_LIMIT: u64 = 40;
const PUBLIC_CONTEXT_ANCESTORS_LIMIT: usize = 40;
const PUBLIC_CONTEXT_DESCENDANTS_LIMIT: usize = 60;
const PUBLIC_CONTEXT_DESCENDANTS_DEPTH: usize = 20;
const AUTHENTICATED_CONTEXT_LIMIT: usize = 4_096;
const MAX_STATUS_CHARS: usize = 500;
const MAX_MEDIA_ATTACHMENTS: u64 = 4;

/// Build routes for local status creation, lookup, deletion, and timelines.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/statuses", post(create_status))
        .route(
            "/api/v1/statuses/{status_id}",
            get(show_status).put(update_status).delete(delete_status),
        )
        .route("/api/v1/statuses/{status_id}/source", get(status_source))
        .route("/api/v1/statuses/{status_id}/context", get(status_context))
        .route(
            "/api/v1/statuses/{status_id}/favourite",
            post(favourite_status),
        )
        .route(
            "/api/v1/statuses/{status_id}/unfavourite",
            post(unfavourite_status),
        )
        .route(
            "/api/v1/statuses/{status_id}/bookmark",
            post(bookmark_status),
        )
        .route(
            "/api/v1/statuses/{status_id}/unbookmark",
            post(unbookmark_status),
        )
        .route("/api/v1/statuses/{status_id}/reblog", post(reblog_status))
        .route(
            "/api/v1/statuses/{status_id}/unreblog",
            post(unreblog_status),
        )
        .route(
            "/api/v1/statuses/{status_id}/reblogged_by",
            get(reblogged_by),
        )
        .route("/api/v1/favourites", get(favourites))
        .route("/api/v1/bookmarks", get(bookmarks))
        .route("/api/v1/timelines/home", get(home_timeline))
        .route("/api/v1/timelines/public", get(public_timeline))
        .route("/api/v1/timelines/tag/{hashtag}", get(tag_timeline))
        .route("/api/v1/tags/{hashtag}", get(show_tag))
        .route("/api/v1/tags/{hashtag}/follow", post(follow_tag))
        .route("/api/v1/tags/{hashtag}/unfollow", post(unfollow_tag))
}

#[derive(Debug, thiserror::Error)]
enum StatusInputError {
    #[error("invalid JSON: {0}")]
    Json(serde_json::Error),
    #[error("invalid form body: {0}")]
    Form(String),
    #[error("status must not be empty")]
    Empty,
    #[error("status is too long")]
    TooLong,
    #[error("status id is invalid")]
    StatusId,
    #[error("media id is invalid")]
    MediaId,
    #[error("too many media attachments")]
    TooManyMedia,
    #[error("media attribute is invalid")]
    MediaAttribute,
}

#[derive(Deserialize)]
struct StatusPath {
    status_id: Uuid,
}

#[derive(Deserialize)]
struct TagPath {
    hashtag: String,
}

#[derive(Clone, Copy, Debug)]
struct TimelineQuery {
    limit: u64,
    cursor: roosty_db::TimelineCursor,
}

#[derive(Deserialize)]
struct TimelineParams {
    limit: Option<u64>,
    max_id: Option<String>,
    since_id: Option<String>,
    min_id: Option<String>,
}

#[derive(Default, Deserialize)]
struct TagTimelineParams {
    limit: Option<u64>,
    max_id: Option<String>,
    since_id: Option<String>,
    min_id: Option<String>,
    #[serde(default)]
    any: Vec<String>,
    #[serde(default)]
    all: Vec<String>,
    #[serde(default)]
    none: Vec<String>,
    local: Option<bool>,
    remote: Option<bool>,
    only_media: Option<bool>,
}

#[derive(Deserialize)]
struct CollectionParams {
    limit: Option<u64>,
    max_id: Option<String>,
    since_id: Option<String>,
    min_id: Option<String>,
}

#[derive(Deserialize)]
struct StatusInput {
    status: Option<String>,
    visibility: Option<String>,
    sensitive: Option<bool>,
    #[serde(alias = "spoilerText")]
    spoiler_text: Option<String>,
    language: Option<String>,
    #[serde(alias = "inReplyToId")]
    in_reply_to_id: Option<String>,
    #[serde(alias = "mediaIds")]
    media_ids: Option<Vec<String>>,
    #[serde(default, alias = "mediaAttributes")]
    media_attributes: Vec<MediaAttributeInput>,
}

#[derive(Deserialize)]
struct MediaAttributeInput {
    id: String,
    description: Option<String>,
    focus: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct StatusResponse {
    id: String,
    created_at: String,
    edited_at: Option<String>,
    in_reply_to_id: Option<String>,
    in_reply_to_account_id: Option<String>,
    sensitive: bool,
    spoiler_text: String,
    visibility: String,
    language: Option<String>,
    uri: String,
    url: String,
    content: String,
    account: StatusAccountResponse,
    media_attachments: Vec<MediaAttachmentResponse>,
    mentions: Vec<MentionResponse>,
    tags: Vec<TagResponse>,
    emojis: Vec<Value>,
    reblogs_count: u64,
    favourites_count: u64,
    replies_count: u64,
    favourited: bool,
    reblogged: bool,
    muted: bool,
    bookmarked: bool,
    pinned: bool,
    reblog: Option<Box<StatusResponse>>,
    application: Option<Value>,
}

/// Plain-text source fields used to populate Mastodon-compatible status editors.
#[derive(Serialize)]
struct StatusSourceResponse {
    id: String,
    text: String,
    spoiler_text: String,
}

/// Mastodon account projection for either a local or cached remote status author.
#[derive(Serialize)]
#[serde(untagged)]
enum StatusAccountResponse {
    Local(Box<AccountResponse>),
    Remote(Box<RemoteAccountResponse>),
}

/// Mastodon-compatible hashtag response.
#[derive(Clone, Serialize)]
pub(crate) struct TagResponse {
    id: String,
    name: String,
    url: String,
    history: Vec<TagHistoryResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    following: Option<bool>,
}

#[derive(Clone, Serialize)]
struct TagHistoryResponse {
    day: String,
    uses: String,
    accounts: String,
}

impl TagResponse {
    /// Build the public tag response for a local tag and computed usage history.
    pub(crate) fn new(
        state: &AppState,
        tag: roosty_db::LocalTag,
        history: Vec<roosty_db::LocalTagHistory>,
        following: Option<bool>,
    ) -> Self {
        Self {
            id: tag.id.to_string(),
            name: tag.name.clone(),
            url: public_url(state, &format!("tags/{}", tag.name)),
            history: history
                .into_iter()
                .map(|bucket| TagHistoryResponse {
                    day: bucket.day.to_string(),
                    uses: bucket.uses.to_string(),
                    accounts: bucket.accounts.to_string(),
                })
                .collect(),
            following,
        }
    }
}

#[derive(Serialize)]
struct ContextResponse {
    ancestors: Vec<StatusResponse>,
    descendants: Vec<StatusResponse>,
}

#[derive(Serialize)]
struct MentionResponse {
    id: String,
    username: String,
    url: String,
    acct: String,
}

impl MentionResponse {
    /// Build the Mastodon mention shape for a local account referenced by a reply.
    fn new(state: &AppState, account: &roosty_db::LocalAccount) -> Self {
        Self {
            id: account.id.0.to_string(),
            username: account.username.clone(),
            url: public_url(state, &format!("@{}", account.username)),
            acct: account.username.clone(),
        }
    }

    /// Build the Mastodon mention shape for a cached remote actor.
    fn remote(actor: &roosty_db::RemoteActor) -> Self {
        Self {
            id: actor.id.0.to_string(),
            username: actor.username.clone(),
            url: actor.activitypub_id.clone(),
            acct: format!("{}@{}", actor.username, actor.domain),
        }
    }
}

#[derive(Serialize)]
struct ErrorResponse<'a> {
    error: &'a str,
    error_description: &'a str,
}

#[derive(Clone, Copy)]
enum StatusCollectionAction {
    Favourite,
    Unfavourite,
    Bookmark,
    Unbookmark,
    Reblog,
    Unreblog,
}

#[derive(Clone, Copy)]
enum StatusCollectionList {
    Favourites,
    Bookmarks,
}

struct ReplyTarget {
    account_id: AccountId,
    account: roosty_db::LocalAccount,
}

async fn create_status(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    request: axum::extract::Request,
) -> Response {
    let input = match parse_status_input(request).await {
        Ok(input) => input,
        Err(error) => return bad_request(&error.to_string()),
    };
    let media_ids = match parse_media_ids(input.media_ids.as_deref().unwrap_or_default()) {
        Ok(media_ids) => media_ids,
        Err(error) => return bad_request(&error.to_string()),
    };
    if let Err(error) = validate_status_text(
        input.status.as_deref().unwrap_or_default(),
        !media_ids.is_empty(),
    ) {
        return bad_request(&error.to_string());
    }

    let visibility = input
        .visibility
        .unwrap_or_else(|| account.default_visibility.to_string());
    let visibility = match StatusVisibility::parse(&visibility) {
        Ok(visibility) => visibility,
        Err(error) => return bad_request(&error.to_string()),
    };
    let mut in_reply_to_id = match parse_optional_status_id(input.in_reply_to_id.as_deref()) {
        Ok(status_id) => status_id,
        Err(error) => return bad_request(&error.to_string()),
    };
    let mut in_reply_to_remote_status_id = None;
    let mut notifiable_reply = true;
    if let Some(parent_id) = in_reply_to_id {
        match roosty_db::find_local_status_by_id(&state.db, parent_id).await {
            Ok(Some(parent)) => {
                notifiable_reply = parent.account_id == account.id;
                match status_visible_to_viewer(&state, &parent, Some(account.id)).await {
                    Ok(true) => {}
                    Ok(false) => return bad_request("reply target status does not exist"),
                    Err(error) => return server_error(error),
                }
            }
            Ok(None) => match roosty_db::find_remote_status_by_id(&state.db, parent_id).await {
                Ok(Some(parent)) => {
                    notifiable_reply = false;
                    let visible = if matches!(
                        parent.visibility,
                        StatusVisibility::Public | StatusVisibility::Unlisted
                    ) {
                        Ok(true)
                    } else {
                        roosty_db::remote_status_visible_to_account(&state.db, &parent, account.id)
                            .await
                    };
                    match visible {
                        Ok(true) => {
                            in_reply_to_remote_status_id = Some(parent.id);
                            in_reply_to_id = None;
                        }
                        Ok(false) => return bad_request("reply target status does not exist"),
                        Err(error) => return server_error(error),
                    }
                }
                Ok(None) => return bad_request("reply target status does not exist"),
                Err(error) => return server_error(error),
            },
            Err(error) => return server_error(error),
        }
    }

    let new_status = roosty_db::NewLocalStatus {
        account_id: account.id,
        content: input.status.unwrap_or_default().trim().to_owned(),
        visibility,
        sensitive: input.sensitive.unwrap_or(account.default_sensitive),
        spoiler_text: input.spoiler_text.unwrap_or_default(),
        language: input.language.or(account.default_language.clone()),
        in_reply_to_id,
        in_reply_to_remote_status_id,
    };

    let author_id = account.id;
    let creates_follow_notification = visibility != StatusVisibility::Direct && notifiable_reply;
    let has_explicit_audience = matches!(
        new_status.visibility,
        StatusVisibility::Private | StatusVisibility::Direct
    );
    let remote_mentions = if state.config.federation_enabled
        && matches!(
            new_status.visibility,
            StatusVisibility::Public
                | StatusVisibility::Unlisted
                | StatusVisibility::Private
                | StatusVisibility::Direct
        ) {
        resolve_remote_mentions(&state, &new_status.content).await
    } else {
        Vec::new()
    };
    let tag_names = hashtag_names(&new_status.content);
    let remote_mention_ids = remote_mentions
        .iter()
        .map(|actor| actor.id)
        .collect::<Vec<_>>();
    let notification_recipients = match local_text_mentions(&state, &new_status.content).await {
        Ok(accounts) => accounts,
        Err(error) => return server_error(error),
    };
    let txn = match state.db.begin().await {
        Ok(txn) => txn,
        Err(error) => return server_error(error.into()),
    };
    match roosty_db::create_local_status_with_media(
        &txn,
        new_status,
        &media_ids,
        roosty_db::LocalStatusMetadata {
            tag_names,
            remote_actor_ids: remote_mention_ids,
            local_recipient_ids: if has_explicit_audience {
                notification_recipients
                    .iter()
                    .map(|account| account.id)
                    .collect()
            } else {
                Vec::new()
            },
        },
    )
    .await
    {
        Ok(mut status) => {
            let mut notifications = Vec::new();
            for account_id in notification_recipients
                .iter()
                .filter(|recipient| recipient.id != author_id)
                .map(|recipient| recipient.id)
            {
                let allowed =
                    match roosty_db::local_account_allows_notification(&txn, account_id, author_id)
                        .await
                    {
                        Ok(allowed) => allowed,
                        Err(error) => return server_error(error),
                    };
                if !allowed {
                    continue;
                }
                match roosty_db::notify_local_account(
                    &txn,
                    account_id,
                    LocalNotificationType::Mention,
                    author_id,
                    Some(status.id),
                )
                .await
                {
                    Ok(notification) => notifications.push(notification),
                    Err(error) => return server_error(error),
                }
            }
            if creates_follow_notification {
                let notified_followers =
                    match roosty_db::local_notified_follower_ids_for_account(&txn, author_id).await
                    {
                        Ok(followers) => followers,
                        Err(error) => return server_error(error),
                    };
                for account_id in notified_followers {
                    let allowed = match roosty_db::local_account_allows_notification(
                        &txn, account_id, author_id,
                    )
                    .await
                    {
                        Ok(allowed) => allowed,
                        Err(error) => return server_error(error),
                    };
                    if !allowed {
                        continue;
                    }
                    match roosty_db::notify_local_account(
                        &txn,
                        account_id,
                        LocalNotificationType::Status,
                        author_id,
                        Some(status.id),
                    )
                    .await
                    {
                        Ok(notification) => notifications.push(notification),
                        Err(error) => return server_error(error),
                    }
                }
            }
            if let Err(error) =
                attach_direct_conversation(&state, &txn, &mut status, author_id).await
            {
                return server_error(error);
            }
            if let Err(error) = enqueue_status_activity_in_transaction(
                &state,
                &txn,
                &status,
                StatusActivityKind::Create,
                &[],
            )
            .await
            {
                return server_error(error);
            }
            if let Err(error) = txn.commit().await {
                return server_error(error.into());
            }
            if let Some(conversation_id) = status.conversation_id
                && let Err(error) = publish_conversation_update(&state, conversation_id).await
            {
                warn!(%error, "failed to publish conversation update");
            }

            match status_response(&state, status.clone(), account).await {
                Ok(response) => {
                    for notification in notifications {
                        if let Err(error) = publish_committed_notification(
                            &state,
                            notification.account_id,
                            notification,
                        )
                        .await
                        {
                            warn!(%error, "failed to publish status notification");
                        }
                    }
                    let recipients = status_stream_recipients(&state, &status).await;
                    state.streaming_events.publish_status_update(
                        &response,
                        author_id,
                        &response.visibility,
                        &recipients,
                    );
                    (StatusCode::OK, Json(response)).into_response()
                }
                Err(error) => server_error(error),
            }
        }
        Err(error) => server_error(error),
    }
}

async fn show_status(
    State(state): State<AppState>,
    OptionalAuthenticatedAccount(viewer): OptionalAuthenticatedAccount,
    Path(path): Path<StatusPath>,
) -> Response {
    let viewer_id = viewer.as_ref().map(|account| account.id);
    let txn = match state
        .db
        .begin_with_config(None, Some(AccessMode::ReadOnly))
        .await
    {
        Ok(txn) => txn,
        Err(error) => return server_error(error.into()),
    };
    let item = match find_status_context_item(&txn, StatusId(path.status_id)).await {
        Ok(Some(item)) => item,
        Ok(None) => return not_found(),
        Err(error) => return server_error(error),
    };
    match status_context_item_visible(&txn, &item, viewer_id).await {
        Ok(true) => {}
        Ok(false) => return not_found(),
        Err(error) => return server_error(error),
    }
    if let Err(error) = txn.commit().await {
        return server_error(error.into());
    }
    match item {
        StatusContextItem::Local(status) => {
            status_with_author_response(&state, status, viewer_id).await
        }
        StatusContextItem::Remote(status) => {
            match remote_status_available(&state, &status).await {
                Ok(true) => {}
                Ok(false) => return not_found(),
                Err(error) => return server_error(error),
            }
            match remote_status_response_for_viewer(&state, status, viewer_id).await {
                Ok(status) => Json(status).into_response(),
                Err(error) => server_error(error),
            }
        }
    }
}

async fn status_source(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<StatusPath>,
) -> Response {
    let status = match roosty_db::find_local_status_by_id(&state.db, StatusId(path.status_id)).await
    {
        Ok(Some(status)) => status,
        Ok(None) => return not_found(),
        Err(error) => return server_error(error),
    };
    match status_visible_to_viewer(&state, &status, Some(account.id)).await {
        Ok(true) => Json(StatusSourceResponse {
            id: status.id.0.to_string(),
            text: status.content,
            spoiler_text: status.spoiler_text,
        })
        .into_response(),
        Ok(false) => not_found(),
        Err(error) => server_error(error),
    }
}

async fn update_status(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<StatusPath>,
    request: axum::extract::Request,
) -> Response {
    let status_id = StatusId(path.status_id);
    let existing = match roosty_db::find_local_status_by_id(&state.db, status_id).await {
        Ok(Some(status)) if status.account_id == account.id && status.deleted_at.is_none() => {
            status
        }
        Ok(Some(_)) | Ok(None) => return not_found(),
        Err(error) => return server_error(error),
    };
    let input = match parse_status_input(request).await {
        Ok(input) => input,
        Err(error) => return bad_request(&error.to_string()),
    };
    let media_ids = match input.media_ids.as_deref() {
        Some(values) => match parse_media_ids(values) {
            Ok(media_ids) => Some(media_ids),
            Err(error) => return bad_request(&error.to_string()),
        },
        None => None,
    };
    let media_attributes = match parse_media_attributes(&input.media_attributes) {
        Ok(attributes) => attributes,
        Err(error) => return bad_request(&error.to_string()),
    };
    let has_media = match media_ids.as_ref() {
        Some(media_ids) => !media_ids.is_empty(),
        None => match roosty_db::local_status_has_media(&state.db, status_id).await {
            Ok(has_media) => has_media,
            Err(error) => return server_error(error),
        },
    };
    if let Some(status) = input.status.as_deref()
        && let Err(error) = validate_status_text(status, has_media)
    {
        return bad_request(&error.to_string());
    }

    let update = roosty_db::LocalStatusUpdate {
        content: input.status.map(|status| status.trim().to_owned()),
        sensitive: input.sensitive,
        spoiler_text: input.spoiler_text,
        language: input.language.map(Some),
    };
    let final_content = update
        .content
        .clone()
        .unwrap_or_else(|| existing.content.clone());
    let remote_mentions = if state.config.federation_enabled
        && matches!(
            existing.visibility,
            StatusVisibility::Public
                | StatusVisibility::Unlisted
                | StatusVisibility::Private
                | StatusVisibility::Direct
        ) {
        resolve_remote_mentions(&state, &final_content).await
    } else {
        Vec::new()
    };
    let remote_mention_ids = remote_mentions
        .iter()
        .map(|actor| actor.id)
        .collect::<Vec<_>>();
    let local_recipient_ids = if matches!(
        existing.visibility,
        StatusVisibility::Private | StatusVisibility::Direct
    ) {
        match local_text_mentions(&state, &final_content).await {
            Ok(accounts) => accounts.into_iter().map(|account| account.id).collect(),
            Err(error) => return server_error(error),
        }
    } else {
        Vec::new()
    };
    let previous_remote_recipients =
        match roosty_db::remote_mentions_for_local_status(&state.db, existing.id).await {
            Ok(recipients) => recipients,
            Err(error) => return server_error(error),
        };
    let txn = match state.db.begin().await {
        Ok(txn) => txn,
        Err(error) => return server_error(error.into()),
    };
    match roosty_db::update_owned_local_status(
        &txn,
        status_id,
        account.id,
        update,
        media_ids.as_deref(),
        &media_attributes,
        roosty_db::LocalStatusMetadata {
            tag_names: hashtag_names(&final_content),
            remote_actor_ids: remote_mention_ids,
            local_recipient_ids,
        },
    )
    .await
    {
        Ok(Some(status)) => {
            if status.visibility == StatusVisibility::Direct
                && let Err(error) = sync_edited_direct_conversation(&state, &txn, &status).await
            {
                return server_error(error);
            }
            let refresh = match roosty_db::repair_direct_conversation_after_delete(
                &txn,
                status.conversation_id,
            )
            .await
            {
                Ok(refresh) => refresh,
                Err(error) => return server_error(error),
            };
            if let Err(error) = enqueue_status_activity_in_transaction(
                &state,
                &txn,
                &status,
                StatusActivityKind::Update,
                &previous_remote_recipients,
            )
            .await
            {
                return server_error(error);
            }
            if let Err(error) = txn.commit().await {
                return server_error(error.into());
            }
            if let Some(refresh) = &refresh {
                state.streaming_events.publish_delete(
                    &status.id.0.to_string(),
                    status.account_id,
                    "direct",
                    &refresh.removed_account_ids,
                );
            }
            if let Some(refresh) = refresh {
                let mut account_ids = refresh.updated_account_ids;
                match roosty_db::local_conversation_accounts_for_last_status(
                    &state.db,
                    refresh.conversation_id,
                    status.id,
                )
                .await
                {
                    Ok(last_status_account_ids) => {
                        account_ids.extend(last_status_account_ids);
                        account_ids.sort_by_key(|id| id.0);
                        account_ids.dedup();
                        if let Err(error) = publish_conversation_updates(
                            &state,
                            refresh.conversation_id,
                            &account_ids,
                        )
                        .await
                        {
                            warn!(%error, "failed to publish conversation update after status edit");
                        }
                    }
                    Err(error) => {
                        warn!(%error, "failed to resolve changed conversation views after status edit")
                    }
                }
            }
            match status_response(&state, status.clone(), account).await {
                Ok(response) => {
                    let recipients = status_stream_recipients(&state, &status).await;
                    state.streaming_events.publish_status_edit(
                        &response,
                        status.account_id,
                        &response.visibility,
                        &recipients,
                    );
                    Json(response).into_response()
                }
                Err(error) => server_error(error),
            }
        }
        Ok(None) => not_found(),
        Err(RoostyError::InvalidInput(error)) => bad_request(&error),
        Err(error) => server_error(error),
    }
}

async fn delete_status(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<StatusPath>,
) -> Response {
    let status_id = StatusId(path.status_id);
    let txn = match state.db.begin().await {
        Ok(txn) => txn,
        Err(error) => return server_error(error.into()),
    };
    match roosty_db::delete_owned_local_status(&txn, status_id, account.id).await {
        Ok(Some(status)) => match status_response(&state, status.clone(), account).await {
            Ok(response) => {
                let refresh = match roosty_db::repair_direct_conversation_after_delete(
                    &txn,
                    status.conversation_id,
                )
                .await
                {
                    Ok(conversation_id) => conversation_id,
                    Err(error) => return server_error(error),
                };
                if let Err(error) = enqueue_status_activity_in_transaction(
                    &state,
                    &txn,
                    &status,
                    StatusActivityKind::Delete,
                    &[],
                )
                .await
                {
                    return server_error(error);
                }
                if let Err(error) = txn.commit().await {
                    return server_error(error.into());
                }
                let reblogs = match roosty_db::local_reblogs_for_status(&state.db, status_id).await
                {
                    Ok(reblogs) => reblogs,
                    Err(error) => return server_error(error),
                };
                publish_status_delete(&state, &status, &reblogs).await;
                if let Some(refresh) = &refresh {
                    state.streaming_events.publish_delete(
                        &status.id.0.to_string(),
                        status.account_id,
                        "direct",
                        &refresh.removed_account_ids,
                    );
                }
                if let Some(refresh) = refresh
                    && let Err(error) = publish_conversation_updates(
                        &state,
                        refresh.conversation_id,
                        &refresh.updated_account_ids,
                    )
                    .await
                {
                    warn!(%error, "failed to publish conversation update after status deletion");
                }
                Json(response).into_response()
            }
            Err(error) => server_error(error),
        },
        Ok(None) => not_found(),
        Err(RoostyError::InvalidInput(error)) => forbidden(&error),
        Err(error) => server_error(error),
    }
}

/// Publish delete events for a removed original status and its local boost wrappers.
async fn publish_status_delete(
    state: &AppState,
    status: &roosty_db::LocalStatus,
    reblogs: &[roosty_db::LocalStatusReblog],
) {
    let recipients = status_stream_recipients(state, status).await;
    state.streaming_events.publish_delete(
        &status.id.0.to_string(),
        status.account_id,
        (&status.visibility).into(),
        &recipients,
    );
    for reblog in reblogs {
        let recipients = reblog_stream_recipients(state, reblog.account_id).await;
        state.streaming_events.publish_delete(
            &reblog.id.to_string(),
            reblog.account_id,
            "direct",
            &recipients,
        );
    }
}

async fn status_context(
    State(state): State<AppState>,
    OptionalAuthenticatedAccount(viewer): OptionalAuthenticatedAccount,
    Path(path): Path<StatusPath>,
) -> Response {
    let status_id = StatusId(path.status_id);
    let viewer = viewer.as_ref().map(|account| account.id);
    let txn = match state
        .db
        .begin_with_config(None, Some(AccessMode::ReadOnly))
        .await
    {
        Ok(txn) => txn,
        Err(error) => return server_error(error.into()),
    };
    let status = match find_status_context_item(&txn, status_id).await {
        Ok(Some(status)) => status,
        Ok(None) => return not_found(),
        Err(error) => return server_error(error),
    };
    match status_context_item_visible(&txn, &status, viewer).await {
        Ok(true) => {}
        Ok(false) => return not_found(),
        Err(error) => return server_error(error),
    }

    let limits = StatusContextLimits::for_viewer(viewer);
    let ancestors = match status_ancestors(&txn, &status, viewer, limits).await {
        Ok(ancestors) => ancestors,
        Err(error) => return server_error(error),
    };
    let descendants = match status_descendants(&txn, &status, viewer, limits).await {
        Ok(descendants) => descendants,
        Err(error) => return server_error(error),
    };
    if let Err(error) = txn.commit().await {
        return server_error(error.into());
    }
    let ancestors = match status_context_models(&state, ancestors, viewer).await {
        Ok(ancestors) => ancestors,
        Err(error) => return server_error(error),
    };
    let descendants = match status_context_models(&state, descendants, viewer).await {
        Ok(descendants) => descendants,
        Err(error) => return server_error(error),
    };

    Json(ContextResponse {
        ancestors,
        descendants,
    })
    .into_response()
}

async fn favourite_status(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<StatusPath>,
) -> Response {
    status_collection_action(&state, account.id, path, StatusCollectionAction::Favourite).await
}

async fn unfavourite_status(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<StatusPath>,
) -> Response {
    status_collection_action(
        &state,
        account.id,
        path,
        StatusCollectionAction::Unfavourite,
    )
    .await
}

async fn bookmark_status(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<StatusPath>,
) -> Response {
    status_collection_action(&state, account.id, path, StatusCollectionAction::Bookmark).await
}

async fn unbookmark_status(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<StatusPath>,
) -> Response {
    status_collection_action(&state, account.id, path, StatusCollectionAction::Unbookmark).await
}

async fn reblog_status(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<StatusPath>,
) -> Response {
    status_collection_action(&state, account.id, path, StatusCollectionAction::Reblog).await
}

async fn unreblog_status(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<StatusPath>,
) -> Response {
    status_collection_action(&state, account.id, path, StatusCollectionAction::Unreblog).await
}

async fn reblogged_by(
    State(state): State<AppState>,
    OptionalAuthenticatedAccount(viewer): OptionalAuthenticatedAccount,
    Path(path): Path<StatusPath>,
    Query(params): Query<CollectionParams>,
) -> Response {
    let viewer_id = viewer.as_ref().map(|account| account.id);
    let status_id = StatusId(path.status_id);
    match roosty_db::find_local_status_by_id(&state.db, status_id).await {
        Ok(Some(status)) => match status_visible_to_viewer(&state, &status, viewer_id).await {
            Ok(true) => {}
            Ok(false) => return not_found(),
            Err(error) => return server_error(error),
        },
        Ok(None) => return not_found(),
        Err(error) => return server_error(error),
    }

    let limit = timeline_limit(params.limit);
    let cursor = match collection_cursor(&params) {
        Ok(cursor) => cursor,
        Err(()) => return bad_request("collection cursor is invalid"),
    };
    match roosty_db::reblogged_by_for_status(&state.db, status_id, limit, cursor).await {
        Ok(page) => {
            reblogged_by_response(
                &state,
                page,
                limit,
                &format!("/api/v1/statuses/{}/reblogged_by", path.status_id),
            )
            .await
        }
        Err(error) => server_error(error),
    }
}

async fn favourites(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Query(params): Query<CollectionParams>,
) -> Response {
    status_collection_list(&state, account.id, params, StatusCollectionList::Favourites).await
}

async fn bookmarks(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Query(params): Query<CollectionParams>,
) -> Response {
    status_collection_list(&state, account.id, params, StatusCollectionList::Bookmarks).await
}

async fn home_timeline(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Query(params): Query<TimelineParams>,
) -> Response {
    let query = match timeline_query(params) {
        Ok(query) => query,
        Err(error) => return bad_request(&error.to_string()),
    };
    match roosty_db::home_timeline_for_account(&state.db, account.id, query.limit, query.cursor)
        .await
    {
        Ok(items) => {
            home_timeline_response(
                &state,
                items,
                query.limit,
                "/api/v1/timelines/home",
                account.id,
            )
            .await
        }
        Err(error) => server_error(error),
    }
}

async fn public_timeline(
    State(state): State<AppState>,
    OptionalAuthenticatedAccount(viewer): OptionalAuthenticatedAccount,
    Query(params): Query<TimelineParams>,
) -> Response {
    let query = match timeline_query(params) {
        Ok(query) => query,
        Err(error) => return bad_request(&error.to_string()),
    };
    match roosty_db::public_local_timeline(&state.db, query.limit, query.cursor).await {
        Ok(statuses) => {
            timeline_response(
                &state,
                statuses,
                query.limit,
                "/api/v1/timelines/public",
                viewer.as_ref().map(|account| account.id),
            )
            .await
        }
        Err(error) => server_error(error),
    }
}

async fn tag_timeline(
    State(state): State<AppState>,
    OptionalAuthenticatedAccount(viewer): OptionalAuthenticatedAccount,
    Path(path): Path<TagPath>,
    RawQuery(query): RawQuery,
) -> Response {
    let params = match tag_timeline_params(query.as_deref()) {
        Ok(params) => params,
        Err(()) => return bad_request("tag timeline query is invalid"),
    };
    let query = match timeline_query(TimelineParams {
        limit: params.limit,
        max_id: params.max_id,
        since_id: params.since_id,
        min_id: params.min_id,
    }) {
        Ok(query) => query,
        Err(error) => return bad_request(&error.to_string()),
    };
    if params.remote.unwrap_or(false) && !params.local.unwrap_or(false) {
        return timeline_response(
            &state,
            roosty_db::TimelinePage {
                items: Vec::new(),
                first_cursor: None,
                last_cursor: None,
                has_more: false,
            },
            query.limit,
            &format!("/api/v1/timelines/tag/{}", path.hashtag),
            viewer.as_ref().map(|account| account.id),
        )
        .await;
    }

    match roosty_db::local_tag_timeline(
        &state.db,
        &path.hashtag,
        roosty_db::LocalTagTimelineOptions {
            any: params.any,
            all: params.all,
            none: params.none,
            only_media: params.only_media.unwrap_or(false),
        },
        query.limit,
        query.cursor,
    )
    .await
    {
        Ok(statuses) => {
            timeline_response(
                &state,
                statuses,
                query.limit,
                &format!("/api/v1/timelines/tag/{}", path.hashtag),
                viewer.as_ref().map(|account| account.id),
            )
            .await
        }
        Err(error) => server_error(error),
    }
}

async fn show_tag(
    State(state): State<AppState>,
    OptionalAuthenticatedAccount(account): OptionalAuthenticatedAccount,
    Path(path): Path<TagPath>,
) -> Response {
    match tag_response_by_name(
        &state,
        &path.hashtag,
        account.as_ref().map(|account| account.id),
    )
    .await
    {
        Ok(Some(tag)) => Json(tag).into_response(),
        Ok(None) => tag_not_found(),
        Err(error) => server_error(error),
    }
}

async fn follow_tag(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<TagPath>,
) -> Response {
    match roosty_db::follow_local_tag(&state.db, account.id, &path.hashtag).await {
        Ok(tag) => tag_response(&state, tag, Some(true)).await,
        Err(error) => server_error(error),
    }
}

async fn unfollow_tag(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<TagPath>,
) -> Response {
    match roosty_db::unfollow_local_tag(&state.db, account.id, &path.hashtag).await {
        Ok(Some(tag)) => tag_response(&state, tag, Some(false)).await,
        Ok(None) => tag_not_found(),
        Err(error) => server_error(error),
    }
}

/// Build a Mastodon tag response for one locally known hashtag.
async fn tag_response_by_name(
    state: &AppState,
    name: &str,
    viewer: Option<AccountId>,
) -> Result<Option<TagResponse>, RoostyError> {
    let Some(tag) = roosty_db::find_local_tag_by_name(&state.db, name).await? else {
        return Ok(None);
    };
    let following = match viewer {
        Some(account_id) => {
            Some(roosty_db::is_local_tag_followed(&state.db, account_id, tag.id).await?)
        }
        None => None,
    };

    Ok(Some(tag_response_model(state, tag, following).await?))
}

/// Convert stored local tag metadata into a Mastodon tag response.
pub(crate) async fn tag_response_model(
    state: &AppState,
    tag: roosty_db::LocalTag,
    following: Option<bool>,
) -> Result<TagResponse, RoostyError> {
    let history = roosty_db::local_tag_history(&state.db, tag.id).await?;
    Ok(TagResponse::new(state, tag, history, following))
}

async fn tag_response(
    state: &AppState,
    tag: roosty_db::LocalTag,
    following: Option<bool>,
) -> Response {
    match tag_response_model(state, tag, following).await {
        Ok(tag) => Json(tag).into_response(),
        Err(error) => server_error(error),
    }
}

fn tag_timeline_params(query: Option<&str>) -> Result<TagTimelineParams, ()> {
    let Some(query) = query else {
        return Ok(TagTimelineParams::default());
    };

    serde_qs::Config::new()
        .array_format(serde_qs::ArrayFormat::EmptyIndexed)
        .use_form_encoding(true)
        .deserialize_str(query)
        .map_err(|_| ())
}

/// Parse either JSON or form-encoded Mastodon status creation input.
async fn parse_status_input(
    request: axum::extract::Request,
) -> Result<StatusInput, StatusInputError> {
    let content_type = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let body = to_bytes(request.into_body(), 1024 * 1024)
        .await
        .map_err(|_| StatusInputError::Empty)?;

    let input: StatusInput = if content_type.contains("application/json") {
        serde_json::from_slice(&body).map_err(StatusInputError::Json)?
    } else {
        let body = String::from_utf8_lossy(&body);
        serde_qs::Config::new()
            .array_format(serde_qs::ArrayFormat::EmptyIndexed)
            .use_form_encoding(true)
            .deserialize_str(&body)
            .map_err(|error| StatusInputError::Form(error.to_string()))?
    };

    Ok(input)
}

/// Validate status text against the current local posting policy.
fn validate_status_text(status: &str, has_media: bool) -> Result<(), StatusInputError> {
    let trimmed = status.trim();
    if trimmed.is_empty() && !has_media {
        return Err(StatusInputError::Empty);
    }
    if trimmed.chars().count() > MAX_STATUS_CHARS {
        return Err(StatusInputError::TooLong);
    }
    Ok(())
}

/// Attach newly created direct statuses to a local Mastodon conversation.
async fn attach_direct_conversation(
    state: &AppState,
    txn: &DatabaseTransaction,
    status: &mut roosty_db::LocalStatus,
    author_id: AccountId,
) -> Result<(), RoostyError> {
    if status.visibility != StatusVisibility::Direct {
        return Ok(());
    }

    let mut participant_ids = local_text_mentions(state, &status.content)
        .await?
        .into_iter()
        .map(|account| account.id)
        .collect::<Vec<_>>();
    participant_ids.sort_by_key(|account_id| account_id.0);
    participant_ids.dedup();

    let remote_participants = roosty_db::remote_mentions_for_local_status(txn, status.id)
        .await?
        .into_iter()
        .map(|actor| RemoteConversationParticipant {
            activitypub_id: actor.activitypub_id,
            remote_actor_id: Some(actor.id),
            mention_name: Some(format!("@{}@{}", actor.username, actor.domain)),
        })
        .collect::<Vec<_>>();

    let conversation_id = roosty_db::attach_direct_status_to_conversation(
        txn,
        status.id,
        author_id,
        status.in_reply_to_id,
        status.in_reply_to_remote_status_id,
        &participant_ids,
        &remote_participants,
    )
    .await?;
    status.conversation_id = Some(conversation_id);

    Ok(())
}

/// Add recipients introduced by a direct-status edit without promoting an historical edit
/// over a newer visible status in any existing account's conversation view.
async fn sync_edited_direct_conversation(
    state: &AppState,
    txn: &DatabaseTransaction,
    status: &roosty_db::LocalStatus,
) -> Result<(), RoostyError> {
    let mut participant_ids = local_text_mentions(state, &status.content)
        .await?
        .into_iter()
        .map(|account| account.id)
        .collect::<Vec<_>>();
    participant_ids.sort_by_key(|account_id| account_id.0);
    participant_ids.dedup();
    let remote_participants = roosty_db::remote_mentions_for_local_status(txn, status.id)
        .await?
        .into_iter()
        .map(|actor| RemoteConversationParticipant {
            activitypub_id: actor.activitypub_id,
            remote_actor_id: Some(actor.id),
            mention_name: Some(format!("@{}@{}", actor.username, actor.domain)),
        })
        .collect::<Vec<_>>();
    roosty_db::sync_edited_direct_status_conversation(
        txn,
        status.id,
        status.account_id,
        &participant_ids,
        &remote_participants,
    )
    .await?;
    Ok(())
}

/// Parse media identifiers attached to a status creation request.
fn parse_media_ids(values: &[String]) -> Result<Vec<Uuid>, StatusInputError> {
    if values.len() > MAX_MEDIA_ATTACHMENTS as usize {
        return Err(StatusInputError::TooManyMedia);
    }
    let mut seen = HashSet::new();
    let mut media_ids = Vec::with_capacity(values.len());
    for value in values {
        let media_id = value
            .trim()
            .parse::<Uuid>()
            .map_err(|_| StatusInputError::MediaId)?;
        if !seen.insert(media_id) {
            return Err(StatusInputError::MediaId);
        }
        media_ids.push(media_id);
    }
    Ok(media_ids)
}

/// Parse media metadata updates accepted by Mastodon status edit requests.
fn parse_media_attributes(
    values: &[MediaAttributeInput],
) -> Result<Vec<roosty_db::LocalStatusMediaAttributeUpdate>, StatusInputError> {
    let mut seen = HashSet::new();
    let mut attributes = Vec::with_capacity(values.len());
    for value in values {
        let media_id = value
            .id
            .trim()
            .parse::<Uuid>()
            .map_err(|_| StatusInputError::MediaAttribute)?;
        if !seen.insert(media_id) {
            return Err(StatusInputError::MediaAttribute);
        }
        let description = match &value.description {
            Some(description) => Some(
                normalize_media_description(Some(description.clone()))
                    .map_err(|_| StatusInputError::MediaAttribute)?,
            ),
            None => None,
        };
        let focus = parse_media_focus(value.focus.as_deref())
            .map_err(|_| StatusInputError::MediaAttribute)?;
        attributes.push(roosty_db::LocalStatusMediaAttributeUpdate {
            media_id,
            description,
            focus,
        });
    }

    Ok(attributes)
}

/// Normalize media alt text sent through status edit media attributes.
fn normalize_media_description(value: Option<String>) -> Result<Option<String>, ()> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.chars().count() > 1500 {
        return Err(());
    }
    let value = value.trim().to_owned();
    Ok((!value.is_empty()).then_some(value))
}

/// Parse Mastodon's media focus field from status edit media attributes.
fn parse_media_focus(value: Option<&str>) -> Result<Option<(f64, f64)>, ()> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let Some((x, y)) = value.split_once(',') else {
        return Err(());
    };
    let x = x.trim().parse::<f64>().map_err(|_| ())?;
    let y = y.trim().parse::<f64>().map_err(|_| ())?;
    if (-1.0..=1.0).contains(&x) && (-1.0..=1.0).contains(&y) {
        Ok(Some((x, y)))
    } else {
        Err(())
    }
}

/// Parse an optional UUID status id from Mastodon form or JSON input.
fn parse_optional_status_id(value: Option<&str>) -> Result<Option<StatusId>, StatusInputError> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .parse()
                .map(StatusId)
                .map_err(|_| StatusInputError::StatusId)
        })
        .transpose()
}

async fn statuses_response(
    state: &AppState,
    statuses: Vec<roosty_db::LocalStatus>,
    viewer: Option<AccountId>,
) -> Response {
    match status_models(state, statuses, viewer).await {
        Ok(statuses) => Json(statuses).into_response(),
        Err(error) => server_error(error),
    }
}

/// Persist a remote favourite and its federated delivery job atomically.
async fn favourite_remote_status(
    state: &AppState,
    account_id: AccountId,
    status: &RemoteStatus,
) -> roosty_core::Result<()> {
    let txn = state.db.begin().await?;
    if roosty_db::is_remote_status_favourited(&txn, account_id, status.id).await? {
        txn.commit().await?;
        return Ok(());
    }

    let (activity_id, job) = prepare_remote_favourite(state, &txn, account_id, status).await?;
    roosty_db::favourite_remote_status_with_job(&txn, account_id, status.id, &activity_id, job)
        .await?;
    txn.commit().await?;
    Ok(())
}

/// Apply a local status collection mutation and return the updated status.
async fn status_collection_action(
    state: &AppState,
    account_id: AccountId,
    path: StatusPath,
    action: StatusCollectionAction,
) -> Response {
    let status_id = StatusId(path.status_id);
    let status = match visible_status_for_account(state, status_id, account_id).await {
        Ok(Some(status)) => status,
        Ok(None) => {
            let remote = match roosty_db::find_remote_status_by_id(&state.db, status_id).await {
                Ok(Some(status)) => match roosty_db::remote_status_visible_to_account(
                    &state.db, &status, account_id,
                )
                .await
                {
                    Ok(true) => status,
                    Ok(false) => return not_found(),
                    Err(error) => return server_error(error),
                },
                Ok(None) => return not_found(),
                Err(error) => return server_error(error),
            };
            if remote.visibility == StatusVisibility::Private
                && matches!(
                    action,
                    StatusCollectionAction::Reblog | StatusCollectionAction::Unreblog
                )
            {
                return bad_request("private statuses cannot be boosted");
            }
            return match action {
                StatusCollectionAction::Favourite => {
                    let result = favourite_remote_status(state, account_id, &remote).await;
                    match result {
                        Ok(()) => {
                            match remote_status_response_for_viewer(state, remote, Some(account_id))
                                .await
                            {
                                Ok(response) => Json(response).into_response(),
                                Err(error) => server_error(error),
                            }
                        }
                        Err(error) => server_error(error),
                    }
                }
                StatusCollectionAction::Unfavourite => {
                    let favourite = match roosty_db::find_remote_status_favourite(
                        &state.db, account_id, remote.id,
                    )
                    .await
                    {
                        Ok(favourite) => favourite,
                        Err(error) => return server_error(error),
                    };
                    match favourite {
                        Some(favourite) => {
                            let job = match prepare_remote_unfavourite(state, favourite).await {
                                Ok(job) => job,
                                Err(error) => return server_error(error),
                            };
                            let txn = match state.db.begin().await {
                                Ok(txn) => txn,
                                Err(error) => return server_error(error.into()),
                            };
                            match roosty_db::unfavourite_remote_status_with_job(
                                &txn, account_id, remote.id, job,
                            )
                            .await
                            {
                                Ok(Some(_)) | Ok(None) => match txn.commit().await {
                                    Ok(()) => {}
                                    Err(error) => return server_error(error.into()),
                                },
                                Err(error) => return server_error(error),
                            }
                            match remote_status_response_for_viewer(state, remote, Some(account_id))
                                .await
                            {
                                Ok(response) => Json(response).into_response(),
                                Err(error) => server_error(error),
                            }
                        }
                        None => {
                            match remote_status_response_for_viewer(state, remote, Some(account_id))
                                .await
                            {
                                Ok(response) => Json(response).into_response(),
                                Err(error) => server_error(error),
                            }
                        }
                    }
                }
                StatusCollectionAction::Reblog => {
                    let already_reblogged = match roosty_db::is_remote_status_reblogged(
                        &state.db, account_id, remote.id,
                    )
                    .await
                    {
                        Ok(value) => value,
                        Err(error) => return server_error(error),
                    };
                    let reblog = if already_reblogged {
                        match roosty_db::reblog_remote_status(&state.db, account_id, remote.id, "")
                            .await
                        {
                            Ok(reblog) => reblog,
                            Err(error) => return server_error(error),
                        }
                    } else {
                        let (activity_id, job) =
                            match prepare_remote_reblog(state, account_id, &remote).await {
                                Ok(id) => id,
                                Err(error) => return server_error(error),
                            };
                        let txn = match state.db.begin().await {
                            Ok(txn) => txn,
                            Err(error) => return server_error(error.into()),
                        };
                        match roosty_db::reblog_remote_status_with_job(
                            &txn,
                            account_id,
                            remote.id,
                            &activity_id,
                            job,
                        )
                        .await
                        {
                            Ok(reblog) => match txn.commit().await {
                                Ok(()) => reblog,
                                Err(error) => return server_error(error.into()),
                            },
                            Err(error) => return server_error(error),
                        }
                    };
                    match local_remote_reblog_response(state, reblog, Some(account_id)).await {
                        Ok(Some(response)) => {
                            let recipients = reblog_stream_recipients(state, account_id).await;
                            state.streaming_events.publish_status_update(
                                &response,
                                account_id,
                                &response.visibility,
                                &recipients,
                            );
                            Json(response).into_response()
                        }
                        Ok(None) => not_found(),
                        Err(error) => server_error(error),
                    }
                }
                StatusCollectionAction::Unreblog => {
                    let reblog = match roosty_db::find_remote_status_reblog(
                        &state.db, account_id, remote.id,
                    )
                    .await
                    {
                        Ok(reblog) => reblog,
                        Err(error) => return server_error(error),
                    };
                    if let Some(reblog) = reblog {
                        let reblog_id = reblog.id;
                        let job = match prepare_remote_unreblog(state, reblog).await {
                            Ok(job) => job,
                            Err(error) => return server_error(error),
                        };
                        let txn = match state.db.begin().await {
                            Ok(txn) => txn,
                            Err(error) => return server_error(error.into()),
                        };
                        match roosty_db::unreblog_remote_status_with_job(
                            &txn, account_id, remote.id, job,
                        )
                        .await
                        {
                            Ok(Some(_)) | Ok(None) => match txn.commit().await {
                                Ok(()) => {}
                                Err(error) => return server_error(error.into()),
                            },
                            Err(error) => return server_error(error),
                        }
                        let recipients = reblog_stream_recipients(state, account_id).await;
                        state.streaming_events.publish_delete(
                            &reblog_id.to_string(),
                            account_id,
                            "unlisted",
                            &recipients,
                        );
                    }
                    match remote_status_response_for_viewer(state, remote, Some(account_id)).await {
                        Ok(response) => Json(response).into_response(),
                        Err(error) => server_error(error),
                    }
                }
                _ => not_found(),
            };
        }
        Err(error) => return server_error(error),
    };

    if status.visibility == StatusVisibility::Private
        && status.account_id != account_id
        && matches!(
            action,
            StatusCollectionAction::Reblog | StatusCollectionAction::Unreblog
        )
    {
        return bad_request("private statuses cannot be boosted");
    }

    let reblog = if matches!(action, StatusCollectionAction::Reblog) {
        match roosty_db::reblog_local_status(&state.db, account_id, status_id).await {
            Ok(reblog) => Some(reblog),
            Err(error) => return server_error(error),
        }
    } else {
        None
    };
    let removed_reblog = if matches!(action, StatusCollectionAction::Unreblog) {
        match roosty_db::unreblog_local_status(&state.db, account_id, status_id).await {
            Ok(reblog) => reblog,
            Err(error) => return server_error(error),
        }
    } else {
        None
    };
    let result = match action {
        StatusCollectionAction::Favourite => {
            roosty_db::favourite_local_status(&state.db, account_id, status_id).await
        }
        StatusCollectionAction::Unfavourite => {
            roosty_db::unfavourite_local_status(&state.db, account_id, status_id).await
        }
        StatusCollectionAction::Bookmark => {
            roosty_db::bookmark_local_status(&state.db, account_id, status_id).await
        }
        StatusCollectionAction::Unbookmark => {
            roosty_db::unbookmark_local_status(&state.db, account_id, status_id).await
        }
        StatusCollectionAction::Reblog => Ok(()),
        StatusCollectionAction::Unreblog => Ok(()),
    };

    match result {
        Ok(()) => {
            if matches!(action, StatusCollectionAction::Favourite)
                && status.account_id != account_id
                && let Err(error) = create_and_stream_notification(
                    state,
                    status.account_id,
                    LocalNotificationType::Favourite,
                    account_id,
                    Some(status.id),
                )
                .await
            {
                warn!(%error, "failed to create favourite notification");
            }
            if matches!(action, StatusCollectionAction::Reblog) {
                return match reblog {
                    Some(reblog) => {
                        if status.account_id != account_id
                            && let Err(error) = create_and_stream_notification(
                                state,
                                status.account_id,
                                LocalNotificationType::Reblog,
                                account_id,
                                Some(status.id),
                            )
                            .await
                        {
                            warn!(%error, "failed to create reblog notification");
                        }
                        match reblog_response(state, reblog, Some(account_id)).await {
                            Ok(Some(response)) => {
                                let recipients = reblog_stream_recipients(state, account_id).await;
                                state.streaming_events.publish_status_update(
                                    &response,
                                    account_id,
                                    &response.visibility,
                                    &recipients,
                                );
                                Json(response).into_response()
                            }
                            Ok(None) => not_found(),
                            Err(error) => server_error(error),
                        }
                    }
                    None => server_error(RoostyError::InvalidInput(
                        "boost was not created".to_owned(),
                    )),
                };
            }
            if let Some(removed_reblog) = removed_reblog {
                let recipients = reblog_stream_recipients(state, account_id).await;
                state.streaming_events.publish_delete(
                    &removed_reblog.id.to_string(),
                    account_id,
                    "direct",
                    &recipients,
                );
            }
            status_with_author_response(state, status, Some(account_id)).await
        }
        Err(error) => server_error(error),
    }
}

/// Return followers that should receive this status in their home stream.
async fn status_stream_recipients(
    state: &AppState,
    status: &roosty_db::LocalStatus,
) -> Vec<AccountId> {
    if status.visibility == StatusVisibility::Direct {
        return Vec::new();
    }
    match roosty_db::local_follower_ids_for_account(&state.db, status.account_id, true).await {
        Ok(mut recipients) => {
            if status.visibility == StatusVisibility::Public {
                match roosty_db::local_tag_follower_ids_for_status(&state.db, status.id).await {
                    Ok(tag_followers) => recipients.extend(tag_followers),
                    Err(error) => warn!(%error, "failed to resolve followed-tag stream recipients"),
                }
            }
            if status.visibility == StatusVisibility::Private {
                match roosty_db::local_status_local_recipients(&state.db, status.id).await {
                    Ok(explicit) => recipients.extend(explicit),
                    Err(error) => warn!(%error, "failed to resolve explicit status recipients"),
                }
            }
            recipients.sort_by_key(|id| id.0);
            recipients.dedup();
            filter_stream_recipients(state, status.account_id, recipients).await
        }
        Err(error) => {
            warn!(%error, "failed to resolve status stream recipients");
            Vec::new()
        }
    }
}

/// Return followers that should receive this account's boost in their home stream.
async fn reblog_stream_recipients(state: &AppState, account_id: AccountId) -> Vec<AccountId> {
    match roosty_db::local_follower_ids_for_account(&state.db, account_id, false).await {
        Ok(recipients) => filter_stream_recipients(state, account_id, recipients).await,
        Err(error) => {
            warn!(%error, "failed to resolve reblog stream recipients");
            Vec::new()
        }
    }
}

/// Remove followers who have muted or blocked the account producing a stream event.
async fn filter_stream_recipients(
    state: &AppState,
    author_id: AccountId,
    recipients: Vec<AccountId>,
) -> Vec<AccountId> {
    let mut visible = Vec::with_capacity(recipients.len());
    for recipient in recipients {
        match roosty_db::local_account_is_hidden_for_viewer(&state.db, recipient, author_id).await {
            Ok(false) => visible.push(recipient),
            Ok(true) => {}
            Err(error) => warn!(%error, "failed to filter muted or blocked stream recipient"),
        }
    }

    visible
}

/// Remove viewers who mute or block the remote actor producing a stream event.
async fn filter_remote_stream_recipients(
    state: &AppState,
    actor_id: AccountId,
    recipients: Vec<AccountId>,
) -> Vec<AccountId> {
    let mut visible = Vec::with_capacity(recipients.len());
    for recipient in recipients {
        match roosty_db::remote_account_is_hidden_for_viewer(&state.db, recipient, actor_id).await {
            Ok(false) => visible.push(recipient),
            Ok(true) => {}
            Err(error) => warn!(%error, "failed to filter moderated remote stream recipient"),
        }
    }
    visible
}

/// Return mixed local and remote boost actors with Mastodon cursor pagination.
async fn reblogged_by_response(
    state: &AppState,
    page: roosty_db::CollectionPage<roosty_db::RebloggedByAccount>,
    limit: u64,
    path: &str,
) -> Response {
    let link_header = CollectionLink::new(
        limit,
        page.first_cursor,
        page.last_cursor,
        page.has_more,
        path,
    )
    .header_value();
    let mut accounts = Vec::with_capacity(page.items.len());
    for account in page.items {
        match account {
            roosty_db::RebloggedByAccount::Local(account) => {
                match account_response(state, account).await {
                    Ok(account) => accounts.push(StatusAccountResponse::Local(Box::new(account))),
                    Err(error) => return server_error(error),
                }
            }
            roosty_db::RebloggedByAccount::Remote(actor) => {
                match remote_account_response(state, actor).await {
                    Ok(account) => accounts.push(StatusAccountResponse::Remote(Box::new(account))),
                    Err(error) => return server_error(error),
                }
            }
        }
    }
    let mut response = Json(accounts).into_response();
    if let Some(link_header) = link_header {
        response.headers_mut().insert(header::LINK, link_header);
    }
    response
}

/// Return a local status collection for an authenticated account.
async fn status_collection_list(
    state: &AppState,
    account_id: AccountId,
    params: CollectionParams,
    collection: StatusCollectionList,
) -> Response {
    let limit = timeline_limit(params.limit);
    let cursor = match collection_cursor(&params) {
        Ok(cursor) => cursor,
        Err(()) => return bad_request("collection cursor is invalid"),
    };
    let result = match collection {
        StatusCollectionList::Favourites => {
            return favourites_response(state, account_id, limit, cursor).await;
        }
        StatusCollectionList::Bookmarks => {
            roosty_db::local_bookmarks_for_account(&state.db, account_id, limit, cursor).await
        }
    };

    match result {
        Ok(page) => {
            let path = match collection {
                StatusCollectionList::Favourites => "/api/v1/favourites",
                StatusCollectionList::Bookmarks => "/api/v1/bookmarks",
            };
            let link_header = CollectionLink::new(
                limit,
                page.first_cursor,
                page.last_cursor,
                page.has_more,
                path,
            )
            .header_value();
            let mut response = statuses_response(state, page.items, Some(account_id)).await;
            if let Some(link_header) = link_header {
                response.headers_mut().insert(header::LINK, link_header);
            }
            response
        }
        Err(error) => server_error(error),
    }
}

/// Return the authenticated user's mixed local and cached-remote favourites collection.
async fn favourites_response(
    state: &AppState,
    account_id: AccountId,
    limit: u64,
    cursor: roosty_db::CollectionCursor,
) -> Response {
    match roosty_db::favourites_for_account(&state.db, account_id, limit, cursor).await {
        Ok(page) => {
            let link_header = CollectionLink::new(
                limit,
                page.first_cursor,
                page.last_cursor,
                page.has_more,
                "/api/v1/favourites",
            )
            .header_value();
            let mut responses = Vec::with_capacity(page.items.len());
            for status in page.items {
                match favourite_status_response(state, status, account_id).await {
                    Ok(Some(response)) => responses.push(response),
                    Ok(None) => {}
                    Err(error) => return server_error(error),
                }
            }
            let mut response = Json(responses).into_response();
            if let Some(link_header) = link_header {
                response.headers_mut().insert(header::LINK, link_header);
            }
            response
        }
        Err(error) => server_error(error),
    }
}

async fn favourite_status_response(
    state: &AppState,
    status: roosty_db::FavouriteStatus,
    viewer: AccountId,
) -> Result<Option<StatusResponse>, RoostyError> {
    match status {
        roosty_db::FavouriteStatus::Local(status) => {
            if !status_visible_to_viewer(state, &status, Some(viewer)).await? {
                return Ok(None);
            }
            Ok(Some(status_with_author(state, status, Some(viewer)).await?))
        }
        roosty_db::FavouriteStatus::Remote(status) => {
            if !roosty_db::remote_status_visible_to_account(&state.db, &status, viewer).await? {
                return Ok(None);
            }
            Ok(Some(
                remote_status_response_for_viewer(state, status, Some(viewer)).await?,
            ))
        }
    }
}

async fn status_models(
    state: &AppState,
    statuses: Vec<roosty_db::LocalStatus>,
    viewer: Option<AccountId>,
) -> Result<Vec<StatusResponse>, RoostyError> {
    let mut response = Vec::with_capacity(statuses.len());
    for status in statuses {
        if status_visible_to_viewer(state, &status, viewer).await? {
            response.push(status_with_author(state, status, viewer).await?);
        }
    }

    Ok(response)
}

async fn home_timeline_models(
    state: &AppState,
    items: Vec<roosty_db::HomeTimelineItem>,
    viewer: AccountId,
) -> Result<Vec<StatusResponse>, RoostyError> {
    let mut response = Vec::with_capacity(items.len());
    for item in items {
        match item {
            roosty_db::HomeTimelineItem::Status(status) => {
                response.push(status_with_author(state, status, Some(viewer)).await?);
            }
            roosty_db::HomeTimelineItem::Reblog(reblog) => {
                if let Some(reblog) = reblog_response(state, reblog, Some(viewer)).await? {
                    response.push(reblog);
                }
            }
            roosty_db::HomeTimelineItem::RemoteStatus(status) => {
                if !remote_status_available(state, &status).await? {
                    continue;
                }
                response
                    .push(remote_status_response_for_viewer(state, status, Some(viewer)).await?);
            }
            roosty_db::HomeTimelineItem::LocalRemoteReblog(reblog) => {
                let Some(original) =
                    roosty_db::find_remote_status_by_id(&state.db, reblog.remote_status_id).await?
                else {
                    continue;
                };
                if !remote_status_available(state, &original).await?
                    || roosty_db::remote_account_is_hidden_for_viewer(
                        &state.db,
                        viewer,
                        original.remote_actor_id,
                    )
                    .await?
                {
                    continue;
                }
                if let Some(reblog_response) =
                    local_remote_reblog_response(state, reblog, Some(viewer)).await?
                {
                    response.push(reblog_response);
                }
            }
            roosty_db::HomeTimelineItem::RemoteReblog(reblog) => {
                if let Some(reblog_response) =
                    remote_reblog_response(state, reblog, Some(viewer)).await?
                {
                    response.push(reblog_response);
                }
            }
        }
    }

    Ok(response)
}

/// Build a Mastodon home timeline response from statuses and boosts.
async fn home_timeline_response(
    state: &AppState,
    page: roosty_db::TimelinePage<roosty_db::HomeTimelineItem>,
    limit: u64,
    path: &str,
    viewer: AccountId,
) -> Response {
    let link_header = home_timeline_link_header(&page, limit, path);
    let mut response = match home_timeline_models(state, page.items, viewer).await {
        Ok(items) => Json(items).into_response(),
        Err(error) => return server_error(error),
    };
    if let Some(link_header) = link_header {
        response.headers_mut().insert(header::LINK, link_header);
    }
    response
}

/// Build a Mastodon timeline response from local statuses and optional viewer state.
pub(crate) async fn timeline_response(
    state: &AppState,
    page: roosty_db::TimelinePage<roosty_db::LocalStatus>,
    limit: u64,
    path: &str,
    viewer: Option<AccountId>,
) -> Response {
    let link_header = timeline_link_header(&page, limit, path);
    let mut response = statuses_response(state, page.items, viewer).await;
    if let Some(link_header) = link_header {
        response.headers_mut().insert(header::LINK, link_header);
    }
    response
}

/// Build a Mastodon timeline response from cached remote statuses and viewer state.
pub(crate) async fn remote_timeline_response(
    state: &AppState,
    page: roosty_db::TimelinePage<roosty_db::RemoteStatus>,
    limit: u64,
    path: &str,
    viewer: Option<AccountId>,
) -> Response {
    let link_header = timeline_link_header(&page, limit, path);
    let mut items = Vec::with_capacity(page.items.len());
    for status in page.items {
        let visible = match viewer {
            Some(viewer) => {
                roosty_db::remote_status_visible_to_account(&state.db, &status, viewer).await
            }
            None => Ok(matches!(
                status.visibility,
                StatusVisibility::Public | StatusVisibility::Unlisted
            )),
        };
        match visible {
            Ok(true) => {}
            Ok(false) => continue,
            Err(error) => return server_error(error),
        }
        match remote_status_response_for_viewer(state, status, viewer).await {
            Ok(status) => items.push(status),
            Err(error) => return server_error(error),
        }
    }
    let mut response = Json(items).into_response();
    if let Some(link_header) = link_header {
        response.headers_mut().insert(header::LINK, link_header);
    }
    response
}

async fn status_with_author_response(
    state: &AppState,
    status: roosty_db::LocalStatus,
    viewer: Option<AccountId>,
) -> Response {
    match status_with_author(state, status, viewer).await {
        Ok(status) => Json(status).into_response(),
        Err(error) => server_error(error),
    }
}

pub(crate) async fn status_with_author(
    state: &AppState,
    status: roosty_db::LocalStatus,
    viewer: Option<AccountId>,
) -> Result<StatusResponse, RoostyError> {
    let account = roosty_db::find_local_account_by_id(&state.db, status.account_id)
        .await?
        .ok_or_else(|| RoostyError::InvalidInput("status author does not exist".to_owned()))?;

    status_response_for_viewer(state, status, account, viewer).await
}

async fn status_response(
    state: &AppState,
    status: roosty_db::LocalStatus,
    account: roosty_db::LocalAccount,
) -> Result<StatusResponse, RoostyError> {
    status_response_for_viewer(state, status, account.clone(), Some(account.id)).await
}

/// Build the limited Mastodon status projection supported for a cached remote Note.
pub(crate) async fn remote_status_response(
    state: &AppState,
    status: roosty_db::RemoteStatus,
) -> Result<StatusResponse, RoostyError> {
    remote_status_response_for_viewer(state, status, None).await
}

/// Build a Mastodon boost wrapper for an Announce received from a remote actor.
pub(crate) async fn remote_reblog_response(
    state: &AppState,
    reblog: roosty_db::RemoteStatusReblog,
    viewer: Option<AccountId>,
) -> Result<Option<StatusResponse>, RoostyError> {
    let actor = roosty_db::find_remote_actor_by_id(&state.db, reblog.remote_actor_id)
        .await?
        .ok_or_else(|| RoostyError::InvalidInput("remote boost actor does not exist".to_owned()))?;
    if state.config.federation_domain_is_blocked(&actor.domain) {
        return Ok(None);
    }
    if let Some(viewer) = viewer
        && roosty_db::remote_account_is_hidden_for_viewer(&state.db, viewer, reblog.remote_actor_id)
            .await?
    {
        return Ok(None);
    }
    let original = match reblog.target {
        roosty_db::RemoteStatusReblogTarget::Local(status_id) => {
            let Some(status) = roosty_db::find_local_status_by_id(&state.db, status_id).await?
            else {
                return Ok(None);
            };
            if !status_visible_to_viewer(state, &status, viewer).await? {
                return Ok(None);
            }
            Box::new(status_with_author(state, status, viewer).await?)
        }
        roosty_db::RemoteStatusReblogTarget::Remote(status_id) => {
            let Some(status) = roosty_db::find_remote_status_by_id(&state.db, status_id).await?
            else {
                return Ok(None);
            };
            let visible = match viewer {
                Some(viewer) => {
                    roosty_db::remote_status_visible_to_account(&state.db, &status, viewer).await?
                }
                None => matches!(
                    status.visibility,
                    StatusVisibility::Public | StatusVisibility::Unlisted
                ),
            };
            if !visible {
                return Ok(None);
            }
            Box::new(remote_status_response_for_viewer(state, status, viewer).await?)
        }
    };
    Ok(Some(StatusResponse {
        id: reblog.id.to_string(),
        created_at: format_timestamp(reblog.created_at),
        edited_at: None,
        in_reply_to_id: None,
        in_reply_to_account_id: None,
        sensitive: original.sensitive,
        spoiler_text: String::new(),
        visibility: original.visibility.clone(),
        language: None,
        uri: reblog.activity_id.clone(),
        url: reblog.activity_id,
        content: String::new(),
        account: StatusAccountResponse::Remote(Box::new(
            remote_account_response(state, actor).await?,
        )),
        media_attachments: Vec::new(),
        mentions: Vec::new(),
        tags: Vec::new(),
        emojis: Vec::new(),
        reblogs_count: 0,
        favourites_count: 0,
        replies_count: 0,
        favourited: false,
        reblogged: false,
        muted: false,
        bookmarked: false,
        pinned: false,
        reblog: Some(original),
        application: None,
    }))
}

/// Publish a remote actor's newly stored boost to accounts following that actor.
pub(crate) async fn publish_remote_reblog_update(
    state: &AppState,
    remote_actor_id: AccountId,
    activity_id: &str,
) -> Result<(), RoostyError> {
    let Some(reblog) = roosty_db::find_remote_status_reblog_by_activity_id(
        &state.db,
        remote_actor_id,
        activity_id,
    )
    .await?
    else {
        return Ok(());
    };
    let recipients =
        roosty_db::accepted_local_reblog_followers_of_remote_actor(&state.db, remote_actor_id)
            .await?;
    let recipients = filter_remote_stream_recipients(state, remote_actor_id, recipients).await;
    if let Some(response) =
        remote_reblog_response(state, reblog, recipients.first().copied()).await?
    {
        state
            .streaming_events
            .publish_home_update(&response, remote_actor_id, &recipients);
    }
    Ok(())
}

/// Publish deletion of a remote actor's undone boost to its local followers.
pub(crate) async fn publish_remote_reblog_delete(
    state: &AppState,
    remote_actor_id: AccountId,
    reblog_id: uuid::Uuid,
) -> Result<(), RoostyError> {
    let recipients =
        roosty_db::accepted_local_reblog_followers_of_remote_actor(&state.db, remote_actor_id)
            .await?;
    let recipients = filter_remote_stream_recipients(state, remote_actor_id, recipients).await;
    state.streaming_events.publish_home_delete(
        &reblog_id.to_string(),
        remote_actor_id,
        &recipients,
    );
    Ok(())
}

/// Build a cached remote Note projection with viewer-specific favourite state.
async fn remote_status_response_for_viewer(
    state: &AppState,
    status: roosty_db::RemoteStatus,
    viewer: Option<AccountId>,
) -> Result<StatusResponse, RoostyError> {
    let replies_count =
        roosty_db::count_status_context_replies(&state.db, StatusContextParent::Remote(status.id))
            .await?;
    let actor = roosty_db::find_remote_actor_by_id(&state.db, status.remote_actor_id)
        .await?
        .ok_or_else(|| {
            RoostyError::InvalidInput("remote status author does not exist".to_owned())
        })?;
    let mentions = remote_status_mentions(state, &status).await?;
    let (in_reply_to_id, in_reply_to_account_id) =
        if let Some(parent_id) = status.in_reply_to_local_status_id {
            match roosty_db::find_local_status_by_id(&state.db, parent_id).await? {
                Some(parent) => (
                    Some(parent.id.0.to_string()),
                    Some(parent.account_id.0.to_string()),
                ),
                None => (None, None),
            }
        } else if let Some(parent_id) = status.in_reply_to_remote_status_id {
            match roosty_db::find_remote_status_by_id(&state.db, parent_id).await? {
                Some(parent) => (
                    Some(parent.id.0.to_string()),
                    Some(parent.remote_actor_id.0.to_string()),
                ),
                None => (None, None),
            }
        } else {
            (None, None)
        };
    Ok(StatusResponse {
        id: status.id.0.to_string(),
        created_at: format_timestamp(status.published_at),
        edited_at: (status.updated_at != status.published_at)
            .then(|| format_timestamp(status.updated_at)),
        in_reply_to_id,
        in_reply_to_account_id,
        sensitive: false,
        spoiler_text: String::new(),
        visibility: status.visibility.to_string(),
        language: None,
        uri: status.activitypub_id.clone(),
        url: status.activitypub_id,
        content: status.content,
        account: StatusAccountResponse::Remote(Box::new(
            remote_account_response(state, actor).await?,
        )),
        media_attachments: roosty_db::remote_media_attachments_for_status(&state.db, status.id)
            .await?
            .into_iter()
            .map(|media| remote_media_attachment_response(state, media))
            .collect(),
        mentions,
        tags: remote_status_tags(&status.object),
        emojis: remote_custom_emojis(&status.object),
        reblogs_count: 0,
        favourites_count: 0,
        replies_count,
        favourited: match viewer {
            Some(account_id) => {
                roosty_db::is_remote_status_favourited(&state.db, account_id, status.id).await?
            }
            None => false,
        },
        reblogged: match viewer {
            Some(account_id) => {
                roosty_db::is_remote_status_reblogged(&state.db, account_id, status.id).await?
            }
            None => false,
        },
        muted: false,
        bookmarked: false,
        pinned: false,
        reblog: None,
        application: None,
    })
}

async fn remote_status_available(
    state: &AppState,
    status: &roosty_db::RemoteStatus,
) -> Result<bool, RoostyError> {
    Ok(
        roosty_db::find_remote_actor_by_id(&state.db, status.remote_actor_id)
            .await?
            .is_some_and(|actor| !state.config.federation_domain_is_blocked(&actor.domain)),
    )
}

/// Project cached ActivityPub Mention tags without resolving new remote identities.
async fn remote_status_mentions(
    state: &AppState,
    status: &roosty_db::RemoteStatus,
) -> Result<Vec<MentionResponse>, RoostyError> {
    let Some(tags) = status.object.get("tag").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let local_prefix = format!(
        "{}/users/",
        state.config.public_base_url.as_str().trim_end_matches('/')
    );
    let mut mentions = Vec::new();
    let mut seen = HashSet::new();
    for tag in tags {
        if tag.get("type").and_then(Value::as_str) != Some("Mention") {
            continue;
        }
        let Some(href) = tag.get("href").and_then(Value::as_str) else {
            continue;
        };
        if let Some(username) = href.strip_prefix(&local_prefix)
            && !username.contains('/')
            && let Some(account) =
                roosty_db::find_local_account_by_username(&state.db, username).await?
            && seen.insert(account.id)
        {
            mentions.push(MentionResponse::new(state, &account));
        } else if let Some(actor) =
            roosty_db::find_remote_actor_by_activitypub_id(&state.db, href).await?
            && seen.insert(actor.id)
        {
            mentions.push(MentionResponse::remote(&actor));
        }
    }
    Ok(mentions)
}

/// Project valid ActivityPub Hashtag tags without merging them into local tag state.
fn remote_status_tags(object: &Value) -> Vec<TagResponse> {
    let Some(tags) = object.get("tag").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut projected = Vec::new();
    let mut seen = HashSet::new();
    for tag in tags {
        let kind = tag.get("type").and_then(Value::as_str);
        if !matches!(
            kind,
            Some("Hashtag") | Some("https://www.w3.org/ns/activitystreams#Hashtag")
        ) {
            continue;
        }
        let Some(name) = tag.get("name").and_then(Value::as_str) else {
            continue;
        };
        let Some(name) = name.strip_prefix('#').filter(|name| !name.is_empty()) else {
            continue;
        };
        let Some(url) = tag
            .get("href")
            .and_then(Value::as_str)
            .filter(|url| url.starts_with("https://"))
        else {
            continue;
        };
        if seen.insert((name.to_owned(), url.to_owned())) {
            projected.push(TagResponse {
                id: name.to_owned(),
                name: name.to_owned(),
                url: url.to_owned(),
                history: Vec::new(),
                following: None,
            });
        }
    }
    projected
}

async fn reblog_response(
    state: &AppState,
    reblog: roosty_db::LocalStatusReblog,
    viewer: Option<AccountId>,
) -> Result<Option<StatusResponse>, RoostyError> {
    let Some(original) = roosty_db::find_local_status_by_id(&state.db, reblog.status_id).await?
    else {
        return Ok(None);
    };
    if !status_visible_to_viewer(state, &original, viewer).await? {
        return Ok(None);
    }
    let Some(account) = roosty_db::find_local_account_by_id(&state.db, reblog.account_id).await?
    else {
        return Ok(None);
    };
    let original = Box::new(status_with_author(state, original, viewer).await?);
    let url = public_url(
        state,
        &format!("@{}/reblogs/{}", account.username, reblog.id),
    );

    let reblogged_by_viewer = viewer.is_some_and(|viewer| viewer == reblog.account_id);
    let muted = match viewer {
        Some(viewer) => roosty_db::active_local_account_mute(&state.db, viewer, reblog.account_id)
            .await?
            .is_some(),
        None => false,
    };

    Ok(Some(StatusResponse {
        id: reblog.id.to_string(),
        created_at: format_timestamp(reblog.created_at),
        edited_at: None,
        in_reply_to_id: None,
        in_reply_to_account_id: None,
        sensitive: original.sensitive,
        spoiler_text: String::new(),
        visibility: original.visibility.clone(),
        language: None,
        uri: url.clone(),
        url,
        content: String::new(),
        account: StatusAccountResponse::Local(Box::new(account_response(state, account).await?)),
        media_attachments: Vec::new(),
        mentions: Vec::new(),
        tags: Vec::new(),
        emojis: Vec::new(),
        reblogs_count: 0,
        favourites_count: 0,
        replies_count: 0,
        favourited: false,
        reblogged: reblogged_by_viewer,
        muted,
        bookmarked: false,
        pinned: false,
        reblog: Some(original),
        application: None,
    }))
}

/// Build a Mastodon boost wrapper for a local account's Announce of a cached remote Note.
async fn local_remote_reblog_response(
    state: &AppState,
    reblog: roosty_db::LocalRemoteStatusReblog,
    viewer: Option<AccountId>,
) -> Result<Option<StatusResponse>, RoostyError> {
    let Some(original) =
        roosty_db::find_remote_status_by_id(&state.db, reblog.remote_status_id).await?
    else {
        return Ok(None);
    };
    let Some(account) =
        roosty_db::find_local_account_by_id(&state.db, reblog.local_account_id).await?
    else {
        return Ok(None);
    };
    let original = Box::new(remote_status_response_for_viewer(state, original, viewer).await?);
    let url = public_url(
        state,
        &format!("@{}/reblogs/{}", account.username, reblog.id),
    );
    Ok(Some(StatusResponse {
        id: reblog.id.to_string(),
        created_at: format_timestamp(reblog.created_at),
        edited_at: None,
        in_reply_to_id: None,
        in_reply_to_account_id: None,
        sensitive: original.sensitive,
        spoiler_text: String::new(),
        visibility: original.visibility.clone(),
        language: None,
        uri: url.clone(),
        url,
        content: String::new(),
        account: StatusAccountResponse::Local(Box::new(account_response(state, account).await?)),
        media_attachments: Vec::new(),
        mentions: Vec::new(),
        tags: Vec::new(),
        emojis: Vec::new(),
        reblogs_count: 0,
        favourites_count: 0,
        replies_count: 0,
        favourited: false,
        reblogged: viewer.is_some_and(|viewer| viewer == reblog.local_account_id),
        muted: false,
        bookmarked: false,
        pinned: false,
        reblog: Some(original),
        application: None,
    }))
}

async fn status_response_for_viewer(
    state: &AppState,
    status: roosty_db::LocalStatus,
    account: roosty_db::LocalAccount,
    viewer: Option<AccountId>,
) -> Result<StatusResponse, RoostyError> {
    let status_path = format!("@{}/{}", account.username, status.id.0);
    let url = public_url(state, &status_path);
    let reply_target = reply_target(state, status.in_reply_to_id).await?;
    let remote_reply_actor = match status.in_reply_to_remote_status_id {
        Some(parent_id) => match roosty_db::find_remote_status_by_id(&state.db, parent_id).await? {
            Some(parent) => {
                roosty_db::find_remote_actor_by_id(&state.db, parent.remote_actor_id).await?
            }
            None => None,
        },
        None => None,
    };
    let in_reply_to_id = status
        .in_reply_to_id
        .or(status.in_reply_to_remote_status_id)
        .map(|id| id.0.to_string());
    let in_reply_to_account_id = reply_target
        .as_ref()
        .map(|target| target.account_id.0.to_string())
        .or_else(|| {
            remote_reply_actor
                .as_ref()
                .map(|actor| actor.id.0.to_string())
        });
    let text_mentions = local_text_mentions(state, &status.content).await?;
    let remote_mentions = roosty_db::remote_mentions_for_local_status(&state.db, status.id).await?;
    let mut mentions = status_mentions(
        state,
        reply_target.as_ref(),
        &text_mentions,
        &remote_mentions,
    );
    if let Some(actor) = remote_reply_actor
        && !mentions
            .iter()
            .any(|mention| mention.id == actor.id.0.to_string())
    {
        mentions.insert(0, MentionResponse::remote(&actor));
    }
    let tags = status_tags(state, status.id).await?;
    let replies_count =
        roosty_db::count_status_context_replies(&state.db, StatusContextParent::Local(status.id))
            .await?;
    let reblogs_count = roosty_db::count_local_reblogs(&state.db, status.id).await?;
    let favourites_count = roosty_db::count_local_favourites(&state.db, status.id).await?;
    let favourited = match viewer {
        Some(account_id) => {
            roosty_db::is_local_status_favourited(&state.db, account_id, status.id).await?
        }
        None => false,
    };
    let bookmarked = match viewer {
        Some(account_id) => {
            roosty_db::is_local_status_bookmarked(&state.db, account_id, status.id).await?
        }
        None => false,
    };
    let reblogged = match viewer {
        Some(account_id) => {
            roosty_db::is_local_status_reblogged(&state.db, account_id, status.id).await?
        }
        None => false,
    };
    let muted = match viewer {
        Some(viewer) => roosty_db::active_local_account_mute(&state.db, viewer, status.account_id)
            .await?
            .is_some(),
        None => false,
    };
    let media_attachments = roosty_db::local_media_attachments_for_status(&state.db, status.id)
        .await?
        .iter()
        .map(|media| media_response(state, media))
        .collect();

    Ok(StatusResponse {
        id: status.id.0.to_string(),
        created_at: format_timestamp(status.created_at),
        edited_at: (status.updated_at != status.created_at)
            .then(|| format_timestamp(status.updated_at)),
        in_reply_to_id,
        in_reply_to_account_id,
        sensitive: status.sensitive,
        spoiler_text: status.spoiler_text,
        visibility: status.visibility.to_string(),
        language: status.language,
        uri: url.clone(),
        url,
        content: status_content_html_with_mentions_and_tags(
            state,
            &status.content,
            &text_mentions,
            &remote_mentions,
            &tags,
        ),
        account: StatusAccountResponse::Local(Box::new(account_response(state, account).await?)),
        media_attachments,
        mentions,
        tags,
        emojis: Vec::new(),
        reblogs_count,
        favourites_count,
        replies_count,
        favourited,
        reblogged,
        muted,
        bookmarked,
        pinned: false,
        reblog: None,
        application: None,
    })
}

/// Replace stored hashtag links for a local status based on its plain text content.
/// Load Mastodon tag responses attached to a local status.
async fn status_tags(
    state: &AppState,
    status_id: StatusId,
) -> Result<Vec<TagResponse>, RoostyError> {
    let tags = roosty_db::local_tags_for_status(&state.db, status_id).await?;
    let mut responses = Vec::with_capacity(tags.len());
    for tag in tags {
        let history = roosty_db::local_tag_history(&state.db, tag.id).await?;
        responses.push(TagResponse::new(state, tag, history, None));
    }

    Ok(responses)
}

/// Resolve local `@username` references present in status text.
async fn local_text_mentions(
    state: &AppState,
    content: &str,
) -> Result<Vec<roosty_db::LocalAccount>, RoostyError> {
    let mut accounts = Vec::new();
    let mut seen = HashSet::new();

    for username in mention_usernames(content) {
        if !seen.insert(username.clone()) {
            continue;
        }
        if let Some(account) =
            roosty_db::find_local_account_by_username(&state.db, &username).await?
        {
            accounts.push(account);
        }
    }

    Ok(accounts)
}

/// Build the combined Mastodon mentions array without duplicate accounts.
fn status_mentions(
    state: &AppState,
    reply_target: Option<&ReplyTarget>,
    text_mentions: &[roosty_db::LocalAccount],
    remote_mentions: &[roosty_db::RemoteActor],
) -> Vec<MentionResponse> {
    let mut mentions = Vec::new();
    let mut seen = HashSet::new();

    if let Some(target) = reply_target {
        seen.insert(target.account_id);
        mentions.push(MentionResponse::new(state, &target.account));
    }

    for account in text_mentions {
        if seen.insert(account.id) {
            mentions.push(MentionResponse::new(state, account));
        }
    }

    for actor in remote_mentions {
        if seen.insert(actor.id) {
            mentions.push(MentionResponse::remote(actor));
        }
    }

    mentions
}

/// Load the account targeted by a local reply, if the status is a reply.
async fn reply_target(
    state: &AppState,
    in_reply_to_id: Option<StatusId>,
) -> Result<Option<ReplyTarget>, RoostyError> {
    let Some(status_id) = in_reply_to_id else {
        return Ok(None);
    };
    let Some(parent) = roosty_db::find_local_status_by_id(&state.db, status_id).await? else {
        return Ok(None);
    };
    let account = roosty_db::find_local_account_by_id(&state.db, parent.account_id)
        .await?
        .ok_or_else(|| {
            RoostyError::InvalidInput("reply target author does not exist".to_owned())
        })?;

    Ok(Some(ReplyTarget {
        account_id: parent.account_id,
        account,
    }))
}

async fn visible_status_for_account(
    state: &AppState,
    status_id: StatusId,
    account_id: AccountId,
) -> Result<Option<roosty_db::LocalStatus>, RoostyError> {
    let status = roosty_db::find_local_status_by_id(&state.db, status_id).await?;
    match status {
        Some(status) if status_visible_to_viewer(state, &status, Some(account_id)).await? => {
            Ok(Some(status))
        }
        Some(_) | None => Ok(None),
    }
}

#[derive(Clone, Copy)]
struct StatusContextLimits {
    ancestors: usize,
    descendants: usize,
    descendants_depth: Option<usize>,
}

impl StatusContextLimits {
    fn for_viewer(viewer: Option<AccountId>) -> Self {
        if viewer.is_some() {
            Self {
                ancestors: AUTHENTICATED_CONTEXT_LIMIT,
                descendants: AUTHENTICATED_CONTEXT_LIMIT,
                descendants_depth: None,
            }
        } else {
            Self {
                ancestors: PUBLIC_CONTEXT_ANCESTORS_LIMIT,
                descendants: PUBLIC_CONTEXT_DESCENDANTS_LIMIT,
                descendants_depth: Some(PUBLIC_CONTEXT_DESCENDANTS_DEPTH),
            }
        }
    }
}

async fn find_status_context_item(
    db: &impl ConnectionTrait,
    status_id: StatusId,
) -> Result<Option<StatusContextItem>, RoostyError> {
    if let Some(status) = roosty_db::find_local_status_by_id(db, status_id).await? {
        return Ok(Some(StatusContextItem::Local(status)));
    }
    Ok(roosty_db::find_remote_status_by_id(db, status_id)
        .await?
        .map(StatusContextItem::Remote))
}

fn status_context_parent(item: &StatusContextItem) -> Option<StatusContextParent> {
    match item {
        StatusContextItem::Local(status) => status
            .in_reply_to_id
            .map(StatusContextParent::Local)
            .or_else(|| {
                status
                    .in_reply_to_remote_status_id
                    .map(StatusContextParent::Remote)
            }),
        StatusContextItem::Remote(status) => status
            .in_reply_to_local_status_id
            .map(StatusContextParent::Local)
            .or_else(|| {
                status
                    .in_reply_to_remote_status_id
                    .map(StatusContextParent::Remote)
            }),
    }
}

async fn find_status_context_parent(
    db: &impl ConnectionTrait,
    parent: StatusContextParent,
) -> Result<Option<StatusContextItem>, RoostyError> {
    match parent {
        StatusContextParent::Local(status_id) => {
            Ok(roosty_db::find_local_status_by_id(db, status_id)
                .await?
                .map(StatusContextItem::Local))
        }
        StatusContextParent::Remote(status_id) => {
            Ok(roosty_db::find_remote_status_by_id(db, status_id)
                .await?
                .map(StatusContextItem::Remote))
        }
    }
}

async fn status_context_item_visible(
    db: &impl ConnectionTrait,
    item: &StatusContextItem,
    viewer: Option<AccountId>,
) -> Result<bool, RoostyError> {
    match item {
        StatusContextItem::Local(status) => status_visible_to_viewer_on(db, status, viewer).await,
        StatusContextItem::Remote(status) => match viewer {
            Some(viewer) => roosty_db::remote_status_visible_to_account(db, status, viewer).await,
            None => Ok(matches!(
                status.visibility,
                StatusVisibility::Public | StatusVisibility::Unlisted
            )),
        },
    }
}

async fn status_context_models(
    state: &AppState,
    items: Vec<StatusContextItem>,
    viewer: Option<AccountId>,
) -> Result<Vec<StatusResponse>, RoostyError> {
    let mut responses = Vec::with_capacity(items.len());
    for item in items {
        responses.push(match item {
            StatusContextItem::Local(status) => status_with_author(state, status, viewer).await?,
            StatusContextItem::Remote(status) => {
                remote_status_response_for_viewer(state, status, viewer).await?
            }
        });
    }
    Ok(responses)
}

/// Walk visible cached parent statuses from root ancestor to direct parent.
async fn status_ancestors(
    db: &impl ConnectionTrait,
    status: &StatusContextItem,
    viewer: Option<AccountId>,
    limits: StatusContextLimits,
) -> Result<Vec<StatusContextItem>, RoostyError> {
    let mut ancestors = Vec::new();
    let mut seen = HashSet::new();
    let mut next = status_context_parent(status);

    while let Some(parent_id) = next {
        if ancestors.len() >= limits.ancestors || !seen.insert(parent_id) {
            break;
        }

        let Some(parent) = find_status_context_parent(db, parent_id).await? else {
            break;
        };
        if !status_context_item_visible(db, &parent, viewer).await? {
            break;
        }

        next = status_context_parent(&parent);
        ancestors.push(parent);
    }

    ancestors.reverse();
    Ok(ancestors)
}

/// Collect visible cached replies below a status in conversation order.
async fn status_descendants(
    db: &impl ConnectionTrait,
    status: &StatusContextItem,
    viewer: Option<AccountId>,
    limits: StatusContextLimits,
) -> Result<Vec<StatusContextItem>, RoostyError> {
    let mut descendants = Vec::new();
    let mut seen = HashSet::new();
    let root = match status {
        StatusContextItem::Local(status) => StatusContextParent::Local(status.id),
        StatusContextItem::Remote(status) => StatusContextParent::Remote(status.id),
    };
    let mut queue = VecDeque::from([(root, 0_usize)]);

    while let Some((parent_id, depth)) = queue.pop_front() {
        if !seen.insert(parent_id) {
            continue;
        }
        if limits
            .descendants_depth
            .is_some_and(|maximum| depth >= maximum)
        {
            continue;
        }

        let replies = roosty_db::status_context_replies(db, parent_id).await?;
        for reply in replies {
            if descendants.len() >= limits.descendants {
                return Ok(descendants);
            }
            let reply_id = match &reply {
                StatusContextItem::Local(status) => StatusContextParent::Local(status.id),
                StatusContextItem::Remote(status) => StatusContextParent::Remote(status.id),
            };
            if seen.contains(&reply_id) || !status_context_item_visible(db, &reply, viewer).await? {
                continue;
            }
            queue.push_back((reply_id, depth + 1));
            descendants.push(reply);
        }
    }

    Ok(descendants)
}

/// Clamp a Mastodon timeline limit to the local supported range.
pub(crate) fn timeline_limit(limit: Option<u64>) -> u64 {
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

fn timeline_query(params: TimelineParams) -> Result<TimelineQuery, StatusInputError> {
    Ok(TimelineQuery {
        limit: timeline_limit(params.limit),
        cursor: roosty_db::TimelineCursor {
            max_id: parse_optional_status_id(params.max_id.as_deref())?,
            since_id: parse_optional_status_id(params.since_id.as_deref())?,
            min_id: parse_optional_status_id(params.min_id.as_deref())?,
        },
    })
}

/// Parse Mastodon cursor parameters from a local collection request.
fn collection_cursor(params: &CollectionParams) -> Result<roosty_db::CollectionCursor, ()> {
    Ok(roosty_db::CollectionCursor {
        max_id: parse_optional_uuid(params.max_id.as_deref())?,
        since_id: parse_optional_uuid(params.since_id.as_deref())?,
        min_id: parse_optional_uuid(params.min_id.as_deref())?,
    })
}

fn timeline_link_header<T>(
    page: &roosty_db::TimelinePage<T>,
    limit: u64,
    path: &str,
) -> Option<HeaderValue> {
    if !page.has_more {
        return None;
    }
    let first = page.first_cursor?;
    let last = page.last_cursor?;
    let value = format!(
        r#"<{path}?limit={limit}&min_id={first}>; rel="prev", <{path}?limit={limit}&max_id={last}>; rel="next""#,
    );
    HeaderValue::from_str(&value).ok()
}

fn home_timeline_link_header(
    page: &roosty_db::TimelinePage<roosty_db::HomeTimelineItem>,
    limit: u64,
    path: &str,
) -> Option<HeaderValue> {
    if !page.has_more {
        return None;
    }
    let first = page.first_cursor?;
    let last = page.last_cursor?;
    let value = format!(
        r#"<{path}?limit={limit}&min_id={first}>; rel="prev", <{path}?limit={limit}&max_id={last}>; rel="next""#
    );
    HeaderValue::from_str(&value).ok()
}

/// Data needed to build a Mastodon collection pagination Link header.
pub(crate) struct CollectionLink<'a> {
    /// Effective clamped request limit.
    limit: u64,
    /// Opaque cursor for the first collection row returned.
    first_cursor: Option<Uuid>,
    /// Opaque cursor for the last collection row returned.
    last_cursor: Option<Uuid>,
    /// Whether another page may exist.
    has_more: bool,
    /// API path used to construct relative pagination links.
    path: &'a str,
}

impl<'a> CollectionLink<'a> {
    /// Create collection pagination metadata from a completed page.
    pub(crate) fn new(
        limit: u64,
        first_cursor: Option<Uuid>,
        last_cursor: Option<Uuid>,
        has_more: bool,
        path: &'a str,
    ) -> Self {
        CollectionLink {
            limit,
            first_cursor,
            last_cursor,
            has_more,
            path,
        }
    }

    /// Render the pagination Link header when the page may have more rows.
    pub(crate) fn header_value(&self) -> Option<HeaderValue> {
        if !self.has_more {
            return None;
        }
        let first_cursor = self.first_cursor?;
        let last_cursor = self.last_cursor?;
        let path = self.path;
        let limit = self.limit;
        let value = format!(
            r#"<{path}?limit={limit}&min_id={first_cursor}>; rel="prev", <{path}?limit={limit}&max_id={last_cursor}>; rel="next""#,
        );
        HeaderValue::from_str(&value).ok()
    }
}

/// Parse an optional UUID cursor from Mastodon collection query parameters.
fn parse_optional_uuid(value: Option<&str>) -> Result<Option<Uuid>, ()> {
    value.map(Uuid::parse_str).transpose().map_err(|_| ())
}

fn can_view_status(status: &roosty_db::LocalStatus, viewer: Option<AccountId>) -> bool {
    matches!(
        status.visibility,
        StatusVisibility::Public | StatusVisibility::Unlisted
    ) || viewer.is_some_and(|account_id| account_id == status.account_id)
}

/// Return whether a viewer can read a local status, including direct conversation membership.
pub(crate) async fn status_visible_to_viewer(
    state: &AppState,
    status: &roosty_db::LocalStatus,
    viewer: Option<AccountId>,
) -> Result<bool, RoostyError> {
    status_visible_to_viewer_on(&state.db, status, viewer).await
}

async fn status_visible_to_viewer_on(
    db: &impl ConnectionTrait,
    status: &roosty_db::LocalStatus,
    viewer: Option<AccountId>,
) -> Result<bool, RoostyError> {
    let Some(viewer) = viewer else {
        return Ok(can_view_status(status, viewer));
    };
    if viewer != status.account_id
        && roosty_db::local_accounts_are_blocked(db, viewer, status.account_id).await?
    {
        return Ok(false);
    }
    if can_view_status(status, Some(viewer)) {
        return Ok(true);
    }

    roosty_db::local_status_visible_to_account(db, status, viewer).await
}

#[cfg(test)]
fn status_content_html(content: &str) -> String {
    let mut escaped = String::new();
    push_escaped_html_with_breaks(&mut escaped, content);
    format!("<p>{escaped}</p>")
}

fn status_content_html_with_mentions_and_tags(
    state: &AppState,
    content: &str,
    mentions: &[roosty_db::LocalAccount],
    remote_mentions: &[roosty_db::RemoteActor],
    tags: &[TagResponse],
) -> String {
    let mention_urls = mentions
        .iter()
        .map(|account| {
            (
                account.username.as_str(),
                public_url(state, &format!("@{}", account.username)),
            )
        })
        .collect::<HashMap<_, _>>();
    let remote_mention_urls = remote_mentions
        .iter()
        .map(|actor| {
            (
                format!("{}@{}", actor.username, actor.domain),
                actor.activitypub_id.as_str(),
            )
        })
        .collect::<HashMap<_, _>>();
    let tag_urls = tags
        .iter()
        .map(|tag| (tag.name.as_str(), tag.url.as_str()))
        .collect::<HashMap<_, _>>();
    let mut matches = local_mention_matches(content)
        .into_iter()
        .map(TextLinkMatch::Mention)
        .chain(
            remote_mention_matches(content)
                .into_iter()
                .map(TextLinkMatch::RemoteMention),
        )
        .chain(
            local_hashtag_matches(content)
                .into_iter()
                .map(TextLinkMatch::Hashtag),
        )
        .collect::<Vec<_>>();
    matches.sort_by_key(TextLinkMatch::start);
    let mut html = String::new();
    let mut last = 0;

    for link in matches {
        if link.start() < last {
            continue;
        }
        push_escaped_html_with_breaks(&mut html, &content[last..link.start()]);
        match link {
            TextLinkMatch::Mention(mention) => {
                if let Some(url) = mention_urls.get(mention.username.as_str()) {
                    html.push_str(r#"<a href=""#);
                    html.push_str(&escape_html(url));
                    html.push_str(r#"" class="u-url mention">@"#);
                    html.push_str(&escape_html(&mention.username));
                    html.push_str("</a>");
                } else {
                    push_escaped_html_with_breaks(&mut html, &content[mention.start..mention.end]);
                }
                last = mention.end;
            }
            TextLinkMatch::RemoteMention(mention) => {
                let handle = format!("{}@{}", mention.username, mention.domain);
                if let Some(url) = remote_mention_urls.get(handle.as_str()) {
                    html.push_str(r#"<a href=""#);
                    html.push_str(&escape_html(url));
                    html.push_str(r#"" class="u-url mention">@"#);
                    html.push_str(&escape_html(&handle));
                    html.push_str("</a>");
                } else {
                    push_escaped_html_with_breaks(&mut html, &content[mention.start..mention.end]);
                }
                last = mention.end;
            }
            TextLinkMatch::Hashtag(hashtag) => {
                if let Some(url) = tag_urls.get(hashtag.name.as_str()) {
                    html.push_str(r#"<a href=""#);
                    html.push_str(&escape_html(url));
                    html.push_str(r#"" class="mention hashtag" rel="tag">#<span>"#);
                    html.push_str(&escape_html(&hashtag.name));
                    html.push_str("</span></a>");
                } else {
                    push_escaped_html_with_breaks(&mut html, &content[hashtag.start..hashtag.end]);
                }
                last = hashtag.end;
            }
        }
    }

    push_escaped_html_with_breaks(&mut html, &content[last..]);
    format!("<p>{html}</p>")
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum TextLinkMatch {
    Mention(MentionMatch),
    RemoteMention(RemoteMentionMatch),
    Hashtag(HashtagMatch),
}

impl TextLinkMatch {
    fn start(&self) -> usize {
        match self {
            TextLinkMatch::Mention(mention) => mention.start,
            TextLinkMatch::RemoteMention(mention) => mention.start,
            TextLinkMatch::Hashtag(hashtag) => hashtag.start,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct MentionMatch {
    start: usize,
    end: usize,
    username: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RemoteMentionMatch {
    start: usize,
    end: usize,
    username: String,
    domain: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct HashtagMatch {
    start: usize,
    end: usize,
    name: String,
}

/// Return local mention usernames in first-seen order.
pub(crate) fn mention_usernames(content: &str) -> Vec<String> {
    local_mention_matches(content)
        .into_iter()
        .map(|mention| mention.username)
        .collect()
}

/// Return normalized hashtag names in first-seen order.
fn hashtag_names(content: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut names = Vec::new();
    for hashtag in local_hashtag_matches(content) {
        if seen.insert(hashtag.name.clone()) {
            names.push(hashtag.name);
        }
    }

    names
}

/// Locate syntactic local `@username` mentions in a plain-text status.
fn local_mention_matches(content: &str) -> Vec<MentionMatch> {
    let mut matches = Vec::new();
    let mut previous = None;
    let mut iter = content.char_indices().peekable();

    while let Some((start, character)) = iter.next() {
        if character != '@' || !valid_mention_prefix(previous) {
            previous = Some(character);
            continue;
        }

        let mut end = start + character.len_utf8();
        let mut username = String::new();
        while let Some((index, next)) = iter.peek().copied() {
            if !valid_mention_name_character(next) {
                break;
            }
            iter.next();
            end = index + next.len_utf8();
            username.push(next);
        }

        if (2..=30).contains(&username.len())
            && iter.peek().is_none_or(|(_, character)| *character != '@')
        {
            matches.push(MentionMatch {
                start,
                end,
                username,
            });
        }
        previous = content[start..end].chars().last();
    }

    matches
}

/// Locate syntactic remote `@username@domain` mentions in a plain-text status.
fn remote_mention_matches(content: &str) -> Vec<RemoteMentionMatch> {
    let mut matches = Vec::new();
    let mut previous = None;
    let mut iter = content.char_indices().peekable();

    while let Some((start, character)) = iter.next() {
        if character != '@' || !valid_mention_prefix(previous) {
            previous = Some(character);
            continue;
        }
        let mut username = String::new();
        let mut end = start + character.len_utf8();
        while let Some((index, next)) = iter.peek().copied() {
            if !valid_mention_name_character(next) {
                break;
            }
            iter.next();
            end = index + next.len_utf8();
            username.push(next);
        }
        if iter.next_if(|(_, next)| *next == '@').is_none() {
            previous = content[start..end].chars().last();
            continue;
        }
        end += 1;
        let mut domain = String::new();
        while let Some((index, next)) = iter.peek().copied() {
            if !(next.is_ascii_alphanumeric() || next == '.' || next == '-') {
                break;
            }
            iter.next();
            end = index + next.len_utf8();
            domain.push(next);
        }
        if (2..=30).contains(&username.len())
            && domain.contains('.')
            && !domain.starts_with('.')
            && !domain.ends_with('.')
        {
            matches.push(RemoteMentionMatch {
                start,
                end,
                username,
                domain,
            });
        }
        previous = content[start..end].chars().last();
    }
    matches
}

/// Return syntactically valid remote handles in first-seen order.
pub(crate) fn remote_mention_handles(content: &str) -> Vec<String> {
    let mut handles = Vec::new();
    for mention in remote_mention_matches(content) {
        let handle = format!("{}@{}", mention.username, mention.domain);
        if !handles.contains(&handle) {
            handles.push(handle);
        }
    }
    handles
}

fn valid_mention_prefix(previous: Option<char>) -> bool {
    previous.is_none_or(|character| {
        !(character.is_ascii_alphanumeric() || character == '_' || character == '@')
    })
}

fn valid_mention_name_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

/// Locate syntactic `#tag` references in plain-text status content.
fn local_hashtag_matches(content: &str) -> Vec<HashtagMatch> {
    let mut matches = Vec::new();
    let mut previous = None;
    let mut iter = content.char_indices().peekable();

    while let Some((start, character)) = iter.next() {
        if character != '#' || !valid_hashtag_prefix(previous) {
            previous = Some(character);
            continue;
        }

        let mut end = start + character.len_utf8();
        let mut name = String::new();
        while let Some((index, next)) = iter.peek().copied() {
            if !valid_hashtag_character(next) {
                break;
            }
            iter.next();
            end = index + next.len_utf8();
            name.push(next);
        }

        if name.chars().any(|character| character.is_alphanumeric()) {
            matches.push(HashtagMatch {
                start,
                end,
                name: name.to_lowercase(),
            });
        }
        previous = content[start..end].chars().last();
    }

    matches
}

fn valid_hashtag_prefix(previous: Option<char>) -> bool {
    previous.is_none_or(|character| !(character.is_alphanumeric() || character == '_'))
}

fn valid_hashtag_character(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

fn push_escaped_html_with_breaks(output: &mut String, value: &str) {
    for segment in value.split_inclusive('\n') {
        if let Some(stripped) = segment.strip_suffix('\n') {
            output.push_str(&escape_html(stripped));
            output.push_str("<br />");
        } else {
            output.push_str(&escape_html(segment));
        }
    }
}

/// Escape untrusted plain text for use in an HTML-valued Mastodon or ActivityPub field.
pub(crate) fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

pub(crate) fn format_timestamp(timestamp: OffsetDateTime) -> String {
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        timestamp.year(),
        u8::from(timestamp.month()),
        timestamp.day(),
        timestamp.hour(),
        timestamp.minute(),
        timestamp.second(),
        timestamp.millisecond(),
    )
}

fn public_url(state: &AppState, path: &str) -> String {
    state
        .config
        .public_base_url
        .join(path.trim_start_matches('/'))
        .map(|url| url.to_string())
        .unwrap_or_else(|_| format!("{}/{}", state.config.public_base_url, path))
}

fn bad_request(description: &str) -> Response {
    error_response(StatusCode::BAD_REQUEST, "invalid_request", description)
}

fn forbidden(description: &str) -> Response {
    error_response(StatusCode::FORBIDDEN, "forbidden", description)
}

fn not_found() -> Response {
    error_response(StatusCode::NOT_FOUND, "not_found", "status not found")
}

fn tag_not_found() -> Response {
    error_response(StatusCode::NOT_FOUND, "not_found", "tag not found")
}

fn server_error(error: RoostyError) -> Response {
    error_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        "server_error",
        &error.to_string(),
    )
}

fn error_response(status: StatusCode, error: &str, description: &str) -> Response {
    (
        status,
        Json(ErrorResponse {
            error,
            error_description: description,
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use std::{
        io::Cursor,
        net::{IpAddr, Ipv4Addr, SocketAddr},
        time::{SystemTime, UNIX_EPOCH},
    };

    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{Request, StatusCode, header},
    };
    use image::{ImageBuffer, ImageFormat, Rgba};
    use postgresql_embedded::PostgreSQL;
    use roosty_core::{AccountId, StatusId};
    use roosty_db::{
        NewRemoteStatus, RemoteActor, RemoteStatus, StatusContextParent, StatusVisibility,
    };
    use roosty_migration::Migrator;
    use sea_orm_migration::MigratorTrait;
    use serde_json::{Value, json};
    use tempfile::TempDir;
    use test_context::{AsyncTestContext, test_context};
    use time::{Duration as TimeDuration, OffsetDateTime};
    use tokio::time::{Duration, timeout};
    use tower::ServiceExt;
    use uuid::Uuid;

    use super::{
        StatusContextLimits, escape_html, hashtag_names, mention_usernames, remote_mention_matches,
        remote_status_tags, status_content_html, timeline_limit,
    };
    use crate::{config::Config, http::AppState, password};

    #[test]
    /// Anonymous and authenticated context traversal retain Mastodon's distinct bounds.
    fn status_context_limits_match_mastodon_access_levels() {
        let public = StatusContextLimits::for_viewer(None);
        assert_eq!(public.ancestors, 40);
        assert_eq!(public.descendants, 60);
        assert_eq!(public.descendants_depth, Some(20));

        let authenticated = StatusContextLimits::for_viewer(Some(AccountId(Uuid::now_v7())));
        assert_eq!(authenticated.ancestors, 4_096);
        assert_eq!(authenticated.descendants, 4_096);
        assert_eq!(authenticated.descendants_depth, None);
    }

    #[test]
    /// Remote hashtag metadata remains contextual and keeps its source-instance URL.
    fn projects_valid_remote_hashtag_tags() {
        let tags = remote_status_tags(&serde_json::json!({
            "tag": [
                {"type": "Hashtag", "name": "#cats", "href": "https://remote.test/tags/cats"},
                {"type": "https://www.w3.org/ns/activitystreams#Hashtag", "name": "#cats", "href": "https://remote.test/tags/cats"},
                {"type": "Hashtag", "name": "cats", "href": "https://remote.test/tags/cats"},
                {"type": "Hashtag", "name": "#local", "href": "http://remote.test/tags/local"}
            ]
        }));
        let value = serde_json::to_value(tags).unwrap();
        assert_eq!(
            value,
            serde_json::json!([{
                "id": "cats",
                "name": "cats",
                "url": "https://remote.test/tags/cats",
                "history": []
            }])
        );
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    async fn creating_a_status_populates_status_lookup_and_timelines(context: &mut StatusContext) {
        // This exercises the first real Mastodon client flow after login:
        // post text, fetch the status, and see it in both relevant timelines.
        let token = context.access_token().await;
        let create = context
            .authenticated_json(
                "POST",
                "/api/v1/statuses",
                &token,
                serde_json::json!({"status": "hello <roosty>"}),
            )
            .await;

        assert_eq!(create.status(), StatusCode::OK);
        let created = json_body(create).await;
        assert_eq!(created["content"], "<p>hello &lt;roosty&gt;</p>");

        let status_id = created["id"].as_str().unwrap();
        let lookup = context.get(&format!("/api/v1/statuses/{status_id}")).await;
        assert_eq!(lookup.status(), StatusCode::OK);
        assert_eq!(json_body(lookup).await["id"], status_id);

        let home = context
            .authenticated_get("/api/v1/timelines/home?limit=30", &token)
            .await;
        assert_eq!(home.status(), StatusCode::OK);
        assert_eq!(json_body(home).await.as_array().unwrap().len(), 1);

        let public = context.get("/api/v1/timelines/public?limit=30").await;
        assert_eq!(public.status(), StatusCode::OK);
        assert_eq!(json_body(public).await.as_array().unwrap().len(), 1);
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Verifies that home timelines do not include unrelated local accounts.
    async fn home_timeline_is_scoped_to_authenticated_account(context: &mut StatusContext) {
        let first_token = context.access_token().await;
        let second_token = context.access_token_for("other", "other@example.com").await;

        let first_status = context
            .create_status(&first_token, "first user", None, None)
            .await;
        context
            .create_status(&second_token, "second user", None, None)
            .await;

        let first_home = json_body(
            context
                .authenticated_get("/api/v1/timelines/home?limit=30", &first_token)
                .await,
        )
        .await;

        assert_eq!(first_home.as_array().unwrap().len(), 1);
        assert_eq!(first_home[0]["id"], first_status["id"]);
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Verifies that local text mentions populate Mastodon mention metadata.
    async fn local_mentions_are_linked_and_returned(context: &mut StatusContext) {
        let token = context.access_token().await;
        context.access_token_for("alice", "alice@example.com").await;

        let status = context
            .create_status(&token, "hello @alice and @missing", None, None)
            .await;

        assert_eq!(status["mentions"].as_array().unwrap().len(), 1);
        assert_eq!(status["mentions"][0]["username"], "alice");
        assert!(status["content"].as_str().unwrap().contains(
            r#"<a href="https://localhost:4000/@alice" class="u-url mention">@alice</a>"#
        ));
        assert!(status["content"].as_str().unwrap().contains("@missing"));
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Given local hashtag text, when creating and editing a status, then Mastodon tag metadata tracks the current content.
    async fn local_hashtags_are_linked_returned_and_replaced(context: &mut StatusContext) {
        let token = context.access_token().await;
        let status = context
            .create_status(&token, "hello #Rust and #Web_Dev #rust", None, None)
            .await;
        let status_id = status["id"].as_str().unwrap();

        assert_eq!(
            status_tag_names(&status),
            ["rust".to_owned(), "web_dev".to_owned()]
        );
        assert!(status["content"].as_str().unwrap().contains(
            r#"<a href="https://localhost:4000/tags/rust" class="mention hashtag" rel="tag">#<span>rust</span></a>"#
        ));
        assert_eq!(status["tags"][0]["following"], serde_json::Value::Null);

        let edit = context
            .authenticated_json(
                "PUT",
                &format!("/api/v1/statuses/{status_id}"),
                &token,
                serde_json::json!({"status": "renamed #Roosty"}),
            )
            .await;
        assert_eq!(edit.status(), StatusCode::OK);
        assert_eq!(
            status_tag_names(&json_body(edit).await),
            ["roosty".to_owned()]
        );
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Given tagged public statuses, when reading tag and account timelines, then Mastodon tag filters are honored.
    async fn tag_timelines_and_account_tag_filters_return_matching_statuses(
        context: &mut StatusContext,
    ) {
        let token = context.access_token().await;
        let rust_web = context
            .create_status(&token, "public #rust #web", Some("public"), None)
            .await;
        let rust_cli = context
            .create_status(&token, "public #rust #cli", Some("public"), None)
            .await;
        context
            .create_status(&token, "public #other", Some("public"), None)
            .await;

        let tag_page = json_body(context.get("/api/v1/timelines/tag/rust?limit=30").await).await;
        assert_eq!(
            status_ids(&tag_page),
            [
                rust_cli["id"].as_str().unwrap().to_owned(),
                rust_web["id"].as_str().unwrap().to_owned(),
            ]
        );

        let only_web = json_body(context.get("/api/v1/timelines/tag/rust?all[]=web").await).await;
        assert_eq!(
            status_ids(&only_web),
            [rust_web["id"].as_str().unwrap().to_owned()]
        );

        let without_web =
            json_body(context.get("/api/v1/timelines/tag/rust?none[]=web").await).await;
        assert_eq!(
            status_ids(&without_web),
            [rust_cli["id"].as_str().unwrap().to_owned()]
        );

        let account = rust_web["account"]["id"].as_str().unwrap();
        let account_tagged = json_body(
            context
                .get(&format!("/api/v1/accounts/{account}/statuses?tagged=cli"))
                .await,
        )
        .await;
        assert_eq!(
            status_ids(&account_tagged),
            [rust_cli["id"].as_str().unwrap().to_owned()]
        );
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Given a stored local hashtag, when viewing the tag API, then Mastodon's public tag lookup returns metadata.
    async fn tag_lookup_returns_local_tag_metadata(context: &mut StatusContext) {
        let token = context.access_token().await;
        context
            .create_status(&token, "public #Testing", Some("public"), None)
            .await;

        let response = context.get("/api/v1/tags/testing").await;
        assert_eq!(response.status(), StatusCode::OK);
        let tag = json_body(response).await;
        assert_eq!(
            tag,
            serde_json::json!({
                "id": tag["id"],
                "name": "testing",
                "url": "https://localhost:4000/tags/testing",
                "history": [{
                    "day": tag["history"][0]["day"],
                    "uses": "1",
                    "accounts": "1"
                }]
            })
        );

        let mixed_case = context.get("/api/v1/tags/Testing").await;
        assert_eq!(mixed_case.status(), StatusCode::OK);
        assert_eq!(json_body(mixed_case).await["name"], "testing");
        assert_eq!(
            context.get("/api/v1/tags/missing").await.status(),
            StatusCode::NOT_FOUND
        );
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Given an account follows a tag, when matching public statuses exist, then followed tags and home timeline reflect that state.
    async fn followed_tags_are_listed_and_insert_matching_statuses_into_home(
        context: &mut StatusContext,
    ) {
        let admin_token = context.access_token().await;
        let bob_token = context
            .access_token_for("bob", "bob-tags@example.com")
            .await;

        let follow = context
            .authenticated_empty("POST", "/api/v1/tags/testing/follow", &bob_token)
            .await;
        assert_eq!(follow.status(), StatusCode::OK);
        assert_eq!(json_body(follow).await["following"], true);

        let status = context
            .create_status(&admin_token, "public #Testing", Some("public"), None)
            .await;
        context
            .create_status(&admin_token, "unlisted #Testing", Some("unlisted"), None)
            .await;
        let status_id = status["id"].as_str().unwrap();

        let followed = json_body(
            context
                .authenticated_get("/api/v1/followed_tags", &bob_token)
                .await,
        )
        .await;
        assert_eq!(followed.as_array().unwrap().len(), 1);
        assert_eq!(followed[0]["name"], "testing");
        assert_eq!(followed[0]["following"], true);

        let tag = json_body(
            context
                .authenticated_get("/api/v1/tags/testing", &bob_token)
                .await,
        )
        .await;
        assert_eq!(tag["following"], true);

        let home = json_body(
            context
                .authenticated_get("/api/v1/timelines/home", &bob_token)
                .await,
        )
        .await;
        assert_eq!(status_ids(&home), [status_id.to_owned()]);

        let unfollow = context
            .authenticated_empty("POST", "/api/v1/tags/testing/unfollow", &bob_token)
            .await;
        assert_eq!(unfollow.status(), StatusCode::OK);
        assert_eq!(json_body(unfollow).await["following"], false);
        assert_eq!(
            json_body(
                context
                    .authenticated_get("/api/v1/followed_tags", &bob_token)
                    .await
            )
            .await,
            serde_json::json!([])
        );
        assert_eq!(
            json_body(
                context
                    .authenticated_get("/api/v1/timelines/home", &bob_token)
                    .await
            )
            .await,
            serde_json::json!([])
        );
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    async fn deleting_a_status_removes_it_from_timelines(context: &mut StatusContext) {
        // Deletion is soft in storage but API reads should no longer expose the
        // status through direct lookup or timeline queries.
        let token = context.access_token().await;
        let create = context
            .authenticated_json(
                "POST",
                "/api/v1/statuses",
                &token,
                serde_json::json!({"status": "temporary"}),
            )
            .await;
        let status_id = json_body(create).await["id"].as_str().unwrap().to_owned();

        let delete = context
            .authenticated_empty("DELETE", &format!("/api/v1/statuses/{status_id}"), &token)
            .await;
        assert_eq!(delete.status(), StatusCode::OK);

        assert_eq!(
            context
                .get(&format!("/api/v1/statuses/{status_id}"))
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        let public = context.get("/api/v1/timelines/public").await;
        assert_eq!(json_body(public).await, serde_json::json!([]));
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    async fn status_creation_validates_auth_and_content(context: &mut StatusContext) {
        // Clients should receive normal Mastodon-style validation failures
        // instead of accidentally creating blank rows.
        let token = context.access_token().await;
        let unauthenticated = context
            .json(
                "POST",
                "/api/v1/statuses",
                serde_json::json!({"status": "hello"}),
            )
            .await;
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

        let blank = context
            .authenticated_json(
                "POST",
                "/api/v1/statuses",
                &token,
                serde_json::json!({"status": "   "}),
            )
            .await;
        assert_eq!(blank.status(), StatusCode::BAD_REQUEST);
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Verifies reply fields, mentions, and counts agree with stored parent relationships.
    async fn replies_validate_parent_statuses_and_return_reply_metadata(
        context: &mut StatusContext,
    ) {
        let token = context.access_token().await;
        let parent_token = context
            .access_token_for("parent", "parent@example.com")
            .await;
        let parent = context
            .create_status(&parent_token, "parent", None, None)
            .await;
        let parent_id = parent["id"].as_str().unwrap();
        let parent_account = parent["account"]["id"].as_str().unwrap();

        let reply = context
            .create_status(&token, "reply", None, Some(parent_id))
            .await;
        let reply_id = reply["id"].as_str().unwrap();
        assert_eq!(reply["in_reply_to_id"], parent_id);
        assert_eq!(reply["in_reply_to_account_id"], parent_account);
        assert_eq!(reply["mentions"][0]["id"], parent_account);
        assert_eq!(reply["mentions"][0]["username"], "parent");
        assert_eq!(reply["mentions"][0]["acct"], "parent");
        assert!(
            reply["mentions"][0]["url"]
                .as_str()
                .unwrap()
                .ends_with("@parent")
        );

        let parent = context.get(&format!("/api/v1/statuses/{parent_id}")).await;
        assert_eq!(json_body(parent).await["replies_count"], 1);

        let nested = context
            .create_status(&parent_token, "nested", None, Some(reply_id))
            .await;
        let nested_id = nested["id"].as_str().unwrap();
        let context_body = json_body(
            context
                .get(&format!("/api/v1/statuses/{reply_id}/context"))
                .await,
        )
        .await;
        assert_eq!(context_body["ancestors"][0]["id"], parent_id);
        assert_eq!(context_body["descendants"][0]["id"], nested_id);

        let missing_reply = context
            .authenticated_json(
                "POST",
                "/api/v1/statuses",
                &token,
                serde_json::json!({
                    "status": "missing parent",
                    "in_reply_to_id": uuid::Uuid::now_v7().to_string(),
                }),
            )
            .await;
        assert_eq!(missing_reply.status(), StatusCode::BAD_REQUEST);
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// A cached thread traverses local-to-local, local-to-remote, remote-to-local, and remote-to-remote edges.
    async fn status_context_traverses_mixed_cached_reply_graph(context: &mut StatusContext) {
        let token = context.access_token().await;
        let local_root = context
            .create_status(&token, "local root", None, None)
            .await;
        let local_root_id = StatusId(local_root["id"].as_str().unwrap().parse().unwrap());
        let remote_actor = context.cache_remote_actor("alice").await;
        let remote_parent = context
            .cache_remote_status(
                &remote_actor,
                "remote parent",
                Some(StatusContextParent::Local(local_root_id)),
            )
            .await;

        let local_child = context
            .create_status(
                &token,
                "local child",
                None,
                Some(&remote_parent.id.0.to_string()),
            )
            .await;
        let local_child_id = StatusId(local_child["id"].as_str().unwrap().parse().unwrap());
        let remote_sibling = context
            .cache_remote_status(
                &remote_actor,
                "remote sibling",
                Some(StatusContextParent::Remote(remote_parent.id)),
            )
            .await;
        let local_grandchild = context
            .create_status(
                &token,
                "local grandchild",
                None,
                Some(&local_child_id.0.to_string()),
            )
            .await;
        let local_grandchild_id = local_grandchild["id"].as_str().unwrap();

        let shown_remote = json_body(
            context
                .get(&format!("/api/v1/statuses/{}", remote_parent.id.0))
                .await,
        )
        .await;
        assert_eq!(shown_remote["id"], remote_parent.id.0.to_string());
        assert_eq!(shown_remote["account"]["acct"], "alice@remote.test");
        assert_eq!(shown_remote["replies_count"], 2);

        let shown_root = json_body(
            context
                .get(&format!("/api/v1/statuses/{}", local_root_id.0))
                .await,
        )
        .await;
        assert_eq!(shown_root["replies_count"], 1);

        let remote_context = json_body(
            context
                .get(&format!("/api/v1/statuses/{}/context", remote_parent.id.0))
                .await,
        )
        .await;
        assert_eq!(
            remote_context["ancestors"][0]["id"],
            local_root_id.0.to_string()
        );
        assert_eq!(
            remote_context["descendants"][0]["id"],
            local_child_id.0.to_string()
        );
        assert_eq!(
            remote_context["descendants"][1]["id"],
            remote_sibling.id.0.to_string()
        );
        assert_eq!(remote_context["descendants"][2]["id"], local_grandchild_id);

        let local_context = json_body(
            context
                .get(&format!("/api/v1/statuses/{}/context", local_child_id.0))
                .await,
        )
        .await;
        assert_eq!(
            local_context["ancestors"][0]["id"],
            local_root_id.0.to_string()
        );
        assert_eq!(
            local_context["ancestors"][1]["id"],
            remote_parent.id.0.to_string()
        );
        assert_eq!(local_context["descendants"][0]["id"], local_grandchild_id);
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Verifies local visibility behavior until follow graph support exists.
    async fn visibility_controls_public_timeline_and_direct_status_reads(
        context: &mut StatusContext,
    ) {
        let token = context.access_token().await;
        context
            .create_status(&token, "public", Some("public"), None)
            .await;
        let unlisted = context
            .create_status(&token, "unlisted", Some("unlisted"), None)
            .await;
        let private = context
            .create_status(&token, "private", Some("private"), None)
            .await;
        let direct = context
            .create_status(&token, "direct", Some("direct"), None)
            .await;

        let public = json_body(context.get("/api/v1/timelines/public").await).await;
        assert_eq!(public.as_array().unwrap().len(), 1);
        assert_eq!(public[0]["visibility"], "public");

        let home = json_body(
            context
                .authenticated_get("/api/v1/timelines/home", &token)
                .await,
        )
        .await;
        assert_eq!(home.as_array().unwrap().len(), 4);

        let unlisted_id = unlisted["id"].as_str().unwrap();
        assert_eq!(
            context
                .get(&format!("/api/v1/statuses/{unlisted_id}"))
                .await
                .status(),
            StatusCode::OK
        );

        for status in [private, direct] {
            let status_id = status["id"].as_str().unwrap();
            assert_eq!(
                context
                    .get(&format!("/api/v1/statuses/{status_id}"))
                    .await
                    .status(),
                StatusCode::NOT_FOUND
            );
            assert_eq!(
                context
                    .authenticated_get(&format!("/api/v1/statuses/{status_id}"), &token)
                    .await
                    .status(),
                StatusCode::OK
            );
        }
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Given a direct status mentioning a local account, when conversations are listed, then both participants see it with read state.
    async fn direct_statuses_create_local_conversations(context: &mut StatusContext) {
        let alice_token = context.access_token().await;
        let bob_token = context
            .access_token_for("bob", "bob-conversation@example.com")
            .await;
        let direct = context
            .create_status(&alice_token, "hello @bob", Some("direct"), None)
            .await;
        let direct_id = direct["id"].as_str().unwrap();

        let bob_lookup = context
            .authenticated_get(&format!("/api/v1/statuses/{direct_id}"), &bob_token)
            .await;
        assert_eq!(bob_lookup.status(), StatusCode::OK);

        let bob_conversations = json_body(
            context
                .authenticated_get("/api/v1/conversations", &bob_token)
                .await,
        )
        .await;
        let alice_conversations = json_body(
            context
                .authenticated_get("/api/v1/conversations", &alice_token)
                .await,
        )
        .await;

        assert_eq!(bob_conversations.as_array().unwrap().len(), 1);
        assert_eq!(alice_conversations.as_array().unwrap().len(), 1);
        assert_eq!(bob_conversations[0]["unread"], true);
        assert_eq!(alice_conversations[0]["unread"], false);
        assert_eq!(bob_conversations[0]["accounts"][0]["username"], "admin");
        assert_eq!(alice_conversations[0]["accounts"][0]["username"], "bob");
        assert_eq!(bob_conversations[0]["last_status"]["id"], direct_id);
        assert!(bob_conversations[0]["last_status"]["status"].is_null());

        let conversation_id = bob_conversations[0]["id"].as_str().unwrap();
        let read = json_body(
            context
                .authenticated_empty(
                    "POST",
                    &format!("/api/v1/conversations/{conversation_id}/read"),
                    &bob_token,
                )
                .await,
        )
        .await;
        assert_eq!(read["unread"], false);

        let delete = context
            .authenticated_empty(
                "DELETE",
                &format!("/api/v1/conversations/{conversation_id}"),
                &bob_token,
            )
            .await;
        assert_eq!(delete.status(), StatusCode::OK);
        let bob_conversations = json_body(
            context
                .authenticated_get("/api/v1/conversations", &bob_token)
                .await,
        )
        .await;
        let alice_conversations = json_body(
            context
                .authenticated_get("/api/v1/conversations", &alice_token)
                .await,
        )
        .await;
        assert_eq!(bob_conversations, serde_json::json!([]));
        assert_eq!(alice_conversations.as_array().unwrap().len(), 1);
        assert_eq!(
            context
                .authenticated_get(&format!("/api/v1/statuses/{direct_id}"), &bob_token)
                .await
                .status(),
            StatusCode::OK
        );
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Given a direct status, when its text and mentions are edited, then visibility and
    /// account-specific conversation identity stay fixed while access follows the new audience.
    async fn direct_status_edits_replace_the_audience(context: &mut StatusContext) {
        let alice_token = context.access_token().await;
        let bob_token = context
            .access_token_for("bob", "bob-edit-conversation@example.com")
            .await;
        let charlie_token = context
            .access_token_for("charlie", "charlie-edit-conversation@example.com")
            .await;
        let direct = context
            .create_status(&alice_token, "hello @bob", Some("direct"), None)
            .await;
        let direct_id = direct["id"].as_str().unwrap();
        let original_conversations = json_body(
            context
                .authenticated_get("/api/v1/conversations", &alice_token)
                .await,
        )
        .await;
        let original_conversation_id = original_conversations[0]["id"].clone();

        let content_edit = context
            .authenticated_json(
                "PUT",
                &format!("/api/v1/statuses/{direct_id}"),
                &alice_token,
                serde_json::json!({"status": "edited hello @bob"}),
            )
            .await;
        assert_eq!(content_edit.status(), StatusCode::OK);
        assert_eq!(json_body(content_edit).await["visibility"], "direct");
        let conversations = json_body(
            context
                .authenticated_get("/api/v1/conversations", &alice_token)
                .await,
        )
        .await;
        assert_eq!(conversations[0]["id"], original_conversation_id);
        assert_eq!(conversations[0]["accounts"][0]["username"], "bob");

        let audience_edit = context
            .authenticated_json(
                "PUT",
                &format!("/api/v1/statuses/{direct_id}"),
                &alice_token,
                serde_json::json!({"status": "hello @charlie"}),
            )
            .await;
        assert_eq!(audience_edit.status(), StatusCode::OK);
        assert_eq!(json_body(audience_edit).await["visibility"], "direct");
        assert_eq!(
            context
                .authenticated_get(&format!("/api/v1/statuses/{direct_id}"), &bob_token)
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            context
                .authenticated_get(&format!("/api/v1/statuses/{direct_id}"), &charlie_token)
                .await
                .status(),
            StatusCode::OK
        );
        let conversations = json_body(
            context
                .authenticated_get("/api/v1/conversations", &alice_token)
                .await,
        )
        .await;
        assert_eq!(conversations[0]["id"], original_conversation_id);
        assert_eq!(conversations[0]["accounts"][0]["username"], "charlie");
        assert_eq!(
            json_body(
                context
                    .authenticated_get("/api/v1/conversations", &bob_token)
                    .await,
            )
            .await,
            serde_json::json!([])
        );
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Given a direct status, when a recipient listens on the direct stream, then a conversation event is emitted.
    async fn direct_statuses_emit_conversation_stream_events(context: &mut StatusContext) {
        let alice_token = context.access_token().await;
        let bob_token = context
            .access_token_for("bob", "bob-direct-stream@example.com")
            .await;
        let bob = roosty_db::find_local_account_by_username(&context.db, "bob")
            .await
            .unwrap()
            .unwrap();
        let mut receiver = context.state.streaming_events.subscribe();

        context
            .create_status(&alice_token, "stream hello @bob", Some("direct"), None)
            .await;

        let mut stream_messages = None;
        for _ in 0..4 {
            let event = timeout(Duration::from_secs(1), receiver.recv())
                .await
                .unwrap()
                .unwrap();
            if let Some(message) = event
                .to_socket_message(bob.id, &["direct".to_owned()])
                .unwrap()
            {
                let user_message = event
                    .to_socket_message(bob.id, &["user".to_owned()])
                    .unwrap();
                stream_messages = Some((message, user_message));
                break;
            }
        }
        let (direct_message, user_message) = stream_messages.unwrap();
        let value: Value = serde_json::from_str(&direct_message).unwrap();
        let payload: Value = serde_json::from_str(value["payload"].as_str().unwrap()).unwrap();
        let conversations = json_body(
            context
                .authenticated_get("/api/v1/conversations", &bob_token)
                .await,
        )
        .await;

        assert_eq!(value["event"], "conversation");
        assert_eq!(value["stream"], serde_json::json!(["direct"]));
        assert_eq!(payload["id"], conversations[0]["id"]);
        assert_eq!(payload["unread"], true);
        assert_eq!(payload["last_status"]["visibility"], "direct");
        assert!(user_message.is_none());
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Given several direct conversations, when listing with a limit, then Mastodon cursor pagination is exposed.
    async fn conversations_use_cursor_pagination(context: &mut StatusContext) {
        let token = context.access_token().await;
        context
            .access_token_for("one", "one-conversation@example.com")
            .await;
        context
            .access_token_for("two", "two-conversation@example.com")
            .await;
        context
            .access_token_for("three", "three-conversation@example.com")
            .await;

        context
            .create_status(&token, "first @one", Some("direct"), None)
            .await;
        context
            .create_status(&token, "second @two", Some("direct"), None)
            .await;
        context
            .create_status(&token, "third @three", Some("direct"), None)
            .await;

        let page = context
            .authenticated_get("/api/v1/conversations?limit=2", &token)
            .await;
        assert_eq!(page.status(), StatusCode::OK);
        let next_cursor = link_cursor(&page, "next", "max_id");
        let body = json_body(page).await;
        assert_eq!(body.as_array().unwrap().len(), 2);
        assert_eq!(body[0]["accounts"][0]["username"], "three");
        assert_eq!(body[1]["accounts"][0]["username"], "two");

        let next = context
            .authenticated_get(
                &format!("/api/v1/conversations?limit=2&max_id={next_cursor}"),
                &token,
            )
            .await;
        assert_eq!(next.status(), StatusCode::OK);
        assert!(next.headers().get(header::LINK).is_none());
        let body = json_body(next).await;
        assert_eq!(body.as_array().unwrap().len(), 1);
        assert_eq!(body[0]["accounts"][0]["username"], "one");
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Verifies that Mastodon cursor parameters page local timelines.
    async fn timeline_cursors_page_through_local_statuses(context: &mut StatusContext) {
        let token = context.access_token().await;
        let first = context.create_status(&token, "first", None, None).await;
        let second = context.create_status(&token, "second", None, None).await;
        let third = context.create_status(&token, "third", None, None).await;

        let page = context.get("/api/v1/timelines/public?limit=2").await;
        let link = page
            .headers()
            .get(header::LINK)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        assert!(link.contains("limit=2"));
        let body = json_body(page).await;
        assert_eq!(body.as_array().unwrap().len(), 2);
        assert_eq!(body[0]["id"], third["id"]);
        assert_eq!(body[1]["id"], second["id"]);

        let second_id = second["id"].as_str().unwrap();
        let older_response = context
            .get(&format!(
                "/api/v1/timelines/public?limit=2&max_id={second_id}"
            ))
            .await;
        assert!(older_response.headers().get(header::LINK).is_none());
        let older = json_body(older_response).await;
        assert_eq!(older.as_array().unwrap().len(), 1);
        assert_eq!(older[0]["id"], first["id"]);

        let newer = json_body(
            context
                .get(&format!("/api/v1/timelines/public?since_id={second_id}"))
                .await,
        )
        .await;
        assert_eq!(newer.as_array().unwrap().len(), 1);
        assert_eq!(newer[0]["id"], third["id"]);
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Verifies that account status metadata ignores soft-deleted statuses.
    async fn account_responses_include_local_status_metadata(context: &mut StatusContext) {
        let token = context.access_token().await;
        context.create_status(&token, "kept", None, None).await;
        let deleted = context.create_status(&token, "deleted", None, None).await;
        let deleted_id = deleted["id"].as_str().unwrap();
        assert_eq!(
            context
                .authenticated_empty("DELETE", &format!("/api/v1/statuses/{deleted_id}"), &token)
                .await
                .status(),
            StatusCode::OK
        );

        let credentials = context
            .authenticated_get("/api/v1/accounts/verify_credentials", &token)
            .await;
        let body = json_body(credentials).await;
        assert_eq!(body["statuses_count"], 1);
        assert!(body["last_status_at"].as_str().is_some());
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Given an uploaded image, status creation attaches it and exposes media responses.
    async fn media_uploads_attach_to_statuses(context: &mut StatusContext) {
        let token = context.access_token().await;
        let image = encoded_test_image(ImageFormat::Png);
        let thumbnail = encoded_test_image(ImageFormat::Png);
        let upload = context
            .authenticated_multipart(
                "/api/v2/media",
                &token,
                &[
                    MultipartPart::file("file", "avatar.png", "image/png", &image),
                    MultipartPart::file("thumbnail", "preview.png", "image/png", &thumbnail),
                    MultipartPart::text("description", "profile image"),
                    MultipartPart::text("focus", "0.25,-0.5"),
                ],
            )
            .await;
        assert_eq!(upload.status(), StatusCode::OK);
        let upload_body = json_body(upload).await;
        let media_id = upload_body["id"].as_str().unwrap();
        assert_eq!(upload_body["type"], "image");
        assert_eq!(upload_body["description"], "profile image");
        assert_eq!(upload_body["meta"]["original"]["width"], 3);
        assert_eq!(upload_body["meta"]["original"]["height"], 2);
        assert_eq!(upload_body["meta"]["small"]["width"], 3);
        assert_eq!(upload_body["meta"]["small"]["height"], 2);
        assert_eq!(upload_body["meta"]["focus"]["x"], 0.25);
        assert_eq!(upload_body["meta"]["focus"]["y"], -0.5);
        assert!(upload_body["blurhash"].as_str().unwrap().len() > 10);
        assert_ne!(upload_body["url"], upload_body["preview_url"]);

        let status = context
            .authenticated_json(
                "POST",
                "/api/v1/statuses",
                &token,
                serde_json::json!({
                    "status": "",
                    "media_ids": [media_id]
                }),
            )
            .await;
        assert_eq!(status.status(), StatusCode::OK);
        let status_body = json_body(status).await;
        assert_eq!(status_body["media_attachments"][0]["id"], media_id);
        assert_eq!(
            status_body["media_attachments"][0]["description"],
            "profile image"
        );

        let media_url = status_body["media_attachments"][0]["url"].as_str().unwrap();
        let media_path = media_url.strip_prefix("https://localhost:4000").unwrap();
        let served = context.get(media_path).await;
        assert_eq!(served.status(), StatusCode::OK);

        let attached_lookup = context
            .authenticated_get(&format!("/api/v1/media/{media_id}"), &token)
            .await;
        assert_eq!(attached_lookup.status(), StatusCode::NOT_FOUND);

        let only_media = json_body(
            context
                .get(&format!(
                    "/api/v1/accounts/{}/statuses?only_media=true",
                    context.account_id.0
                ))
                .await,
        )
        .await;
        assert_eq!(only_media.as_array().unwrap().len(), 1);
        assert_eq!(only_media[0]["id"], status_body["id"]);
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Given a legacy Mastodon upload URL, accepts the same local image upload.
    async fn media_upload_accepts_v1_endpoint(context: &mut StatusContext) {
        let token = context.access_token().await;
        let image = encoded_test_image(ImageFormat::Png);
        let upload = context
            .authenticated_multipart(
                "/api/v1/media",
                &token,
                &[MultipartPart::file(
                    "file",
                    "avatar.png",
                    "image/png",
                    &image,
                )],
            )
            .await;

        assert_eq!(upload.status(), StatusCode::OK);
        let upload_body = json_body(upload).await;
        assert_eq!(upload_body["type"], "image");
        assert!(
            upload_body["url"]
                .as_str()
                .unwrap()
                .contains("/media_attachments/files/")
        );
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Given the instance descriptor, clients see the expanded local image formats.
    async fn instance_descriptor_advertises_supported_image_formats(context: &mut StatusContext) {
        let instance = json_body(context.get("/api/v2/instance").await).await;
        let supported = instance["configuration"]["media_attachments"]["supported_mime_types"]
            .as_array()
            .unwrap();
        let supported: Vec<&str> = supported
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect();

        assert!(supported.contains(&"image/avif"));
        assert!(supported.contains(&"image/bmp"));
        assert!(supported.contains(&"image/gif"));
        assert!(supported.contains(&"image/jpeg"));
        assert!(supported.contains(&"image/png"));
        assert!(supported.contains(&"image/tiff"));
        assert!(supported.contains(&"image/webp"));
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Given a newly advertised image format, upload processing accepts and previews it.
    async fn media_upload_accepts_bmp_from_expanded_formats(context: &mut StatusContext) {
        let token = context.access_token().await;
        let image = encoded_test_image(ImageFormat::Bmp);
        let upload = context
            .authenticated_multipart(
                "/api/v2/media",
                &token,
                &[MultipartPart::file(
                    "file",
                    "avatar.bmp",
                    "image/bmp",
                    &image,
                )],
            )
            .await;

        assert_eq!(upload.status(), StatusCode::OK);
        let body = json_body(upload).await;
        assert_eq!(body["type"], "image");
        assert_eq!(body["meta"]["original"]["size"], "3x2");
        assert_eq!(body["meta"]["small"]["size"], "3x2");
        assert!(
            body["preview_url"]
                .as_str()
                .unwrap()
                .ends_with("-small.png")
        );
        assert!(body["blurhash"].as_str().is_some());
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Given unattached media, updating its thumbnail replaces small metadata.
    async fn media_update_accepts_custom_thumbnail(context: &mut StatusContext) {
        let token = context.access_token().await;
        let image = encoded_test_image(ImageFormat::Png);
        let upload = context
            .authenticated_multipart(
                "/api/v2/media",
                &token,
                &[MultipartPart::file(
                    "file",
                    "avatar.png",
                    "image/png",
                    &image,
                )],
            )
            .await;
        let upload = json_body(upload).await;
        let media_id = upload["id"].as_str().unwrap();
        assert_eq!(upload["meta"]["small"]["size"], "3x2");

        let thumbnail = encoded_sized_test_image(ImageFormat::Png, 2, 4);
        let update = context
            .authenticated_multipart_method(
                "PUT",
                &format!("/api/v1/media/{media_id}"),
                &token,
                &[MultipartPart::file(
                    "thumbnail",
                    "preview.png",
                    "image/png",
                    &thumbnail,
                )],
            )
            .await;

        assert_eq!(update.status(), StatusCode::OK);
        let update = json_body(update).await;
        assert_eq!(update["meta"]["original"]["size"], "3x2");
        assert_eq!(update["meta"]["small"]["size"], "2x4");
        assert_ne!(upload["blurhash"], update["blurhash"]);
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Given unattached media, updating description persists alt text into status responses.
    async fn media_update_persists_description(context: &mut StatusContext) {
        let token = context.access_token().await;
        let image = encoded_test_image(ImageFormat::Png);
        let upload = context
            .authenticated_multipart(
                "/api/v2/media",
                &token,
                &[MultipartPart::file(
                    "file",
                    "avatar.png",
                    "image/png",
                    &image,
                )],
            )
            .await;
        let upload = json_body(upload).await;
        let media_id = upload["id"].as_str().unwrap();

        let update = context
            .authenticated_json(
                "PUT",
                &format!("/api/v1/media/{media_id}"),
                &token,
                serde_json::json!({ "description": "Alt test" }),
            )
            .await;

        assert_eq!(update.status(), StatusCode::OK);
        assert_eq!(json_body(update).await["description"], "Alt test");

        let status = context
            .authenticated_json(
                "POST",
                "/api/v1/statuses",
                &token,
                serde_json::json!({
                    "status": "",
                    "media_ids": [media_id]
                }),
            )
            .await;
        assert_eq!(status.status(), StatusCode::OK);
        assert_eq!(
            json_body(status).await["media_attachments"][0]["description"],
            "Alt test"
        );
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Given an owned status, the Mastodon edit endpoint updates text and edit metadata.
    async fn status_update_persists_text_changes(context: &mut StatusContext) {
        let token = context.access_token().await;
        let status = context
            .create_status(&token, "original text", None, None)
            .await;
        let status_id = status["id"].as_str().unwrap();

        let update = context
            .authenticated_json(
                "PUT",
                &format!("/api/v1/statuses/{status_id}"),
                &token,
                serde_json::json!({
                    "status": "edited text",
                    "sensitive": true,
                    "spoiler_text": "warning",
                    "language": "en"
                }),
            )
            .await;

        assert_eq!(update.status(), StatusCode::OK);
        let update = json_body(update).await;
        assert_eq!(update["content"], "<p>edited text</p>");
        assert_eq!(update["sensitive"], true);
        assert_eq!(update["spoiler_text"], "warning");
        assert_eq!(update["language"], "en");
        assert!(update["edited_at"].as_str().is_some());
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Given a local follower subscribed to their home stream, when the author edits a status,
    /// then the follower receives the replacement status as a streaming update.
    async fn status_update_streams_to_local_followers(context: &mut StatusContext) {
        let author_token = context.access_token().await;
        context
            .access_token_for("follower", "follower-update@example.com")
            .await;
        let follower = roosty_db::find_local_account_by_username(&context.db, "follower")
            .await
            .unwrap()
            .unwrap();
        roosty_db::follow_local_account(&context.db, follower.id, context.account_id, true, false)
            .await
            .unwrap();
        let status = context
            .create_status(&author_token, "original text", None, None)
            .await;
        let status_id = status["id"].as_str().unwrap();
        let mut receiver = context.state.streaming_events.subscribe();

        let update = context
            .authenticated_json(
                "PUT",
                &format!("/api/v1/statuses/{status_id}"),
                &author_token,
                serde_json::json!({"status": "edited text"}),
            )
            .await;

        assert_eq!(update.status(), StatusCode::OK);
        let event = timeout(Duration::from_secs(1), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        let message = event
            .to_socket_message(follower.id, &["user".to_owned()])
            .unwrap()
            .unwrap();
        let message: Value = serde_json::from_str(&message).unwrap();
        let payload: Value = serde_json::from_str(message["payload"].as_str().unwrap()).unwrap();

        assert_eq!(message["event"], "status.update");
        assert_eq!(message["stream"], serde_json::json!(["user"]));
        assert_eq!(payload["id"], status_id);
        assert_eq!(payload["content"], "<p>edited text</p>");
        assert!(payload["edited_at"].as_str().is_some());
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Given a followed hashtag, matching public creates, edits, and deletes reach the user stream
    /// while an edit that removes the hashtag no longer targets that subscriber.
    async fn followed_tags_stream_current_matching_statuses(context: &mut StatusContext) {
        let author_token = context.access_token().await;
        let follower_token = context
            .access_token_for("tag_follower", "tag-follower@example.com")
            .await;
        let follower = roosty_db::find_local_account_by_username(&context.db, "tag_follower")
            .await
            .unwrap()
            .unwrap();
        let follow = context
            .authenticated_empty("POST", "/api/v1/tags/testing/follow", &follower_token)
            .await;
        assert_eq!(follow.status(), StatusCode::OK);
        let mut receiver = context.state.streaming_events.subscribe();

        let status = context
            .create_status(&author_token, "first #testing", Some("public"), None)
            .await;
        let status_id = status["id"].as_str().unwrap();
        let event = timeout(Duration::from_secs(1), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        let message = event
            .to_socket_message(follower.id, &["user".to_owned()])
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&message).unwrap()["event"],
            "update"
        );

        context
            .authenticated_json(
                "PUT",
                &format!("/api/v1/statuses/{status_id}"),
                &author_token,
                serde_json::json!({"status": "edited #testing"}),
            )
            .await;
        let event = timeout(Duration::from_secs(1), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        let message = event
            .to_socket_message(follower.id, &["user".to_owned()])
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&message).unwrap()["event"],
            "status.update"
        );

        context
            .authenticated_json(
                "PUT",
                &format!("/api/v1/statuses/{status_id}"),
                &author_token,
                serde_json::json!({"status": "tag removed"}),
            )
            .await;
        let event = timeout(Duration::from_secs(1), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            event
                .to_socket_message(follower.id, &["user".to_owned()])
                .unwrap()
                .is_none()
        );

        let deleted = context
            .create_status(&author_token, "delete #testing", Some("public"), None)
            .await;
        let deleted_id = deleted["id"].as_str().unwrap();
        let _ = timeout(Duration::from_secs(1), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        context
            .authenticated_empty(
                "DELETE",
                &format!("/api/v1/statuses/{deleted_id}"),
                &author_token,
            )
            .await;
        let event = timeout(Duration::from_secs(1), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        let message = event
            .to_socket_message(follower.id, &["user".to_owned()])
            .unwrap()
            .unwrap();
        let message: Value = serde_json::from_str(&message).unwrap();
        assert_eq!(message["event"], "delete");
        assert_eq!(message["payload"], deleted_id);
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Given an owned status, when an editor requests its source, then the original plain text and
    /// content warning are returned instead of the rendered HTML.
    async fn status_source_returns_editable_plain_text(context: &mut StatusContext) {
        let token = context.access_token().await;
        let status = context
            .authenticated_json(
                "POST",
                "/api/v1/statuses",
                &token,
                serde_json::json!({
                    "status": "hello <roosty>",
                    "spoiler_text": "warning"
                }),
            )
            .await;
        let status = json_body(status).await;
        let status_id = status["id"].as_str().unwrap();

        let source = context
            .authenticated_get(&format!("/api/v1/statuses/{status_id}/source"), &token)
            .await;

        assert_eq!(source.status(), StatusCode::OK);
        assert_eq!(
            json_body(source).await,
            serde_json::json!({
                "id": status_id,
                "text": "hello <roosty>",
                "spoiler_text": "warning"
            })
        );
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Given a status source request without a token, then the editor endpoint rejects it.
    async fn status_source_requires_authentication(context: &mut StatusContext) {
        let token = context.access_token().await;
        let status = context.create_status(&token, "hello", None, None).await;
        let status_id = status["id"].as_str().unwrap();

        let source = context
            .get(&format!("/api/v1/statuses/{status_id}/source"))
            .await;

        assert_eq!(source.status(), StatusCode::UNAUTHORIZED);
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Given an owned status with media, status edit media attributes persist alt text.
    async fn status_update_persists_media_attributes(context: &mut StatusContext) {
        let token = context.access_token().await;
        let image = encoded_test_image(ImageFormat::Png);
        let upload = context
            .authenticated_multipart(
                "/api/v2/media",
                &token,
                &[MultipartPart::file(
                    "file",
                    "avatar.png",
                    "image/png",
                    &image,
                )],
            )
            .await;
        let upload = json_body(upload).await;
        let media_id = upload["id"].as_str().unwrap();
        let status = context
            .authenticated_json(
                "POST",
                "/api/v1/statuses",
                &token,
                serde_json::json!({
                    "status": "",
                    "media_ids": [media_id]
                }),
            )
            .await;
        let status = json_body(status).await;
        let status_id = status["id"].as_str().unwrap();

        let update = context
            .authenticated_json(
                "PUT",
                &format!("/api/v1/statuses/{status_id}"),
                &token,
                serde_json::json!({
                    "media_attributes": [{
                        "id": media_id,
                        "description": "Alt test",
                        "focus": "0.1,-0.2"
                    }]
                }),
            )
            .await;

        assert_eq!(update.status(), StatusCode::OK);
        let update = json_body(update).await;
        assert_eq!(update["media_attachments"][0]["description"], "Alt test");
        assert_eq!(update["media_attachments"][0]["meta"]["focus"]["x"], 0.1);
        assert_eq!(update["media_attachments"][0]["meta"]["focus"]["y"], -0.2);
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Given an owned status with media, omitting `media_ids` retains its attachments while an
    /// explicitly empty collection detaches every attachment.
    async fn status_update_distinguishes_omitted_and_empty_media_ids(context: &mut StatusContext) {
        let token = context.access_token().await;
        let image = encoded_test_image(ImageFormat::Png);
        let upload = context
            .authenticated_multipart(
                "/api/v2/media",
                &token,
                &[MultipartPart::file(
                    "file",
                    "attachment.png",
                    "image/png",
                    &image,
                )],
            )
            .await;
        let upload = json_body(upload).await;
        let media_id = upload["id"].as_str().unwrap();
        let status = context
            .authenticated_json(
                "POST",
                "/api/v1/statuses",
                &token,
                serde_json::json!({
                    "status": "with attachment",
                    "media_ids": [media_id]
                }),
            )
            .await;
        let status = json_body(status).await;
        let status_id = status["id"].as_str().unwrap();

        let omitted = context
            .authenticated_json(
                "PUT",
                &format!("/api/v1/statuses/{status_id}"),
                &token,
                serde_json::json!({"status": "attachment retained"}),
            )
            .await;
        assert_eq!(omitted.status(), StatusCode::OK);
        assert_eq!(
            json_body(omitted).await["media_attachments"][0]["id"],
            media_id
        );

        let empty = context
            .authenticated_json(
                "PUT",
                &format!("/api/v1/statuses/{status_id}"),
                &token,
                serde_json::json!({"media_ids": []}),
            )
            .await;
        assert_eq!(empty.status(), StatusCode::OK);
        assert_eq!(
            json_body(empty).await["media_attachments"],
            serde_json::json!([])
        );
        assert_eq!(
            context
                .authenticated_get(&format!("/api/v1/media/{media_id}"), &token)
                .await
                .status(),
            StatusCode::OK
        );
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Given unsupported media input, upload rejects it before storing metadata.
    async fn media_upload_rejects_unsupported_content_type(context: &mut StatusContext) {
        let token = context.access_token().await;
        let upload = context
            .authenticated_multipart(
                "/api/v2/media",
                &token,
                &[MultipartPart::file(
                    "file",
                    "notes.txt",
                    "text/plain",
                    b"plain text",
                )],
            )
            .await;

        assert_eq!(upload.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Given a browser sends `file=null`, upload returns validation instead of extractor failure.
    async fn media_upload_rejects_null_text_file_field(context: &mut StatusContext) {
        let token = context.access_token().await;
        let upload = context
            .authenticated_multipart(
                "/api/v2/media",
                &token,
                &[MultipartPart::text("file", "null")],
            )
            .await;

        assert_eq!(upload.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(json_body(upload).await["error"], "file is required");
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Verifies that repeated favourite and unfavourite calls keep counts stable.
    async fn favourites_are_idempotent_and_update_status_fields(context: &mut StatusContext) {
        let token = context.access_token().await;
        let status = context
            .create_status(&token, "favourite me", None, None)
            .await;
        let status_id = status["id"].as_str().unwrap();

        let first = context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/statuses/{status_id}/favourite"),
                &token,
            )
            .await;
        assert_eq!(first.status(), StatusCode::OK);
        let first = json_body(first).await;
        assert_eq!(first["favourited"], true);
        assert_eq!(first["favourites_count"], 1);

        let second = context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/statuses/{status_id}/favourite"),
                &token,
            )
            .await;
        assert_eq!(second.status(), StatusCode::OK);
        let second = json_body(second).await;
        assert_eq!(second["favourited"], true);
        assert_eq!(second["favourites_count"], 1);

        let lookup = context
            .authenticated_get(&format!("/api/v1/statuses/{status_id}"), &token)
            .await;
        let lookup = json_body(lookup).await;
        assert_eq!(lookup["favourited"], true);
        assert_eq!(lookup["favourites_count"], 1);

        let favourites = json_body(
            context
                .authenticated_get("/api/v1/favourites?limit=30", &token)
                .await,
        )
        .await;
        assert_eq!(favourites.as_array().unwrap().len(), 1);
        assert_eq!(favourites[0]["id"], status_id);
        assert_eq!(favourites[0]["favourited"], true);
        assert_eq!(favourites[0]["favourites_count"], 1);

        let anonymous =
            json_body(context.get(&format!("/api/v1/statuses/{status_id}")).await).await;
        assert_eq!(anonymous["favourited"], false);
        assert_eq!(anonymous["favourites_count"], 1);

        let unfavourite = context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/statuses/{status_id}/unfavourite"),
                &token,
            )
            .await;
        assert_eq!(unfavourite.status(), StatusCode::OK);
        let unfavourite = json_body(unfavourite).await;
        assert_eq!(unfavourite["favourited"], false);
        assert_eq!(unfavourite["favourites_count"], 0);
        let favourites = json_body(
            context
                .authenticated_get("/api/v1/favourites", &token)
                .await,
        )
        .await;
        assert_eq!(favourites, serde_json::json!([]));

        let repeated = context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/statuses/{status_id}/unfavourite"),
                &token,
            )
            .await;
        assert_eq!(repeated.status(), StatusCode::OK);
        assert_eq!(json_body(repeated).await["favourites_count"], 0);
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Given repeated boost mutations, when the status is read, then count and viewer state remain stable.
    async fn reblogs_are_idempotent_and_update_status_fields(context: &mut StatusContext) {
        let token = context.access_token().await;
        let status = context.create_status(&token, "boost me", None, None).await;
        let status_id = status["id"].as_str().unwrap();

        let first = context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/statuses/{status_id}/reblog"),
                &token,
            )
            .await;
        assert_eq!(first.status(), StatusCode::OK);
        let first = json_body(first).await;
        assert_eq!(
            reblog_projection(&first),
            serde_json::json!({
                "account": "admin",
                "reblogged": true,
                "reblog": {
                    "id": status_id,
                    "reblogged": true,
                    "reblogs_count": 1
                }
            })
        );

        let repeated = context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/statuses/{status_id}/reblog"),
                &token,
            )
            .await;
        assert_eq!(repeated.status(), StatusCode::OK);
        assert_eq!(
            reblog_projection(&json_body(repeated).await),
            serde_json::json!({
                "account": "admin",
                "reblogged": true,
                "reblog": {
                    "id": status_id,
                    "reblogged": true,
                    "reblogs_count": 1
                }
            })
        );

        let anonymous =
            json_body(context.get(&format!("/api/v1/statuses/{status_id}")).await).await;
        assert_eq!(
            status_interaction_projection(&anonymous),
            serde_json::json!({
                "reblogged": false,
                "reblogs_count": 1,
                "favourited": false,
                "favourites_count": 0,
            })
        );

        let unreblog = context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/statuses/{status_id}/unreblog"),
                &token,
            )
            .await;
        assert_eq!(unreblog.status(), StatusCode::OK);
        assert_eq!(
            status_interaction_projection(&json_body(unreblog).await),
            serde_json::json!({
                "reblogged": false,
                "reblogs_count": 0,
                "favourited": false,
                "favourites_count": 0,
            })
        );
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Given several local boosts, when `reblogged_by` is paged, then accounts and Link cursors are returned.
    async fn reblogged_by_uses_cursor_pagination(context: &mut StatusContext) {
        let owner_token = context.access_token().await;
        let alice_token = context
            .access_token_for("alice", "alice-reblogged-by@example.com")
            .await;
        let bob_token = context
            .access_token_for("bob", "bob-reblogged-by@example.com")
            .await;
        let carol_token = context
            .access_token_for("carol", "carol-reblogged-by@example.com")
            .await;
        let status = context
            .create_status(&owner_token, "boost target", None, None)
            .await;
        let status_id = status["id"].as_str().unwrap();

        for token in [&alice_token, &bob_token, &carol_token] {
            let response = context
                .authenticated_empty(
                    "POST",
                    &format!("/api/v1/statuses/{status_id}/reblog"),
                    token,
                )
                .await;
            assert_eq!(response.status(), StatusCode::OK);
        }

        let page = context
            .get(&format!(
                "/api/v1/statuses/{status_id}/reblogged_by?limit=2"
            ))
            .await;
        assert_eq!(page.status(), StatusCode::OK);
        let next_cursor = link_cursor(&page, "next", "max_id");
        let page_body = json_body(page).await;
        assert_eq!(
            account_usernames(&page_body),
            serde_json::json!(["carol", "bob"])
        );

        let next_page = context
            .get(&format!(
                "/api/v1/statuses/{status_id}/reblogged_by?limit=2&max_id={next_cursor}"
            ))
            .await;
        assert_eq!(next_page.status(), StatusCode::OK);
        assert!(next_page.headers().get(header::LINK).is_none());
        assert_eq!(
            account_usernames(&json_body(next_page).await),
            serde_json::json!(["alice"])
        );
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Given a followed account boosts a visible status, when home is loaded, then the boost appears as a reblog entry.
    async fn home_timeline_includes_followed_reblogs(context: &mut StatusContext) {
        let owner_token = context.access_token().await;
        let bob_token = context
            .access_token_for("bob", "bob-home-reblog@example.com")
            .await;
        let bob = roosty_db::find_local_account_by_username(&context.db, "bob")
            .await
            .unwrap()
            .unwrap();
        let status = context
            .create_status(&owner_token, "home boost target", None, None)
            .await;
        let status_id = status["id"].as_str().unwrap();
        let follow = context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/accounts/{}/follow", bob.id.0),
                &owner_token,
            )
            .await;
        assert_eq!(follow.status(), StatusCode::OK);
        let reblog = context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/statuses/{status_id}/reblog"),
                &bob_token,
            )
            .await;
        assert_eq!(reblog.status(), StatusCode::OK);

        let home = context
            .authenticated_get("/api/v1/timelines/home?limit=30", &owner_token)
            .await;
        assert_eq!(home.status(), StatusCode::OK);
        let home = json_body(home).await;

        assert_eq!(
            reblog_projection(&home[0]),
            serde_json::json!({
                "account": "bob",
                "reblogged": false,
                "reblog": {
                    "id": status_id,
                    "reblogged": false,
                    "reblogs_count": 1
                }
            })
        );
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Given an original status with a local boost, when the original is deleted, then the boost leaves home timelines too.
    async fn deleting_original_status_removes_reblog_timeline_entries(context: &mut StatusContext) {
        let owner_token = context.access_token().await;
        let bob_token = context
            .access_token_for("bob", "bob-delete-reblog@example.com")
            .await;
        let bob = roosty_db::find_local_account_by_username(&context.db, "bob")
            .await
            .unwrap()
            .unwrap();
        let status = context
            .create_status(&owner_token, "delete boost target", None, None)
            .await;
        let status_id = status["id"].as_str().unwrap();
        let follow = context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/accounts/{}/follow", bob.id.0),
                &owner_token,
            )
            .await;
        assert_eq!(follow.status(), StatusCode::OK);
        let reblog = context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/statuses/{status_id}/reblog"),
                &bob_token,
            )
            .await;
        assert_eq!(reblog.status(), StatusCode::OK);

        let delete = context
            .authenticated_empty(
                "DELETE",
                &format!("/api/v1/statuses/{status_id}"),
                &owner_token,
            )
            .await;
        assert_eq!(delete.status(), StatusCode::OK);
        let home = context
            .authenticated_get("/api/v1/timelines/home?limit=30", &owner_token)
            .await;

        assert_eq!(json_body(home).await, serde_json::json!([]));
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Given a local mention, when the mentioned user lists notifications, then the mention appears with actor and status data.
    async fn mentions_create_local_notifications(context: &mut StatusContext) {
        let admin_token = context.access_token().await;
        let bob_token = context
            .access_token_for("bob", "bob-notifications@example.com")
            .await;
        let status = context
            .create_status(&bob_token, "hello @admin", None, None)
            .await;

        let response = context
            .authenticated_get("/api/v1/notifications?limit=30", &admin_token)
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let notifications = json_body(response).await;

        assert_eq!(
            notification_projection(&notifications[0]),
            serde_json::json!({
                "type": "mention",
                "account": "bob",
                "status": status["id"],
            })
        );
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Given a follow with notifications enabled, only new original posts and self-thread replies
    /// create Mastodon `status` notifications, and the first notification is streamed live.
    async fn notified_follows_create_status_notifications(context: &mut StatusContext) {
        let author_token = context.access_token().await;
        let follower_token = context
            .access_token_for("notified", "notified@example.com")
            .await;
        let follower = roosty_db::find_local_account_by_username(&context.db, "notified")
            .await
            .unwrap()
            .unwrap();
        let follow = context
            .authenticated_json(
                "POST",
                &format!("/api/v1/accounts/{}/follow", context.account_id.0),
                &follower_token,
                serde_json::json!({"notify": true}),
            )
            .await;
        assert_eq!(json_body(follow).await["notifying"], true);
        let mut receiver = context.state.streaming_events.subscribe();

        let original = context
            .create_status(&author_token, "notify original", Some("public"), None)
            .await;
        let original_id = original["id"].as_str().unwrap();
        let event = timeout(Duration::from_secs(1), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        let message = event
            .to_socket_message(follower.id, &["user".to_owned()])
            .unwrap()
            .unwrap();
        let message: Value = serde_json::from_str(&message).unwrap();
        assert_eq!(message["event"], "notification");
        let payload: Value = serde_json::from_str(message["payload"].as_str().unwrap()).unwrap();
        assert_eq!(payload["type"], "status");
        assert_eq!(payload["status"]["id"], original_id);

        context
            .authenticated_json(
                "PUT",
                &format!("/api/v1/statuses/{original_id}"),
                &author_token,
                serde_json::json!({"status": "edited without notification"}),
            )
            .await;
        context
            .create_status(
                &author_token,
                "self reply",
                Some("public"),
                Some(original_id),
            )
            .await;
        let foreign = context
            .create_status(&follower_token, "foreign parent", Some("public"), None)
            .await;
        context
            .create_status(
                &author_token,
                "reply to another account",
                Some("public"),
                foreign["id"].as_str(),
            )
            .await;
        context
            .create_status(&author_token, "direct", Some("direct"), None)
            .await;

        let response = context
            .authenticated_get("/api/v1/notifications?types[]=status", &follower_token)
            .await;
        let notifications = json_body(response).await;
        assert_eq!(notifications.as_array().unwrap().len(), 2);
        assert!(
            notifications
                .as_array()
                .unwrap()
                .iter()
                .all(|notification| notification["type"] == "status")
        );
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Given a local favourite, when the status owner lists notifications, then the favourite is persisted once.
    async fn favourites_create_local_notifications(context: &mut StatusContext) {
        let owner_token = context.access_token().await;
        let bob_token = context
            .access_token_for("bob", "bob-favourite-notifications@example.com")
            .await;
        let status = context
            .create_status(&owner_token, "favourite target", None, None)
            .await;
        let status_id = status["id"].as_str().unwrap();

        for _ in 0..2 {
            let response = context
                .authenticated_empty(
                    "POST",
                    &format!("/api/v1/statuses/{status_id}/favourite"),
                    &bob_token,
                )
                .await;
            assert_eq!(response.status(), StatusCode::OK);
        }

        let response = context
            .authenticated_get("/api/v1/notifications?types[]=favourite", &owner_token)
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let notifications = json_body(response).await;

        assert_eq!(
            notifications
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .map(notification_projection)
                        .collect::<Vec<_>>()
                })
                .unwrap(),
            vec![serde_json::json!({
                "type": "favourite",
                "account": "bob",
                "status": status["id"],
            })]
        );
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Given a local boost, when the status owner lists notifications, then the reblog notification is persisted.
    async fn reblogs_create_local_notifications(context: &mut StatusContext) {
        let owner_token = context.access_token().await;
        let bob_token = context
            .access_token_for("bob", "bob-reblog-notifications@example.com")
            .await;
        let status = context
            .create_status(&owner_token, "reblog notification target", None, None)
            .await;
        let status_id = status["id"].as_str().unwrap();
        let reblog = context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/statuses/{status_id}/reblog"),
                &bob_token,
            )
            .await;
        assert_eq!(reblog.status(), StatusCode::OK);

        let response = context
            .authenticated_get("/api/v1/notifications?types[]=reblog", &owner_token)
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let notifications = json_body(response).await;

        assert_eq!(
            notifications
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .map(notification_projection)
                        .collect::<Vec<_>>()
                })
                .unwrap(),
            vec![serde_json::json!({
                "type": "reblog",
                "account": "bob",
                "status": status["id"],
            })]
        );
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Given several local notifications, when paging to the final page, then no extra Link header is advertised.
    async fn notifications_suppress_final_page_link(context: &mut StatusContext) {
        let owner_token = context.access_token().await;
        let alice_token = context
            .access_token_for("alice", "alice-notification-page@example.com")
            .await;
        let bob_token = context
            .access_token_for("bob", "bob-notification-page@example.com")
            .await;
        let carol_token = context
            .access_token_for("carol", "carol-notification-page@example.com")
            .await;
        let status = context
            .create_status(&owner_token, "notification page target", None, None)
            .await;
        let status_id = status["id"].as_str().unwrap();

        for token in [&alice_token, &bob_token, &carol_token] {
            let response = context
                .authenticated_empty(
                    "POST",
                    &format!("/api/v1/statuses/{status_id}/favourite"),
                    token,
                )
                .await;
            assert_eq!(response.status(), StatusCode::OK);
        }

        let page = context
            .authenticated_get("/api/v1/notifications?limit=2", &owner_token)
            .await;
        assert_eq!(page.status(), StatusCode::OK);
        let next_cursor = link_cursor(&page, "next", "max_id");
        let page_body = json_body(page).await;
        assert_eq!(page_body.as_array().unwrap().len(), 2);

        let next_page = context
            .authenticated_get(
                &format!("/api/v1/notifications?limit=2&max_id={next_cursor}"),
                &owner_token,
            )
            .await;
        assert_eq!(next_page.status(), StatusCode::OK);
        assert!(next_page.headers().get(header::LINK).is_none());
        assert_eq!(json_body(next_page).await.as_array().unwrap().len(), 1);
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Given a follow notification, when it is dismissed, then it disappears from the recipient's collection.
    async fn follow_notifications_can_be_dismissed(context: &mut StatusContext) {
        let admin_token = context.access_token().await;
        let bob_token = context
            .access_token_for("bob", "bob-follow-notifications@example.com")
            .await;
        let admin = roosty_db::find_local_account_by_username(&context.db, "admin")
            .await
            .unwrap()
            .unwrap();

        let follow = context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/accounts/{}/follow", admin.id.0),
                &bob_token,
            )
            .await;
        assert_eq!(follow.status(), StatusCode::OK);

        let response = context
            .authenticated_get("/api/v1/notifications?types[]=follow", &admin_token)
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let notifications = json_body(response).await;
        let notification_id = notifications[0]["id"].as_str().unwrap();
        assert_eq!(
            notification_projection(&notifications[0]),
            serde_json::json!({
                "type": "follow",
                "account": "bob",
                "status": null,
            })
        );

        let dismiss = context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/notifications/{notification_id}/dismiss"),
                &admin_token,
            )
            .await;
        assert_eq!(dismiss.status(), StatusCode::OK);
        let response = context
            .authenticated_get("/api/v1/notifications?types[]=follow", &admin_token)
            .await;

        assert_eq!(json_body(response).await, serde_json::json!([]));
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Verifies favourites expose Mastodon cursor pagination through Link headers.
    async fn favourites_collection_uses_cursor_pagination(context: &mut StatusContext) {
        let token = context.access_token().await;
        let first = context.create_status(&token, "first", None, None).await;
        let second = context.create_status(&token, "second", None, None).await;
        let third = context.create_status(&token, "third", None, None).await;
        for status in [&first, &second, &third] {
            let status_id = status["id"].as_str().unwrap();
            let response = context
                .authenticated_empty(
                    "POST",
                    &format!("/api/v1/statuses/{status_id}/favourite"),
                    &token,
                )
                .await;
            assert_eq!(response.status(), StatusCode::OK);
        }

        let page = context
            .authenticated_get("/api/v1/favourites?limit=2", &token)
            .await;
        assert_eq!(page.status(), StatusCode::OK);
        let next_cursor = link_cursor(&page, "next", "max_id");
        let body = json_body(page).await;
        assert_eq!(
            status_ids(&body),
            [
                third["id"].as_str().unwrap().to_owned(),
                second["id"].as_str().unwrap().to_owned(),
            ]
        );

        let next = context
            .authenticated_get(
                &format!("/api/v1/favourites?limit=2&max_id={next_cursor}"),
                &token,
            )
            .await;
        assert_eq!(next.status(), StatusCode::OK);
        assert!(next.headers().get(header::LINK).is_none());
        let body = json_body(next).await;
        assert_eq!(
            status_ids(&body),
            [first["id"].as_str().unwrap().to_owned()]
        );
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Verifies that public timelines preserve viewer-specific favourite state.
    async fn public_timeline_marks_statuses_favourited_by_the_viewer(context: &mut StatusContext) {
        let token = context.access_token().await;
        let status = context
            .create_status(&token, "public favourite", Some("public"), None)
            .await;
        let status_id = status["id"].as_str().unwrap();

        let favourite = context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/statuses/{status_id}/favourite"),
                &token,
            )
            .await;
        assert_eq!(favourite.status(), StatusCode::OK);

        let anonymous = json_body(
            context
                .get("/api/v1/timelines/public?limit=30&local=true")
                .await,
        )
        .await;
        assert_eq!(anonymous[0]["id"], status_id);
        assert_eq!(anonymous[0]["favourited"], false);
        assert_eq!(anonymous[0]["favourites_count"], 1);

        let authenticated = json_body(
            context
                .authenticated_get("/api/v1/timelines/public?limit=30&local=true", &token)
                .await,
        )
        .await;
        assert_eq!(authenticated[0]["id"], status_id);
        assert_eq!(authenticated[0]["favourited"], true);
        assert_eq!(authenticated[0]["favourites_count"], 1);
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Verifies that favourite permissions use the same policy as status reads.
    async fn favourites_follow_status_visibility(context: &mut StatusContext) {
        let owner_token = context.access_token().await;
        let other_token = context.access_token_for("other", "other@example.com").await;
        let status = context
            .create_status(&owner_token, "private", Some("private"), None)
            .await;
        let status_id = status["id"].as_str().unwrap();

        let forbidden = context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/statuses/{status_id}/favourite"),
                &other_token,
            )
            .await;
        assert_eq!(forbidden.status(), StatusCode::NOT_FOUND);

        let other = roosty_db::find_local_account_by_username(&context.db, "other")
            .await
            .unwrap()
            .unwrap();
        roosty_db::follow_local_account(&context.db, other.id, context.account_id, true, false)
            .await
            .unwrap();
        let follower_favourite = context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/statuses/{status_id}/favourite"),
                &other_token,
            )
            .await;
        assert_eq!(follower_favourite.status(), StatusCode::OK);
        let home = json_body(
            context
                .authenticated_get("/api/v1/timelines/home", &other_token)
                .await,
        )
        .await;
        assert!(
            home.as_array()
                .unwrap()
                .iter()
                .any(|item| item["id"] == status_id)
        );
        let profile = json_body(
            context
                .authenticated_get(
                    &format!("/api/v1/accounts/{}/statuses", context.account_id.0),
                    &other_token,
                )
                .await,
        )
        .await;
        assert!(
            profile
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["id"] == status_id)
        );
        roosty_db::unfollow_local_account(&context.db, other.id, context.account_id)
            .await
            .unwrap();
        assert_eq!(
            context
                .authenticated_get(&format!("/api/v1/statuses/{status_id}"), &other_token)
                .await
                .status(),
            StatusCode::NOT_FOUND
        );

        let owner = context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/statuses/{status_id}/favourite"),
                &owner_token,
            )
            .await;
        assert_eq!(owner.status(), StatusCode::OK);
        assert_eq!(json_body(owner).await["favourited"], true);
    }

    /// Explicitly mentioned accounts retain follower-only access without following the author.
    #[test_context(StatusContext)]
    #[tokio::test]
    async fn private_mentions_are_visible_in_lookup_profile_and_home(context: &mut StatusContext) {
        let owner_token = context.access_token().await;
        let mentioned_token = context
            .access_token_for("mentionedprivate", "mentioned-private@example.com")
            .await;
        let unrelated_token = context
            .access_token_for("unrelated-private", "unrelated-private@example.com")
            .await;
        let status = context
            .create_status(
                &owner_token,
                "hello @mentionedprivate",
                Some("private"),
                None,
            )
            .await;
        let status_id = status["id"].as_str().unwrap();

        assert_eq!(
            context
                .authenticated_get(&format!("/api/v1/statuses/{status_id}"), &mentioned_token)
                .await
                .status(),
            StatusCode::OK
        );
        let profile = json_body(
            context
                .authenticated_get(
                    &format!("/api/v1/accounts/{}/statuses", context.account_id.0),
                    &mentioned_token,
                )
                .await,
        )
        .await;
        assert!(
            profile
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["id"] == status_id)
        );
        let home = json_body(
            context
                .authenticated_get("/api/v1/timelines/home", &mentioned_token)
                .await,
        )
        .await;
        assert!(
            home.as_array()
                .unwrap()
                .iter()
                .any(|item| item["id"] == status_id)
        );
        assert_eq!(
            context
                .authenticated_get(&format!("/api/v1/statuses/{status_id}"), &unrelated_token)
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Given a private status, when boosting or listing boosts, then status read visibility is enforced.
    async fn reblogs_follow_status_visibility(context: &mut StatusContext) {
        let owner_token = context.access_token().await;
        let other_token = context
            .access_token_for("other-reblog", "other-reblog@example.com")
            .await;
        let status = context
            .create_status(&owner_token, "private boost", Some("private"), None)
            .await;
        let status_id = status["id"].as_str().unwrap();

        let other = roosty_db::find_local_account_by_username(&context.db, "other-reblog")
            .await
            .unwrap()
            .unwrap();
        roosty_db::follow_local_account(&context.db, other.id, context.account_id, true, false)
            .await
            .unwrap();

        let forbidden = context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/statuses/{status_id}/reblog"),
                &other_token,
            )
            .await;
        let anonymous_reblogged_by = context
            .get(&format!("/api/v1/statuses/{status_id}/reblogged_by"))
            .await;
        let owner = context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/statuses/{status_id}/reblog"),
                &owner_token,
            )
            .await;

        assert_eq!(forbidden.status(), StatusCode::BAD_REQUEST);
        assert_eq!(anonymous_reblogged_by.status(), StatusCode::NOT_FOUND);
        assert_eq!(owner.status(), StatusCode::OK);
        assert_eq!(json_body(owner).await["reblogged"], true);
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Verifies bookmark toggles and collection listing follow Mastodon shapes.
    async fn bookmarks_are_idempotent_and_update_status_fields(context: &mut StatusContext) {
        let token = context.access_token().await;
        let status = context
            .create_status(&token, "bookmark me", None, None)
            .await;
        let status_id = status["id"].as_str().unwrap();

        let first = context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/statuses/{status_id}/bookmark"),
                &token,
            )
            .await;
        assert_eq!(first.status(), StatusCode::OK);
        let first = json_body(first).await;
        assert_eq!(first["bookmarked"], true);

        let second = context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/statuses/{status_id}/bookmark"),
                &token,
            )
            .await;
        assert_eq!(second.status(), StatusCode::OK);
        assert_eq!(json_body(second).await["bookmarked"], true);

        let bookmarks = json_body(
            context
                .authenticated_get("/api/v1/bookmarks?limit=30", &token)
                .await,
        )
        .await;
        assert_eq!(bookmarks.as_array().unwrap().len(), 1);
        assert_eq!(bookmarks[0]["id"], status_id);
        assert_eq!(bookmarks[0]["bookmarked"], true);

        let anonymous =
            json_body(context.get(&format!("/api/v1/statuses/{status_id}")).await).await;
        assert_eq!(anonymous["bookmarked"], false);

        let unbookmark = context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/statuses/{status_id}/unbookmark"),
                &token,
            )
            .await;
        assert_eq!(unbookmark.status(), StatusCode::OK);
        assert_eq!(json_body(unbookmark).await["bookmarked"], false);
        let bookmarks =
            json_body(context.authenticated_get("/api/v1/bookmarks", &token).await).await;
        assert_eq!(bookmarks, serde_json::json!([]));
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Verifies bookmarks expose Mastodon cursor pagination through Link headers.
    async fn bookmarks_collection_uses_cursor_pagination(context: &mut StatusContext) {
        let token = context.access_token().await;
        let first = context.create_status(&token, "first", None, None).await;
        let second = context.create_status(&token, "second", None, None).await;
        let third = context.create_status(&token, "third", None, None).await;
        for status in [&first, &second, &third] {
            let status_id = status["id"].as_str().unwrap();
            let response = context
                .authenticated_empty(
                    "POST",
                    &format!("/api/v1/statuses/{status_id}/bookmark"),
                    &token,
                )
                .await;
            assert_eq!(response.status(), StatusCode::OK);
        }

        let page = context
            .authenticated_get("/api/v1/bookmarks?limit=2", &token)
            .await;
        assert_eq!(page.status(), StatusCode::OK);
        let next_cursor = link_cursor(&page, "next", "max_id");
        let body = json_body(page).await;
        assert_eq!(
            status_ids(&body),
            [
                third["id"].as_str().unwrap().to_owned(),
                second["id"].as_str().unwrap().to_owned(),
            ]
        );

        let next = context
            .authenticated_get(
                &format!("/api/v1/bookmarks?limit=2&max_id={next_cursor}"),
                &token,
            )
            .await;
        assert_eq!(next.status(), StatusCode::OK);
        assert!(next.headers().get(header::LINK).is_none());
        let body = json_body(next).await;
        assert_eq!(
            status_ids(&body),
            [first["id"].as_str().unwrap().to_owned()]
        );
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Verifies malformed collection cursors are rejected before database access.
    async fn status_collections_reject_invalid_cursors(context: &mut StatusContext) {
        let token = context.access_token().await;
        let response = context
            .authenticated_get("/api/v1/favourites?max_id=not-a-uuid", &token)
            .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    /// Verifies bookmark permissions use the same policy as status reads.
    async fn bookmarks_follow_status_visibility(context: &mut StatusContext) {
        let owner_token = context.access_token().await;
        let other_token = context.access_token_for("other", "other@example.com").await;
        let status = context
            .create_status(&owner_token, "private", Some("private"), None)
            .await;
        let status_id = status["id"].as_str().unwrap();

        let forbidden = context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/statuses/{status_id}/bookmark"),
                &other_token,
            )
            .await;
        assert_eq!(forbidden.status(), StatusCode::NOT_FOUND);

        let owner = context
            .authenticated_empty(
                "POST",
                &format!("/api/v1/statuses/{status_id}/bookmark"),
                &owner_token,
            )
            .await;
        assert_eq!(owner.status(), StatusCode::OK);
        assert_eq!(json_body(owner).await["bookmarked"], true);
    }

    #[test]
    fn status_helpers_match_mastodon_compatibility_shapes() {
        // These helpers are intentionally tiny, but they define externally
        // visible timeline sizing and HTML escaping behavior.
        assert_eq!(timeline_limit(None), 20);
        assert_eq!(timeline_limit(Some(0)), 1);
        assert_eq!(timeline_limit(Some(100)), 40);
        assert_eq!(escape_html("<&>'\""), "&lt;&amp;&gt;&#39;&quot;");
        assert_eq!(status_content_html("a\nb"), "<p>a<br />b</p>");
        assert_eq!(
            mention_usernames("@alice test x@y @bo_b"),
            ["alice", "bo_b"]
        );
        assert_eq!(
            mention_usernames("@alice@example.test"),
            Vec::<String>::new()
        );
        assert_eq!(
            remote_mention_matches("hello @alice@example.test!")
                .into_iter()
                .map(|mention| format!("{}@{}", mention.username, mention.domain))
                .collect::<Vec<_>>(),
            ["alice@example.test"]
        );
        assert_eq!(
            hashtag_names("#Rust text##ignored word#skip #web_dev"),
            ["rust", "ignored", "web_dev"]
        );
    }

    /// Build a small valid image fixture for media upload compatibility tests.
    fn encoded_test_image(format: ImageFormat) -> Vec<u8> {
        encoded_sized_test_image(format, 3, 2)
    }

    /// Build a valid image fixture with caller-controlled dimensions.
    fn encoded_sized_test_image(format: ImageFormat, width: u32, height: u32) -> Vec<u8> {
        let image = ImageBuffer::from_fn(width, height, |x, y| {
            if (x + y) % 2 == 0 {
                Rgba([220_u8, 20, 60, 255])
            } else {
                Rgba([20_u8, 80, 220, 255])
            }
        });
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, format).unwrap();
        bytes.into_inner()
    }

    /// Extract status identifiers from a Mastodon status collection response.
    fn status_ids(body: &Value) -> Vec<String> {
        body.as_array()
            .unwrap()
            .iter()
            .map(|status| status["id"].as_str().unwrap().to_owned())
            .collect()
    }

    /// Extract hashtag names from a Mastodon status response.
    fn status_tag_names(status: &Value) -> Vec<String> {
        status["tags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tag| tag["name"].as_str().unwrap().to_owned())
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

    fn notification_projection(notification: &Value) -> Value {
        serde_json::json!({
            "type": notification["type"],
            "account": notification["account"]["username"],
            "status": notification.get("status").map(|status| status["id"].clone()),
        })
    }

    fn status_interaction_projection(status: &Value) -> Value {
        serde_json::json!({
            "reblogged": status["reblogged"],
            "reblogs_count": status["reblogs_count"],
            "favourited": status["favourited"],
            "favourites_count": status["favourites_count"],
        })
    }

    fn reblog_projection(status: &Value) -> Value {
        serde_json::json!({
            "account": status["account"]["username"],
            "reblogged": status["reblogged"],
            "reblog": {
                "id": status["reblog"]["id"],
                "reblogged": status["reblog"]["reblogged"],
                "reblogs_count": status["reblog"]["reblogs_count"],
            }
        })
    }

    fn account_usernames(accounts: &Value) -> Value {
        Value::Array(
            accounts
                .as_array()
                .unwrap()
                .iter()
                .map(|account| account["username"].clone())
                .collect(),
        )
    }

    enum MultipartPart<'a> {
        Text {
            name: &'a str,
            value: &'a str,
        },
        File {
            name: &'a str,
            filename: &'a str,
            content_type: &'a str,
            bytes: &'a [u8],
        },
    }

    impl<'a> MultipartPart<'a> {
        fn text(name: &'a str, value: &'a str) -> Self {
            Self::Text { name, value }
        }

        fn file(name: &'a str, filename: &'a str, content_type: &'a str, bytes: &'a [u8]) -> Self {
            Self::File {
                name,
                filename,
                content_type,
                bytes,
            }
        }
    }

    struct StatusContext {
        postgresql: PostgreSQL,
        db: roosty_db::DbConnection,
        config: Config,
        state: AppState,
        account_id: AccountId,
        application_id: uuid::Uuid,
        _temp_dir: TempDir,
    }

    impl AsyncTestContext for StatusContext {
        async fn setup() -> Self {
            let temp_dir = tempfile::Builder::new()
                .prefix("roosty-status-")
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

            let password_hash = password::hash_password("password").unwrap();
            let account_id = AccountId(
                roosty_db::create_bootstrap_admin(
                    &db,
                    "admin",
                    "admin@example.com",
                    &password_hash,
                )
                .await
                .unwrap(),
            );
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
                media_root: temp_dir.path().join("media").to_string_lossy().to_string(),
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
                config,
                account_id,
                application_id: application.id,
                _temp_dir: temp_dir,
            }
        }

        async fn teardown(self) {
            let StatusContext {
                postgresql,
                db,
                state,
                ..
            } = self;
            let AppState { db: state_db, .. } = state;

            state_db.close().await.unwrap();
            db.close().await.unwrap();
            postgresql.stop().await.unwrap();
        }
    }

    impl StatusContext {
        fn app(&self) -> Router {
            crate::http::app_router(self.state.clone(), false)
        }

        async fn request(&self, request: Request<Body>) -> axum::http::Response<Body> {
            self.app().oneshot(request).await.unwrap()
        }

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

        async fn json(
            &self,
            method: &str,
            uri: &str,
            body: serde_json::Value,
        ) -> axum::http::Response<Body> {
            self.request(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
        }

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

        async fn authenticated_multipart(
            &self,
            uri: &str,
            token: &str,
            parts: &[MultipartPart<'_>],
        ) -> axum::http::Response<Body> {
            self.authenticated_multipart_method("POST", uri, token, parts)
                .await
        }

        async fn authenticated_multipart_method(
            &self,
            method: &str,
            uri: &str,
            token: &str,
            parts: &[MultipartPart<'_>],
        ) -> axum::http::Response<Body> {
            let boundary = "roosty-test-boundary";
            let mut body = Vec::new();
            for part in parts {
                body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
                match part {
                    MultipartPart::Text { name, value } => {
                        body.extend_from_slice(
                            format!(
                                "Content-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
                            )
                            .as_bytes(),
                        );
                    }
                    MultipartPart::File {
                        name,
                        filename,
                        content_type,
                        bytes,
                    } => {
                        body.extend_from_slice(
                            format!(
                                "Content-Disposition: form-data; name=\"{name}\"; filename=\"{filename}\"\r\nContent-Type: {content_type}\r\n\r\n"
                            )
                            .as_bytes(),
                        );
                        body.extend_from_slice(bytes);
                        body.extend_from_slice(b"\r\n");
                    }
                }
            }
            body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());

            self.request(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
        }

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

        async fn access_token(&self) -> String {
            roosty_db::create_access_token(
                &self.db,
                &self.config.token_pepper,
                self.account_id,
                self.application_id,
                "read write follow push",
            )
            .await
            .unwrap()
            .token
        }

        async fn access_token_for(&self, username: &str, email: &str) -> String {
            let password_hash = password::hash_password("password").unwrap();
            let account_id = AccountId(
                roosty_db::create_local_account(&self.db, username, email, &password_hash)
                    .await
                    .unwrap(),
            );
            roosty_db::create_access_token(
                &self.db,
                &self.config.token_pepper,
                account_id,
                self.application_id,
                "read write follow push",
            )
            .await
            .unwrap()
            .token
        }

        async fn create_status(
            &self,
            token: &str,
            status: &str,
            visibility: Option<&str>,
            in_reply_to_id: Option<&str>,
        ) -> Value {
            let mut body = serde_json::json!({ "status": status });
            if let Some(visibility) = visibility {
                body["visibility"] = serde_json::json!(visibility);
            }
            if let Some(in_reply_to_id) = in_reply_to_id {
                body["in_reply_to_id"] = serde_json::json!(in_reply_to_id);
            }

            let response = self
                .authenticated_json("POST", "/api/v1/statuses", token, body)
                .await;
            assert_eq!(response.status(), StatusCode::OK);
            json_body(response).await
        }

        async fn cache_remote_actor(&self, username: &str) -> RemoteActor {
            let now = OffsetDateTime::now_utc();
            let actor_url = format!("https://remote.test/users/{username}");
            let actor = RemoteActor {
                id: AccountId(Uuid::now_v7()),
                activitypub_id: actor_url.clone(),
                username: username.to_owned(),
                domain: "remote.test".to_owned(),
                display_name: "Remote Alice".to_owned(),
                summary: String::new(),
                emojis: json!([]),
                inbox_url: format!("{actor_url}/inbox"),
                shared_inbox_url: None,
                followers_url: Some(format!("{actor_url}/followers")),
                public_key_id: format!("{actor_url}#main-key"),
                public_key_pem: "test-public-key".to_owned(),
                expires_at: now + TimeDuration::hours(1),
                profile_created_at: None,
                first_seen_at: now,
                deleted_at: None,
                moved_to_remote_actor_id: None,
            };
            roosty_db::upsert_remote_actor(&self.db, &actor)
                .await
                .unwrap()
        }

        async fn cache_remote_status(
            &self,
            actor: &RemoteActor,
            content: &str,
            parent: Option<StatusContextParent>,
        ) -> RemoteStatus {
            let now = OffsetDateTime::now_utc();
            let activitypub_id = format!(
                "https://remote.test/users/{}/statuses/{}",
                actor.username,
                Uuid::now_v7()
            );
            let (in_reply_to, in_reply_to_local_status_id, in_reply_to_remote_status_id) =
                match parent {
                    Some(StatusContextParent::Local(status_id)) => (
                        Some(format!(
                            "https://localhost:4000/users/admin/statuses/{}",
                            status_id.0
                        )),
                        Some(status_id),
                        None,
                    ),
                    Some(StatusContextParent::Remote(status_id)) => (
                        Some(format!("https://remote.test/statuses/{}", status_id.0)),
                        None,
                        Some(status_id),
                    ),
                    None => (None, None, None),
                };

            roosty_db::upsert_remote_status(
                &self.db,
                NewRemoteStatus {
                    activitypub_id,
                    remote_actor_id: actor.id,
                    content: content.to_owned(),
                    visibility: StatusVisibility::Public,
                    published_at: now,
                    updated_at: now,
                    in_reply_to,
                    in_reply_to_local_status_id,
                    in_reply_to_remote_status_id,
                    object: json!({}),
                },
            )
            .await
            .unwrap()
        }
    }

    async fn json_body(response: axum::http::Response<Body>) -> Value {
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    fn unique_name() -> String {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();

        format!("roosty_status_{}_{}", std::process::id(), timestamp)
    }
}
