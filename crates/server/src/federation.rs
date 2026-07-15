//! ActivityPub discovery and public-object endpoints for local actors.

pub(crate) mod discovery;
#[cfg(test)]
mod test_transport;

use axum::{
    Json, Router,
    body::to_bytes,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use rand_core::{OsRng, RngCore};
use ring::{aead, digest};
use roosty_core::{AccountId, RoostyError, StatusId};
use roosty_db::{NewRemoteCustomEmoji, RemoteConversationParticipant, StatusVisibility};
use rsa::{
    RsaPrivateKey,
    pkcs1v15::SigningKey,
    pkcs1v15::{Signature as RsaSignature, VerifyingKey},
    pkcs8::{DecodePrivateKey, DecodePublicKey, EncodePrivateKey, EncodePublicKey, LineEnding},
    signature::{SignatureEncoding, Signer, Verifier},
};
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

use crate::http::AppState;

const ACTIVITYSTREAMS_CONTENT_TYPE: &str = "application/activity+json";
const JRD_CONTENT_TYPE: &str = "application/jrd+json";
const ACTIVITYSTREAMS_CONTEXT: &str = "https://www.w3.org/ns/activitystreams";
const PUBLIC_AUDIENCE: &str = "https://www.w3.org/ns/activitystreams#Public";

/// ActivityStreams actor types accepted and emitted by Roosty.
#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
enum ActorType {
    Person,
    Service,
}

/// ActivityStreams object types emitted for local statuses.
#[derive(Serialize)]
enum NoteType {
    Note,
}

/// ActivityStreams activity types emitted for local status publication.
#[derive(Serialize)]
enum CreateType {
    Create,
}

/// ActivityStreams activity types emitted when an existing status changes.
#[derive(Serialize)]
enum UpdateType {
    Update,
}

/// ActivityStreams activity types emitted when a status is removed.
#[derive(Serialize)]
enum DeleteType {
    Delete,
}

/// Activity types that carry a remote Note object in an inbox request.
#[derive(Clone, Copy, Deserialize)]
enum InboundStatusType {
    Create,
    Update,
}

/// Signed remote Create or Update activity containing a Note.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InboundStatusActivity {
    r#type: InboundStatusType,
    actor: String,
    object: InboundNote,
}

/// ActivityPub reference forms accepted for a Follow target.
#[derive(Deserialize)]
#[serde(untagged)]
enum InboundActorReference {
    Id(String),
    Object { id: String },
}

impl InboundActorReference {
    /// Return the canonical actor identity carried by this reference.
    fn id(self) -> String {
        match self {
            Self::Id(id) | Self::Object { id } => id,
        }
    }
}

/// ActivityPub Follow fields needed for a local subscription request.
#[derive(Deserialize)]
struct InboundFollowActivity {
    object: InboundActorReference,
}

/// ActivityPub Undo object forms accepted for a prior Follow activity.
#[derive(Deserialize)]
#[serde(untagged)]
enum InboundUndoFollowObject {
    Id(String),
    Follow { id: String, r#type: String },
}

impl InboundUndoFollowObject {
    /// Return the original Follow activity ID only for an embedded Follow object.
    fn follow_id(self) -> Option<String> {
        match self {
            Self::Id(id) => Some(id),
            Self::Follow { id, r#type } if r#type == "Follow" => Some(id),
            Self::Follow { .. } => None,
        }
    }
}

/// ActivityPub Undo fields needed to revoke a remote subscription.
#[derive(Deserialize)]
struct InboundUndoFollowActivity {
    object: InboundUndoFollowObject,
}

/// ActivityPub Like fields accepted from a signed remote inbox.
#[derive(Deserialize)]
struct InboundLikeActivity {
    actor: String,
    object: String,
}

/// ActivityPub Undo fields used to revoke a Like.
#[derive(Deserialize)]
struct InboundUndoLikeActivity {
    object: InboundUndoLikeObject,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum InboundUndoLikeObject {
    Id(String),
    Like { id: String, r#type: InboundLikeType },
}

/// Closed ActivityStreams type accepted in an embedded Undo(Like) object.
#[derive(Deserialize, PartialEq)]
enum InboundLikeType {
    Like,
}

impl InboundUndoLikeObject {
    /// Return an embedded Like activity ID only when its type is correct.
    fn like_id(self) -> Option<String> {
        match self {
            Self::Id(id) => Some(id),
            Self::Like {
                id,
                r#type: InboundLikeType::Like,
            } => Some(id),
        }
    }
}

/// ActivityPub Announce fields accepted from a signed remote inbox.
#[derive(Deserialize)]
struct InboundAnnounceActivity {
    actor: String,
    object: String,
}

/// ActivityPub Undo fields used to revoke an Announce.
#[derive(Deserialize)]
struct InboundUndoAnnounceActivity {
    object: InboundUndoAnnounceObject,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum InboundUndoAnnounceObject {
    Id(String),
    Announce {
        id: String,
        r#type: InboundAnnounceType,
    },
}

/// Closed ActivityStreams type accepted in an embedded Undo(Announce) object.
#[derive(Deserialize, PartialEq)]
enum InboundAnnounceType {
    Announce,
}

impl InboundUndoAnnounceObject {
    /// Return an embedded Announce activity ID only when its type is correct.
    fn announce_id(self) -> Option<String> {
        match self {
            Self::Id(id) => Some(id),
            Self::Announce {
                id,
                r#type: InboundAnnounceType::Announce,
            } => Some(id),
        }
    }
}

/// Remote Note fields needed for the first cache projection.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct InboundNote {
    id: String,
    r#type: String,
    attributed_to: String,
    content: String,
    published: String,
    updated: Option<String>,
    #[serde(default)]
    to: Vec<String>,
    #[serde(default)]
    cc: Vec<String>,
    #[serde(default)]
    in_reply_to: Option<String>,
    #[serde(default)]
    tag: Vec<InboundTag>,
    #[serde(default)]
    attachment: Vec<InboundAttachment>,
}

/// Attachment metadata declared by a remote ActivityPub Note.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct InboundAttachment {
    #[serde(rename = "type")]
    r#type: String,
    media_type: Option<String>,
    url: Option<JsonValue>,
    name: Option<String>,
}

impl InboundAttachment {
    fn url(&self) -> Option<String> {
        match self.url.as_ref()? {
            JsonValue::String(url) => Some(url.clone()),
            JsonValue::Object(object) => object
                .get("href")
                .and_then(JsonValue::as_str)
                .map(str::to_owned),
            _ => None,
        }
    }
}

/// ActivityPub tag fields retained from an inbound Note.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct InboundTag {
    r#type: InboundTagType,
    href: Option<String>,
    name: Option<String>,
    #[serde(default)]
    icon: Option<InboundEmojiIcon>,
}

/// ActivityPub tag types understood by this implementation.
#[derive(Deserialize, Serialize, PartialEq)]
enum InboundTagType {
    Mention,
    Hashtag,
    Emoji,
    #[serde(rename = "http://joinmastodon.org/ns#Emoji")]
    MastodonEmoji,
    #[serde(other)]
    Other,
}

/// Image reference used by Mastodon ActivityPub Emoji tags.
#[derive(Deserialize, Serialize)]
struct InboundEmojiIcon {
    url: JsonValue,
}

impl InboundEmojiIcon {
    fn url(&self) -> Option<String> {
        match &self.url {
            JsonValue::String(url) => Some(url.clone()),
            JsonValue::Object(value) => value
                .get("href")
                .and_then(JsonValue::as_str)
                .map(str::to_owned),
            _ => None,
        }
    }
}

/// Signed remote Delete activity, whose object may be an object ID or a Tombstone.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InboundDeleteActivity {
    actor: String,
    object: InboundDeleteObject,
}

/// ActivityPub Delete object forms accepted by the first remote-status cache.
#[derive(Deserialize)]
#[serde(untagged)]
enum InboundDeleteObject {
    Id(String),
    Tombstone { id: String },
}

/// Signed remote account migration fields.
#[derive(Deserialize)]
struct InboundMoveActivity {
    actor: String,
    object: InboundActorReference,
    target: InboundActorReference,
}

/// ActivityStreams collection types exposed by local actor endpoints.
#[derive(Serialize)]
enum CollectionType {
    Collection,
    OrderedCollection,
}

/// Build opt-in ActivityPub discovery and local actor routes.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/.well-known/webfinger", get(webfinger))
        .route("/users/{username}", get(actor))
        .route("/users/{username}/outbox", get(outbox))
        .route("/users/{username}/followers", get(followers))
        .route("/users/{username}/following", get(following))
        .route("/users/{username}/inbox", post(inbox))
        .route("/inbox", post(inbox))
        .route("/users/{username}/statuses/{status_id}", get(note))
}

#[derive(Deserialize)]
struct WebFingerQuery {
    resource: Option<String>,
}

#[derive(Serialize)]
struct WebFinger {
    subject: String,
    links: Vec<WebFingerLink>,
}

#[derive(Serialize)]
struct WebFingerLink {
    rel: &'static str,
    r#type: &'static str,
    href: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PublicKey {
    id: String,
    owner: String,
    public_key_pem: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Actor {
    #[serde(rename = "@context")]
    context: ActorContext,
    id: String,
    r#type: ActorType,
    preferred_username: String,
    name: String,
    summary: String,
    inbox: String,
    outbox: String,
    followers: String,
    following: String,
    url: String,
    manually_approves_followers: bool,
    discoverable: bool,
    published: String,
    attachment: Vec<ActorProfileField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    icon: Option<ActorImage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image: Option<ActorImage>,
    public_key: PublicKey,
}

/// JSON-LD context for local actors, including Mastodon's profile metadata vocabulary.
#[derive(Serialize)]
struct ActorContext([ActorContextEntry; 3]);

#[derive(Serialize)]
#[serde(untagged)]
enum ActorContextEntry {
    ActivityStreams(&'static str),
    Security(&'static str),
    Extensions(ActorExtensionsContext),
}

#[derive(Serialize)]
struct ActorExtensionsContext {
    #[serde(rename = "manuallyApprovesFollowers")]
    manually_approves_followers: &'static str,
    toot: &'static str,
    discoverable: &'static str,
    schema: &'static str,
    #[serde(rename = "PropertyValue")]
    property_value: &'static str,
    value: &'static str,
}

/// ActivityStreams image reference used for actor avatars and headers.
#[derive(Serialize)]
struct ActorImage {
    r#type: ActorImageType,
    #[serde(rename = "mediaType")]
    media_type: String,
    url: String,
}

/// Closed ActivityStreams type emitted for actor images.
#[derive(Serialize)]
enum ActorImageType {
    Image,
}

/// ActivityStreams `PropertyValue` metadata published on local actor profiles.
#[derive(Serialize)]
struct ActorProfileField {
    r#type: ActorProfileFieldType,
    name: String,
    value: String,
}

#[derive(Serialize)]
enum ActorProfileFieldType {
    PropertyValue,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Note {
    #[serde(rename = "@context")]
    context: &'static str,
    id: String,
    r#type: NoteType,
    attributed_to: String,
    content: String,
    published: String,
    updated: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    in_reply_to: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tag: Vec<MentionTag>,
    to: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    cc: Vec<String>,
}

/// Typed ActivityPub mention tag emitted on locally authored Notes.
#[derive(Serialize)]
struct MentionTag {
    r#type: MentionType,
    href: String,
    name: String,
}

/// ActivityStreams tag type used by this federation slice.
#[derive(Serialize)]
enum MentionType {
    Mention,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Create {
    #[serde(rename = "@context")]
    context: &'static str,
    r#type: CreateType,
    id: String,
    actor: String,
    published: String,
    to: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    cc: Vec<String>,
    object: Note,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Update {
    #[serde(rename = "@context")]
    context: &'static str,
    id: String,
    r#type: UpdateType,
    actor: String,
    to: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    cc: Vec<String>,
    object: Note,
}

/// ActivityPub profile update containing the refreshed local actor document.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ActorUpdate {
    #[serde(rename = "@context")]
    context: &'static str,
    id: String,
    r#type: UpdateType,
    actor: String,
    to: Vec<String>,
    object: Actor,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Delete {
    #[serde(rename = "@context")]
    context: &'static str,
    id: String,
    r#type: DeleteType,
    actor: String,
    to: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    cc: Vec<String>,
    object: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OrderedCollection {
    #[serde(rename = "@context")]
    context: &'static str,
    r#type: CollectionType,
    total_items: u64,
    ordered_items: Vec<Create>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Collection {
    #[serde(rename = "@context")]
    context: &'static str,
    id: String,
    r#type: CollectionType,
    total_items: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    first: Option<String>,
}

#[derive(Serialize)]
struct OrderedCollectionPage {
    #[serde(rename = "@context")]
    context: &'static str,
    id: String,
    r#type: CollectionType,
    ordered_items: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next: Option<String>,
}

#[derive(Deserialize)]
struct CollectionQuery {
    page: Option<bool>,
    max_id: Option<Uuid>,
}

/// Serve a local WebFinger identity. Remote and malformed resources are never resolved here.
async fn webfinger(State(state): State<AppState>, Query(query): Query<WebFingerQuery>) -> Response {
    if !state.config.federation_enabled {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some((username, domain)) = query.resource.as_deref().and_then(parse_acct) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if state.config.public_base_url.host_str() != Some(domain) {
        return StatusCode::NOT_FOUND.into_response();
    }
    match roosty_db::find_local_account_by_username(&state.db, username).await {
        Ok(Some(_)) => {
            let subject = format!("acct:{username}@{domain}");
            (
                [(header::CONTENT_TYPE, JRD_CONTENT_TYPE)],
                Json(WebFinger {
                    subject,
                    links: vec![WebFingerLink {
                        rel: "self",
                        r#type: ACTIVITYSTREAMS_CONTENT_TYPE,
                        href: actor_url(&state, username),
                    }],
                }),
            )
                .into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => internal_error(error),
    }
}

/// Serve one local actor with a persisted public signing key.
async fn actor(State(state): State<AppState>, Path(username): Path<String>) -> Response {
    if !state.config.federation_enabled {
        return StatusCode::NOT_FOUND.into_response();
    }
    let account = match roosty_db::find_local_account_by_username(&state.db, &username).await {
        Ok(Some(account)) => account,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(error) => return internal_error(error),
    };
    let public_key_pem = match ensure_actor_key(&state, account.id).await {
        Ok(key) => key,
        Err(error) => return internal_error(error),
    };
    activity_response(actor_document(&state, account, public_key_pem))
}

/// Build the canonical public actor document used for direct reads and Update activities.
fn actor_document(
    state: &AppState,
    account: roosty_db::LocalAccount,
    public_key_pem: String,
) -> Actor {
    let id = actor_url(state, &account.username);
    Actor {
        context: actor_context(),
        id: id.clone(),
        r#type: local_actor_type(account.bot),
        preferred_username: account.username.clone(),
        name: if account.display_name.is_empty() {
            account.username.clone()
        } else {
            account.display_name
        },
        summary: account.note,
        inbox: format!("{id}/inbox"),
        outbox: format!("{id}/outbox"),
        followers: format!("{id}/followers"),
        following: format!("{id}/following"),
        url: public_url(state, &format!("@{}", account.username)),
        manually_approves_followers: account.locked,
        discoverable: account.discoverable,
        published: crate::statuses::format_timestamp(account.created_at),
        attachment: actor_profile_fields(&account.profile_fields),
        icon: account.avatar_file_path.as_deref().map(|path| ActorImage {
            r#type: ActorImageType::Image,
            media_type: crate::media::media_content_type(path).to_owned(),
            url: crate::media::media_url(state, path),
        }),
        image: account.header_file_path.as_deref().map(|path| ActorImage {
            r#type: ActorImageType::Image,
            media_type: crate::media::media_content_type(path).to_owned(),
            url: crate::media::media_url(state, path),
        }),
        public_key: PublicKey {
            id: format!("{id}#main-key"),
            owner: id,
            public_key_pem,
        },
    }
}

/// Map Roosty's local bot setting to the ActivityPub actor type Mastodon uses for services.
fn local_actor_type(bot: bool) -> ActorType {
    if bot {
        ActorType::Service
    } else {
        ActorType::Person
    }
}

/// Build the actor JSON-LD context required for Schema.org profile fields.
fn actor_context() -> ActorContext {
    ActorContext([
        ActorContextEntry::ActivityStreams(ACTIVITYSTREAMS_CONTEXT),
        ActorContextEntry::Security("https://w3id.org/security/v1"),
        ActorContextEntry::Extensions(ActorExtensionsContext {
            manually_approves_followers: "as:manuallyApprovesFollowers",
            toot: "http://joinmastodon.org/ns#",
            discoverable: "toot:discoverable",
            schema: "http://schema.org#",
            property_value: "schema:PropertyValue",
            value: "schema:value",
        }),
    ])
}

/// Convert persisted Mastodon profile fields to ActivityStreams `PropertyValue` attachments.
fn actor_profile_fields(profile_fields: &JsonValue) -> Vec<ActorProfileField> {
    profile_fields
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|field| {
            Some(ActorProfileField {
                r#type: ActorProfileFieldType::PropertyValue,
                name: field.get("name")?.as_str()?.to_owned(),
                value: crate::statuses::escape_html(field.get("value")?.as_str()?),
            })
        })
        .collect()
}

/// Serve the local actor's public outbox as an ordered ActivityStreams collection.
async fn outbox(State(state): State<AppState>, Path(username): Path<String>) -> Response {
    if !state.config.federation_enabled {
        return StatusCode::NOT_FOUND.into_response();
    }
    let account = match roosty_db::find_local_account_by_username(&state.db, &username).await {
        Ok(Some(account)) => account,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(error) => return internal_error(error),
    };
    match roosty_db::public_local_statuses_by_account(&state.db, account.id, 20).await {
        Ok(statuses) => {
            let mut items = Vec::with_capacity(statuses.len());
            for status in statuses {
                match create(&state, &account.username, status).await {
                    Ok(item) => items.push(item),
                    Err(error) => return internal_error(error),
                }
            }
            match roosty_db::count_public_local_statuses_by_account(&state.db, account.id).await {
                Ok(total_items) => activity_response(OrderedCollection {
                    context: ACTIVITYSTREAMS_CONTEXT,
                    r#type: CollectionType::OrderedCollection,
                    total_items,
                    ordered_items: items,
                }),
                Err(error) => internal_error(error),
            }
        }
        Err(error) => internal_error(error),
    }
}

/// Serve a public local status as a Note.
async fn note(
    State(state): State<AppState>,
    Path((username, status_id)): Path<(String, String)>,
) -> Response {
    if !state.config.federation_enabled {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Ok(id) = uuid::Uuid::parse_str(&status_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match roosty_db::find_local_status_by_id(&state.db, StatusId(id)).await {
        Ok(Some(status)) if status.visibility == StatusVisibility::Public => {
            match roosty_db::find_local_account_by_id(&state.db, status.account_id).await {
                Ok(Some(account)) if account.username == username => {
                    match note_object(&state, &username, status).await {
                        Ok(note) => activity_response(note),
                        Err(error) => internal_error(error),
                    }
                }
                Ok(_) => StatusCode::NOT_FOUND.into_response(),
                Err(error) => internal_error(error),
            }
        }
        Ok(_) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => internal_error(error),
    }
}

/// Serve the actor's follower collection metadata without leaking local-only details.
async fn followers(
    State(state): State<AppState>,
    Path(username): Path<String>,
    Query(query): Query<CollectionQuery>,
) -> Response {
    let Some(account) = account_for_collection(&state, &username).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match (
        roosty_db::count_local_followers(&state.db, account.id).await,
        roosty_db::count_remote_followers(&state.db, account.id).await,
    ) {
        (Ok(local), Ok(remote)) if query.page != Some(true) => activity_response(Collection {
            context: ACTIVITYSTREAMS_CONTEXT,
            id: format!("{}/followers", actor_url(&state, &username)),
            r#type: CollectionType::Collection,
            total_items: local + remote,
            first: Some(format!(
                "{}/followers?page=true",
                actor_url(&state, &username)
            )),
        }),
        (Ok(_), Ok(_)) => {
            activity_collection_page(&state, &username, account.id, query.max_id, true).await
        }
        (Err(error), _) | (_, Err(error)) => internal_error(error),
    }
}

/// Serve the actor's following collection metadata without leaking local-only details.
async fn following(
    State(state): State<AppState>,
    Path(username): Path<String>,
    Query(query): Query<CollectionQuery>,
) -> Response {
    let Some(account) = account_for_collection(&state, &username).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match (
        roosty_db::count_local_following(&state.db, account.id).await,
        roosty_db::count_remote_following(&state.db, account.id).await,
    ) {
        (Ok(local), Ok(remote)) if query.page != Some(true) => activity_response(Collection {
            context: ACTIVITYSTREAMS_CONTEXT,
            id: format!("{}/following", actor_url(&state, &username)),
            r#type: CollectionType::Collection,
            total_items: local + remote,
            first: Some(format!(
                "{}/following?page=true",
                actor_url(&state, &username)
            )),
        }),
        (Ok(_), Ok(_)) => {
            activity_collection_page(&state, &username, account.id, query.max_id, false).await
        }
        (Err(error), _) | (_, Err(error)) => internal_error(error),
    }
}

/// Render one public ordered page of a local actor's mixed follow collection.
async fn activity_collection_page(
    state: &AppState,
    username: &str,
    account_id: AccountId,
    max_id: Option<Uuid>,
    followers: bool,
) -> Response {
    let cursor = roosty_db::CollectionCursor {
        max_id,
        ..Default::default()
    };
    let page = if followers {
        roosty_db::followers_for_local_account(&state.db, account_id, 20, cursor).await
    } else {
        roosty_db::following_for_local_account(&state.db, account_id, 20, cursor).await
    };
    match page {
        Ok(page) => {
            let ordered_items = page
                .items
                .into_iter()
                .map(|entry| match entry.account {
                    roosty_db::FollowCollectionAccount::Local(account) => {
                        actor_url(state, &account.username)
                    }
                    roosty_db::FollowCollectionAccount::Remote(actor) => actor.activitypub_id,
                })
                .collect();
            let name = if followers { "followers" } else { "following" };
            let next = page
                .has_more
                .then_some(page.last_cursor)
                .flatten()
                .map(|cursor| {
                    format!(
                        "{}/{name}?page=true&max_id={cursor}",
                        actor_url(state, username),
                    )
                });
            activity_response(OrderedCollectionPage {
                context: ACTIVITYSTREAMS_CONTEXT,
                id: format!("{}/{name}?page=true", actor_url(state, username)),
                r#type: CollectionType::OrderedCollection,
                ordered_items,
                next,
            })
        }
        Err(error) => internal_error(error),
    }
}

async fn account_for_collection(
    state: &AppState,
    username: &str,
) -> Option<roosty_db::LocalAccount> {
    if !state.config.federation_enabled {
        return None;
    }
    match roosty_db::find_local_account_by_username(&state.db, username).await {
        Ok(account) => account,
        Err(error) => {
            tracing::error!(%error, "could not load ActivityPub collection actor");
            None
        }
    }
}

/// Verify and process a remote Follow or Undo(Follow) inbox activity.
async fn inbox(State(state): State<AppState>, request: axum::extract::Request) -> Response {
    if state.config.federation_enabled {
        process_inbox(&state, request).await
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

async fn process_inbox(state: &AppState, request: axum::extract::Request) -> Response {
    let (parts, body) = request.into_parts();
    let body = match to_bytes(body, 1_048_576).await {
        Ok(body) => body,
        Err(_) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    let activity: JsonValue = match serde_json::from_slice(&body) {
        Ok(activity) => activity,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let Some(actor_id) = activity.get("actor").and_then(JsonValue::as_str) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let remote_actor = match discovery::resolve_remote_actor_by_id(state, actor_id).await {
        Ok(actor) => actor,
        Err(error) => {
            tracing::warn!(%error, "rejected remote inbox actor");
            return StatusCode::FORBIDDEN.into_response();
        }
    };
    if !verify_legacy_signature(&parts, &body, &remote_actor).unwrap_or(false) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(activity_id) = activity
        .get("id")
        .and_then(JsonValue::as_str)
        .map(str::to_owned)
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if !activity_id.starts_with("https://") {
        return StatusCode::BAD_REQUEST.into_response();
    }
    if is_remote_actor_lifecycle_activity(&activity, &remote_actor.activitypub_id) {
        return match process_remote_actor_lifecycle(state, &activity_id, &activity, &remote_actor)
            .await
        {
            Ok(()) => StatusCode::ACCEPTED.into_response(),
            Err(error) => {
                tracing::warn!(%error, activity_id, "rejected remote actor lifecycle activity");
                StatusCode::ACCEPTED.into_response()
            }
        };
    }
    if matches!(
        activity.get("type").and_then(JsonValue::as_str),
        Some("Create") | Some("Update") | Some("Delete")
    ) {
        return match process_remote_status_activity(state, &activity_id, &activity, &remote_actor)
            .await
        {
            Ok(change) => {
                if let Err(error) =
                    publish_remote_status_change(state, remote_actor.id, change).await
                {
                    tracing::warn!(%error, activity_id, "could not stream remote status activity");
                }
                StatusCode::ACCEPTED.into_response()
            }
            Err(error) => {
                tracing::warn!(%error, activity_id, "rejected remote status activity");
                StatusCode::ACCEPTED.into_response()
            }
        };
    }
    if matches!(
        activity.get("type").and_then(JsonValue::as_str),
        Some("Accept") | Some("Reject")
    ) {
        let Some(object_id) = activity
            .get("object")
            .and_then(JsonValue::as_object)
            .and_then(|object| object.get("id"))
            .and_then(JsonValue::as_str)
        else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        let result = if activity.get("type").and_then(JsonValue::as_str) == Some("Accept") {
            roosty_db::accept_remote_following(&state.db, remote_actor.id, object_id).await
        } else {
            roosty_db::reject_remote_following(&state.db, remote_actor.id, object_id).await
        };
        return match result {
            Ok(accepted) => {
                if accepted
                    && activity.get("type").and_then(JsonValue::as_str) == Some("Accept")
                    && let Err(error) =
                        crate::media::enqueue_remote_profile_media_fetches(state, remote_actor.id)
                            .await
                {
                    tracing::warn!(%error, "could not queue remote profile media fetches");
                }
                StatusCode::ACCEPTED.into_response()
            }
            Err(error) => internal_error(error),
        };
    }
    if activity.get("type").and_then(JsonValue::as_str) == Some("Like") {
        let like: InboundLikeActivity = match serde_json::from_value(activity.clone()) {
            Ok(like) => like,
            Err(_) => return StatusCode::BAD_REQUEST.into_response(),
        };
        if like.actor != remote_actor.activitypub_id || !like.object.starts_with("https://") {
            return StatusCode::ACCEPTED.into_response();
        }
        let Some(status_id) = local_status_id_from_url(state, &like.object)
            .await
            .ok()
            .flatten()
        else {
            return StatusCode::ACCEPTED.into_response();
        };
        let status = match roosty_db::find_local_status_by_id(&state.db, status_id).await {
            Ok(Some(status))
                if matches!(
                    status.visibility,
                    StatusVisibility::Public | StatusVisibility::Unlisted
                ) =>
            {
                status
            }
            Ok(_) => return StatusCode::ACCEPTED.into_response(),
            Err(error) => return internal_error(error),
        };
        let txn = match state.db.begin().await {
            Ok(txn) => txn,
            Err(error) => return internal_error(error),
        };
        match roosty_db::process_remote_like(
            &txn,
            remote_actor.id,
            status.id,
            &activity_id,
            status.account_id,
        )
        .await
        {
            Ok(notification) => {
                if let Err(error) = txn.commit().await {
                    return internal_error(error);
                }
                if let Some(notification) = notification
                    && let Err(error) = crate::notifications::publish_committed_notification(
                        state,
                        status.account_id,
                        notification,
                    )
                    .await
                {
                    tracing::warn!(%error, activity_id, "could not create remote favourite notification");
                }
                return StatusCode::ACCEPTED.into_response();
            }
            Err(error) => return internal_error(error),
        }
    }
    if activity.get("type").and_then(JsonValue::as_str) == Some("Announce") {
        let announce: InboundAnnounceActivity = match serde_json::from_value(activity.clone()) {
            Ok(announce) => announce,
            Err(_) => return StatusCode::BAD_REQUEST.into_response(),
        };
        if announce.actor != remote_actor.activitypub_id || !announce.object.starts_with("https://")
        {
            return StatusCode::ACCEPTED.into_response();
        }
        let target = match local_status_id_from_url(state, &announce.object).await {
            Ok(Some(status_id)) => {
                match roosty_db::find_local_status_by_id(&state.db, status_id).await {
                    Ok(Some(status))
                        if matches!(
                            status.visibility,
                            StatusVisibility::Public | StatusVisibility::Unlisted
                        ) =>
                    {
                        roosty_db::RemoteStatusReblogTarget::Local(status.id)
                    }
                    Ok(_) => return StatusCode::ACCEPTED.into_response(),
                    Err(error) => return internal_error(error),
                }
            }
            Ok(None) => {
                match roosty_db::find_remote_status_by_activitypub_id(&state.db, &announce.object)
                    .await
                {
                    Ok(Some(status))
                        if matches!(
                            status.visibility,
                            StatusVisibility::Public | StatusVisibility::Unlisted
                        ) =>
                    {
                        roosty_db::RemoteStatusReblogTarget::Remote(status.id)
                    }
                    Ok(_) => return StatusCode::ACCEPTED.into_response(),
                    Err(error) => return internal_error(error),
                }
            }
            Err(error) => return internal_error(error),
        };
        let txn = match state.db.begin().await {
            Ok(txn) => txn,
            Err(error) => return internal_error(error),
        };
        let created = match roosty_db::reblog_status_by_remote_actor(
            &txn,
            remote_actor.id,
            target.clone(),
            &activity_id,
        )
        .await
        {
            Ok(created) => created,
            Err(error) => return internal_error(error),
        };
        let notification = if created {
            if let roosty_db::RemoteStatusReblogTarget::Local(status_id) = target {
                match roosty_db::find_local_status_by_id(&state.db, status_id).await {
                    Ok(Some(status)) => match roosty_db::notify_remote_actor_reblog(
                        &txn,
                        status.account_id,
                        remote_actor.id,
                        status.id,
                    )
                    .await
                    {
                        Ok(notification) => notification,
                        Err(error) => return internal_error(error),
                    },
                    Ok(None) => None,
                    Err(error) => return internal_error(error),
                }
            } else {
                None
            }
        } else {
            None
        };
        if let Err(error) =
            roosty_db::record_processed_inbox_activity(&txn, &activity_id, remote_actor.id).await
        {
            return internal_error(error);
        }
        if let Err(error) = txn.commit().await {
            return internal_error(error);
        }
        if created {
            if let Some(notification) = notification
                && let Err(error) = crate::notifications::publish_committed_notification(
                    state,
                    notification.account_id,
                    notification,
                )
                .await
            {
                tracing::warn!(%error, activity_id, "could not publish remote reblog notification");
            }
            if let Err(error) =
                crate::statuses::publish_remote_reblog_update(state, remote_actor.id, &activity_id)
                    .await
            {
                tracing::warn!(%error, activity_id, "could not stream remote reblog");
            }
        }
        return StatusCode::ACCEPTED.into_response();
    }
    if activity.get("type").and_then(JsonValue::as_str) == Some("Undo")
        && let Ok(undo) = serde_json::from_value::<InboundUndoAnnounceActivity>(activity.clone())
        && let Some(original_id) = undo.object.announce_id()
    {
        let txn = match state.db.begin().await {
            Ok(txn) => txn,
            Err(error) => return internal_error(error),
        };
        match roosty_db::process_remote_undo_reblog(
            &txn,
            remote_actor.id,
            &activity_id,
            &original_id,
        )
        .await
        {
            Ok(Some(reblog)) => {
                if let Err(error) = txn.commit().await {
                    return internal_error(error);
                }
                if let Err(error) =
                    crate::statuses::publish_remote_reblog_delete(state, remote_actor.id, reblog.id)
                        .await
                {
                    tracing::warn!(%error, activity_id, "could not stream remote unboost");
                }
                return StatusCode::ACCEPTED.into_response();
            }
            Ok(None) => match txn.commit().await {
                Ok(()) => return StatusCode::ACCEPTED.into_response(),
                Err(error) => return internal_error(error),
            },
            Err(error) => return internal_error(error),
        }
    }
    if activity.get("type").and_then(JsonValue::as_str) == Some("Undo")
        && let Ok(undo) = serde_json::from_value::<InboundUndoLikeActivity>(activity.clone())
        && let Some(original_id) = undo.object.like_id()
    {
        let txn = match state.db.begin().await {
            Ok(txn) => txn,
            Err(error) => return internal_error(error),
        };
        match roosty_db::process_remote_undo_like(&txn, remote_actor.id, &activity_id, &original_id)
            .await
        {
            Ok(true) | Ok(false) => {
                if let Err(error) = txn.commit().await {
                    return internal_error(error);
                }
                return StatusCode::ACCEPTED.into_response();
            }
            Err(error) => return internal_error(error),
        }
    }
    if !matches!(
        activity.get("type").and_then(JsonValue::as_str),
        Some("Follow") | Some("Undo")
    ) {
        return StatusCode::ACCEPTED.into_response();
    }
    if activity.get("type").and_then(JsonValue::as_str) == Some("Undo") {
        let undo: InboundUndoFollowActivity = match serde_json::from_value(activity.clone()) {
            Ok(undo) => undo,
            Err(_) => return StatusCode::BAD_REQUEST.into_response(),
        };
        let Some(original_id) = undo.object.follow_id() else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        let txn = match state.db.begin().await {
            Ok(txn) => txn,
            Err(error) => return internal_error(error),
        };
        match roosty_db::process_remote_undo_follow(
            &txn,
            remote_actor.id,
            &activity_id,
            &original_id,
        )
        .await
        {
            Ok(_) => match txn.commit().await {
                Ok(()) => {}
                Err(error) => return internal_error(error),
            },
            Err(error) => return internal_error(error),
        }
        return StatusCode::ACCEPTED.into_response();
    }
    let follow: InboundFollowActivity = match serde_json::from_value(activity.clone()) {
        Ok(follow) => follow,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let target_url = follow.object.id();
    let Some(username) = target_url
        .rsplit('/')
        .next()
        .filter(|username| !username.is_empty())
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if target_url != actor_url(state, username) {
        return StatusCode::ACCEPTED.into_response();
    }
    let local_account = match roosty_db::find_local_account_by_username(&state.db, username).await {
        Ok(Some(account)) => account,
        Ok(None) => return StatusCode::ACCEPTED.into_response(),
        Err(error) => return internal_error(error),
    };
    let follow_state = if local_account.locked {
        InboundFollowState::Pending
    } else {
        InboundFollowState::Accepted
    };
    let txn = match state.db.begin().await {
        Ok(txn) => txn,
        Err(error) => return internal_error(error),
    };
    let persisted = if matches!(follow_state, InboundFollowState::Accepted) {
        let payload = match serde_json::to_value(FollowResponseDelivery {
            local_account_id: local_account.id,
            remote_actor_id: remote_actor.id,
            follow: activity.clone(),
            response_type: FollowResponseType::Accept,
        }) {
            Ok(payload) => payload,
            Err(error) => return internal_error(error),
        };
        roosty_db::upsert_processed_remote_follow_with_response_job(
            &txn,
            remote_actor.id,
            local_account.id,
            &activity_id,
            activity.clone(),
            roosty_db::RemoteFollowResponseJob {
                kind: roosty_db::JobKind::FederationFollowResponse,
                payload,
                deduplication_key: format!("{}:{activity_id}", FollowResponseType::Accept.as_str()),
            },
        )
        .await
    } else {
        roosty_db::upsert_processed_pending_remote_follow(
            &txn,
            remote_actor.id,
            local_account.id,
            &activity_id,
            activity.clone(),
        )
        .await
    };
    let persisted = match persisted {
        Ok(persisted) => persisted,
        Err(error) => return internal_error(error),
    };
    let notification = if persisted {
        let notification_type = if matches!(follow_state, InboundFollowState::Pending) {
            roosty_db::LocalNotificationType::FollowRequest
        } else {
            roosty_db::LocalNotificationType::Follow
        };
        match roosty_db::notify_remote_actor_follow(
            &txn,
            local_account.id,
            remote_actor.id,
            notification_type,
        )
        .await
        {
            Ok(notification) => notification,
            Err(error) => return internal_error(error),
        }
    } else {
        None
    };
    if let Err(error) = txn.commit().await {
        return internal_error(error);
    }
    match persisted {
        true => {
            tracing::info!(
                activity_id,
                remote_actor_id = %remote_actor.id.0,
                local_account_id = %local_account.id.0,
                state = follow_state.as_str(),
                "processed remote follow"
            );
            if let Some(notification) = notification
                && let Err(error) = crate::notifications::publish_committed_notification(
                    state,
                    notification.account_id,
                    notification,
                )
                .await
            {
                tracing::warn!(%error, "failed to publish remote follow notification");
            }
            StatusCode::ACCEPTED.into_response()
        }
        false => StatusCode::ACCEPTED.into_response(),
    }
}

/// Identify actor lifecycle activities before the similarly named Note handlers.
fn is_remote_actor_lifecycle_activity(activity: &JsonValue, actor_id: &str) -> bool {
    match activity.get("type").and_then(JsonValue::as_str) {
        Some("Move") => true,
        Some("Update") => activity
            .get("object")
            .and_then(|object| match object {
                JsonValue::String(id) => Some(id == actor_id),
                JsonValue::Object(object) => Some(
                    object
                        .get("id")
                        .and_then(JsonValue::as_str)
                        .is_some_and(|id| id == actor_id)
                        && object.get("type").and_then(JsonValue::as_str) == Some("Person"),
                ),
                _ => None,
            })
            .unwrap_or(false),
        Some("Delete") => activity
            .get("object")
            .and_then(|object| match object {
                JsonValue::String(id) => Some(id == actor_id),
                JsonValue::Object(object) => object
                    .get("id")
                    .and_then(JsonValue::as_str)
                    .map(|id| id == actor_id),
                _ => None,
            })
            .unwrap_or(false),
        _ => false,
    }
}

/// Process a verified remote actor refresh, tombstone, or Move activity.
async fn process_remote_actor_lifecycle(
    state: &AppState,
    activity_id: &str,
    activity: &JsonValue,
    remote_actor: &roosty_db::RemoteActor,
) -> Result<(), RoostyError> {
    match activity.get("type").and_then(JsonValue::as_str) {
        Some("Update") => {
            let object_id = activity
                .get("object")
                .and_then(|object| match object {
                    JsonValue::String(id) => Some(id.as_str()),
                    JsonValue::Object(object) => object.get("id").and_then(JsonValue::as_str),
                    _ => None,
                })
                .ok_or_else(|| {
                    RoostyError::InvalidInput("remote actor Update is invalid".to_owned())
                })?;
            if object_id != remote_actor.activitypub_id {
                return Err(RoostyError::InvalidInput(
                    "remote actor Update does not match signer".to_owned(),
                ));
            }
            let refreshed = discovery::refresh_remote_actor_by_id(state, object_id).await?;
            let txn = state.db.begin().await?;
            roosty_db::record_processed_inbox_activity(&txn, activity_id, refreshed.id).await?;
            txn.commit().await?;
            Ok(())
        }
        Some("Delete") => {
            let delete: InboundDeleteActivity =
                serde_json::from_value(activity.clone()).map_err(|_| {
                    RoostyError::InvalidInput("remote actor Delete is invalid".to_owned())
                })?;
            let object_id = match delete.object {
                InboundDeleteObject::Id(id) | InboundDeleteObject::Tombstone { id } => id,
            };
            if delete.actor != remote_actor.activitypub_id
                || object_id != remote_actor.activitypub_id
            {
                return Err(RoostyError::InvalidInput(
                    "remote actor Delete does not match signer".to_owned(),
                ));
            }
            let txn = state.db.begin().await?;
            roosty_db::process_remote_actor_delete(&txn, activity_id, remote_actor.id).await?;
            txn.commit().await?;
            Ok(())
        }
        Some("Move") => {
            let movement: InboundMoveActivity =
                serde_json::from_value(activity.clone()).map_err(|_| {
                    RoostyError::InvalidInput("remote actor Move is invalid".to_owned())
                })?;
            let source = movement.object.id();
            let target = movement.target.id();
            if movement.actor != remote_actor.activitypub_id
                || source != remote_actor.activitypub_id
            {
                return Err(RoostyError::InvalidInput(
                    "remote actor Move does not match signer".to_owned(),
                ));
            }
            let target = discovery::resolve_remote_move_target(state, &target, &source).await?;
            let txn = state.db.begin().await?;
            roosty_db::process_remote_actor_move(&txn, activity_id, remote_actor.id, target.id)
                .await?;
            txn.commit().await?;
            Ok(())
        }
        _ => Err(RoostyError::InvalidInput(
            "unsupported remote actor lifecycle activity".to_owned(),
        )),
    }
}

/// Cached remote status change that can be published to accepted local followers.
enum RemoteStatusChange {
    /// An idempotent replay with no state or stream effect.
    Ignored,
    /// A newly created or edited Note.
    Upsert(
        Box<roosty_db::RemoteStatus>,
        Vec<roosty_db::LocalNotification>,
        Option<roosty_db::DirectConversationRefresh>,
    ),
    /// A removed Note with its internal API ID.
    Delete(String, Option<roosty_db::DirectConversationRefresh>),
}

/// Resolve a canonical local Note URL without accepting look-alike remote URLs.
async fn local_status_id_from_url(
    state: &AppState,
    activitypub_id: &str,
) -> Result<Option<StatusId>, RoostyError> {
    let prefix = format!(
        "{}/users/",
        state.config.public_base_url.as_str().trim_end_matches('/')
    );
    let Some(path) = activitypub_id.strip_prefix(&prefix) else {
        return Ok(None);
    };
    let Some((username, status_id)) = path.split_once("/statuses/") else {
        return Ok(None);
    };
    if username.is_empty() || status_id.contains('/') {
        return Ok(None);
    }
    let Ok(status_id) = Uuid::parse_str(status_id) else {
        return Ok(None);
    };
    let Some(status) = roosty_db::find_local_status_by_id(&state.db, StatusId(status_id)).await?
    else {
        return Ok(None);
    };
    let Some(account) = roosty_db::find_local_account_by_id(&state.db, status.account_id).await?
    else {
        return Ok(None);
    };
    Ok((account.username == username).then_some(status.id))
}

/// Validate and cache one signed public or unlisted remote status lifecycle activity.
async fn process_remote_status_activity(
    state: &AppState,
    activity_id: &str,
    activity: &JsonValue,
    remote_actor: &roosty_db::RemoteActor,
) -> Result<RemoteStatusChange, RoostyError> {
    match activity.get("type").and_then(JsonValue::as_str) {
        Some("Create") | Some("Update") => {
            let activity_type = activity.get("type").and_then(JsonValue::as_str);
            let activity: InboundStatusActivity = serde_json::from_value(activity.clone())
                .map_err(|_| {
                    RoostyError::InvalidInput("remote status activity is invalid".to_owned())
                })?;
            if !matches!(
                (activity_type, activity.r#type),
                (Some("Create"), InboundStatusType::Create)
                    | (Some("Update"), InboundStatusType::Update)
            ) {
                return Err(RoostyError::InvalidInput(
                    "remote status activity type is invalid".to_owned(),
                ));
            }
            let is_create = matches!(activity.r#type, InboundStatusType::Create);
            let object = serde_json::to_value(&activity.object)
                .map_err(|error| RoostyError::InvalidInput(error.to_string()))?;
            let note = activity.object;
            let attachments = note
                .attachment
                .iter()
                .filter(|attachment| attachment.r#type == "Document")
                .filter_map(|attachment| {
                    attachment
                        .url()
                        .map(|remote_url| roosty_db::NewRemoteMediaAttachment {
                            remote_url,
                            content_type: attachment.media_type.clone(),
                            description: attachment.name.clone(),
                        })
                })
                .collect::<Vec<_>>();
            let emojis = remote_custom_emoji_definitions(&note.tag);
            let mention_urls = note
                .tag
                .iter()
                .filter(|tag| tag.r#type == InboundTagType::Mention)
                .filter_map(|tag| tag.href.clone())
                .collect::<Vec<_>>();
            if activity.actor != remote_actor.activitypub_id
                || note.attributed_to != remote_actor.activitypub_id
                || note.r#type != "Note"
                || !note.id.starts_with("https://")
            {
                return Err(RoostyError::InvalidInput(
                    "remote status activity has an invalid actor or object".to_owned(),
                ));
            }
            let direct_recipients = remote_direct_recipients(state, &note).await?;
            let direct_participants =
                remote_direct_participants(state, &note, remote_actor).await?;
            let visibility = remote_status_visibility(&note)
                .or_else(|| direct_recipients.as_ref().map(|_| "direct"))
                .ok_or_else(|| {
                    RoostyError::InvalidInput("remote Note audience is unsupported".to_owned())
                })?;
            let published_at = OffsetDateTime::parse(&note.published, &Rfc3339).map_err(|_| {
                RoostyError::InvalidInput("remote Note published timestamp is invalid".to_owned())
            })?;
            let updated_at = note
                .updated
                .as_deref()
                .map(|updated| OffsetDateTime::parse(updated, &Rfc3339))
                .transpose()
                .map_err(|_| {
                    RoostyError::InvalidInput("remote Note updated timestamp is invalid".to_owned())
                })?
                .unwrap_or(published_at);
            let in_reply_to_remote_status_id = match note.in_reply_to.as_deref() {
                Some(id) => roosty_db::find_remote_status_by_activitypub_id(&state.db, id)
                    .await?
                    .map(|status| status.id),
                None => None,
            };
            let mut notification_recipients =
                local_mention_recipients(state, &mention_urls).await?;
            if let Some(parent_id) = note.in_reply_to.as_deref()
                && let Some(status_id) = local_status_id_from_url(state, parent_id).await?
                && let Some(parent) =
                    roosty_db::find_local_status_by_id(&state.db, status_id).await?
            {
                notification_recipients.push(parent.account_id);
            }
            if let Some(recipients) = direct_recipients.as_ref() {
                notification_recipients.extend(recipients.iter().copied());
            }
            notification_recipients.sort_by_key(|id| id.0);
            notification_recipients.dedup();
            let txn = state.db.begin().await?;
            roosty_db::upsert_remote_custom_emojis(&txn, &emojis).await?;
            let status = roosty_db::process_remote_status_upsert(
                &txn,
                activity_id,
                remote_actor.id,
                roosty_db::NewRemoteStatus {
                    activitypub_id: note.id,
                    remote_actor_id: remote_actor.id,
                    content: note.content,
                    visibility: StatusVisibility::parse(visibility)?,
                    published_at,
                    updated_at,
                    in_reply_to: note.in_reply_to.clone(),
                    in_reply_to_local_status_id: match note.in_reply_to.as_deref() {
                        Some(url) => local_status_id_from_url(state, url).await?,
                        None => None,
                    },
                    in_reply_to_remote_status_id,
                    object,
                },
                &attachments,
            )
            .await?;
            let direct_conversation_refresh =
                if let (Some(status), Some(recipients)) = (&status, direct_recipients.as_deref()) {
                    Some(
                        roosty_db::attach_remote_direct_status_to_conversation(
                            &txn,
                            status.id,
                            status.in_reply_to_local_status_id,
                            status.in_reply_to_remote_status_id,
                            recipients,
                            &direct_participants,
                            is_create,
                        )
                        .await?,
                    )
                } else {
                    None
                };
            let mut notifications = Vec::new();
            if let Some(status) = &status {
                for account_id in notification_recipients {
                    if let Some(notification) = roosty_db::notify_remote_status_mention(
                        &txn,
                        account_id,
                        remote_actor.id,
                        status.id,
                    )
                    .await?
                    {
                        notifications.push(notification);
                    }
                }
            }
            if let Some(status) = &status {
                let has_local_recipients =
                    !direct_recipients.as_deref().unwrap_or_default().is_empty();
                let has_local_followers =
                    !roosty_db::accepted_local_followers_of_remote_actor(&txn, remote_actor.id)
                        .await?
                        .is_empty();
                if has_local_recipients || has_local_followers {
                    crate::media::enqueue_remote_status_media_fetches_in_transaction(
                        &txn, status.id,
                    )
                    .await?;
                }
            }
            txn.commit().await?;
            Ok(match status {
                Some(status) => RemoteStatusChange::Upsert(
                    Box::new(status),
                    notifications,
                    direct_conversation_refresh,
                ),
                None => RemoteStatusChange::Ignored,
            })
        }
        Some("Delete") => {
            let activity: InboundDeleteActivity = serde_json::from_value(activity.clone())
                .map_err(|_| {
                    RoostyError::InvalidInput("remote Delete activity is invalid".to_owned())
                })?;
            if activity.actor != remote_actor.activitypub_id {
                return Err(RoostyError::InvalidInput(
                    "remote Delete actor does not match signer".to_owned(),
                ));
            }
            let object_id = match activity.object {
                InboundDeleteObject::Id(id) | InboundDeleteObject::Tombstone { id } => id,
            };
            if !object_id.starts_with("https://") {
                return Err(RoostyError::InvalidInput(
                    "remote Delete object is invalid".to_owned(),
                ));
            }
            let txn = state.db.begin().await?;
            let deleted = roosty_db::process_remote_status_delete(
                &txn,
                activity_id,
                remote_actor.id,
                &object_id,
            )
            .await?;
            let conversation_id = match deleted {
                Some(status_id) => {
                    roosty_db::repair_direct_conversation_after_delete(
                        &txn,
                        roosty_db::remote_status_conversation_id(&txn, status_id).await?,
                    )
                    .await?
                }
                None => None,
            };
            txn.commit().await?;
            Ok(match deleted {
                Some(status_id) => {
                    RemoteStatusChange::Delete(status_id.0.to_string(), conversation_id)
                }
                None => RemoteStatusChange::Ignored,
            })
        }
        _ => Err(RoostyError::InvalidInput(
            "unsupported remote status activity".to_owned(),
        )),
    }
}

/// Publish a cached remote Note lifecycle event only to local accounts following its author.
async fn publish_remote_status_change(
    state: &AppState,
    remote_actor_id: AccountId,
    change: RemoteStatusChange,
) -> Result<(), RoostyError> {
    let recipients =
        roosty_db::accepted_local_followers_of_remote_actor(&state.db, remote_actor_id).await?;
    match change {
        RemoteStatusChange::Ignored => {}
        RemoteStatusChange::Upsert(status, notifications, refresh) => {
            let response =
                crate::statuses::remote_status_response(state, (*status).clone()).await?;
            if let Some(refresh) = refresh {
                let mut account_ids = refresh.updated_account_ids;
                account_ids.extend(
                    roosty_db::local_conversation_accounts_for_last_remote_status(
                        &state.db,
                        refresh.conversation_id,
                        status.id,
                    )
                    .await?,
                );
                account_ids.sort_by_key(|id| id.0);
                account_ids.dedup();
                crate::conversations::publish_conversation_updates(
                    state,
                    refresh.conversation_id,
                    &account_ids,
                )
                .await?;
            }
            if !recipients.is_empty() {
                state
                    .streaming_events
                    .publish_home_update(&response, remote_actor_id, &recipients);
            }
            for notification in notifications {
                crate::notifications::publish_committed_notification(
                    state,
                    notification.account_id,
                    notification,
                )
                .await?;
            }
        }
        RemoteStatusChange::Delete(status_id, refresh) if !status_id.is_empty() => {
            if !recipients.is_empty() {
                state.streaming_events.publish_home_delete(
                    &status_id,
                    remote_actor_id,
                    &recipients,
                );
            }
            if let Some(refresh) = refresh {
                state.streaming_events.publish_delete(
                    &status_id,
                    remote_actor_id,
                    "direct",
                    &refresh.removed_account_ids,
                );
                crate::conversations::publish_conversation_updates(
                    state,
                    refresh.conversation_id,
                    &refresh.updated_account_ids,
                )
                .await?;
            }
        }
        RemoteStatusChange::Delete(_, _) => {}
    }
    Ok(())
}

/// Return a Mastodon visibility only for ActivityPub's public and unlisted audiences.
/// Retain only well-formed Mastodon Emoji tags; malformed decorations do not reject a Note.
fn remote_custom_emoji_definitions(tags: &[InboundTag]) -> Vec<NewRemoteCustomEmoji> {
    tags.iter()
        .filter(|tag| {
            matches!(
                tag.r#type,
                InboundTagType::Emoji | InboundTagType::MastodonEmoji
            )
        })
        .filter_map(|tag| {
            let shortcode = tag.name.as_deref()?.strip_prefix(':')?.strip_suffix(':')?;
            let remote_url = tag.icon.as_ref()?.url()?;
            (!shortcode.is_empty()
                && !shortcode.chars().any(char::is_whitespace)
                && remote_url.starts_with("https://"))
            .then(|| NewRemoteCustomEmoji {
                shortcode: shortcode.to_owned(),
                remote_url,
            })
        })
        .collect()
}

fn remote_status_visibility(note: &InboundNote) -> Option<&'static str> {
    if note.to.iter().any(|audience| audience == PUBLIC_AUDIENCE) {
        Some("public")
    } else if note.cc.iter().any(|audience| audience == PUBLIC_AUDIENCE) {
        Some("unlisted")
    } else {
        None
    }
}

/// Return addressed local actors only for Mastodon-compatible direct Notes.
async fn remote_direct_recipients(
    state: &AppState,
    note: &InboundNote,
) -> Result<Option<Vec<AccountId>>, RoostyError> {
    if note
        .to
        .iter()
        .chain(&note.cc)
        .any(|value| value == PUBLIC_AUDIENCE)
    {
        return Ok(None);
    }
    let mentions = note
        .tag
        .iter()
        .filter(|tag| tag.r#type == InboundTagType::Mention)
        .filter_map(|tag| tag.href.as_deref())
        .collect::<std::collections::HashSet<_>>();
    let audience = note.to.iter().chain(&note.cc).collect::<Vec<_>>();
    if audience.is_empty()
        || audience
            .iter()
            .any(|address| !mentions.contains(address.as_str()))
        || mentions
            .iter()
            .any(|mention| !audience.iter().any(|address| address.as_str() == *mention))
    {
        return Ok(None);
    }
    let prefix = format!(
        "{}/users/",
        state.config.public_base_url.as_str().trim_end_matches('/')
    );
    let mut recipients = Vec::new();
    for address in audience {
        if let Some(username) = address.strip_prefix(&prefix)
            && !username.contains('/')
            && let Some(account) =
                roosty_db::find_local_account_by_username(&state.db, username).await?
        {
            recipients.push(account.id);
        }
    }
    recipients.sort_by_key(|id| id.0);
    recipients.dedup();
    Ok((!recipients.is_empty()).then_some(recipients))
}

/// Retain every remote direct-message participant without fetching unknown actors.
async fn remote_direct_participants(
    state: &AppState,
    note: &InboundNote,
    author: &roosty_db::RemoteActor,
) -> Result<Vec<RemoteConversationParticipant>, RoostyError> {
    let mut participants = vec![RemoteConversationParticipant {
        activitypub_id: author.activitypub_id.clone(),
        remote_actor_id: Some(author.id),
        mention_name: Some(format!("@{}@{}", author.username, author.domain)),
    }];
    let local_prefix = format!(
        "{}/users/",
        state.config.public_base_url.as_str().trim_end_matches('/')
    );
    for tag in &note.tag {
        if tag.r#type != InboundTagType::Mention {
            continue;
        }
        let Some(activitypub_id) = tag.href.as_deref() else {
            continue;
        };
        if activitypub_id.starts_with(&local_prefix) {
            continue;
        }
        let remote_actor_id =
            roosty_db::find_remote_actor_by_activitypub_id(&state.db, activitypub_id)
                .await?
                .map(|actor| actor.id);
        participants.push(RemoteConversationParticipant {
            activitypub_id: activitypub_id.to_owned(),
            remote_actor_id,
            mention_name: tag.name.clone(),
        });
    }
    participants.sort_by(|left, right| left.activitypub_id.cmp(&right.activitypub_id));
    participants.dedup_by(|left, right| left.activitypub_id == right.activitypub_id);
    Ok(participants)
}

/// Resolve local recipients named by verified Mention tags without remote fetches.
async fn local_mention_recipients(
    state: &AppState,
    mention_urls: &[String],
) -> Result<Vec<AccountId>, RoostyError> {
    let prefix = format!(
        "{}/users/",
        state.config.public_base_url.as_str().trim_end_matches('/')
    );
    let mut recipients = Vec::new();
    for url in mention_urls {
        if let Some(username) = url.strip_prefix(&prefix)
            && !username.contains('/')
            && let Some(account) =
                roosty_db::find_local_account_by_username(&state.db, username).await?
        {
            recipients.push(account.id);
        }
    }
    Ok(recipients)
}

fn verify_legacy_signature(
    parts: &axum::http::request::Parts,
    body: &[u8],
    actor: &roosty_db::RemoteActor,
) -> Result<bool, RoostyError> {
    let digest = parts
        .headers
        .get("digest")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let expected_digest = format!("SHA-256={}", STANDARD.encode(Sha256::digest(body)));
    if digest != expected_digest {
        return Ok(false);
    }
    let date = parts
        .headers
        .get("date")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| RoostyError::InvalidInput("missing HTTP date".to_owned()))?;
    let date = httpdate::parse_http_date(date)
        .map_err(|_| RoostyError::InvalidInput("invalid HTTP date".to_owned()))?;
    let skew = std::time::SystemTime::now()
        .duration_since(date)
        .or_else(|_| date.duration_since(std::time::SystemTime::now()))
        .map_err(|_| RoostyError::InvalidInput("invalid HTTP date".to_owned()))?;
    if skew > std::time::Duration::from_secs(300) {
        return Ok(false);
    }
    let signature = parts
        .headers
        .get("signature")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| RoostyError::InvalidInput("missing HTTP signature".to_owned()))?;
    let attributes = signature_attributes(signature);
    if attributes.get("keyId") != Some(&actor.public_key_id) {
        return Ok(false);
    }
    let headers = attributes
        .get("headers")
        .map(String::as_str)
        .unwrap_or("(request-target)");
    for required in ["(request-target)", "host", "date", "digest"] {
        if !headers
            .split_whitespace()
            .any(|header| header.eq_ignore_ascii_case(required))
        {
            return Ok(false);
        }
    }
    let mut signed = Vec::new();
    for header_name in headers.split_whitespace() {
        let value = if header_name.eq_ignore_ascii_case("(request-target)") {
            format!(
                "post {}",
                parts
                    .uri
                    .path_and_query()
                    .map(|value| value.as_str())
                    .unwrap_or("/")
            )
        } else {
            parts
                .headers
                .get(header_name)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned()
        };
        if value.is_empty() {
            return Ok(false);
        }
        signed.push(format!("{}: {value}", header_name.to_ascii_lowercase()));
    }
    let signature_bytes = attributes
        .get("signature")
        .and_then(|value| STANDARD.decode(value).ok())
        .ok_or_else(|| RoostyError::InvalidInput("invalid HTTP signature encoding".to_owned()))?;
    let public_key = rsa::RsaPublicKey::from_public_key_pem(&actor.public_key_pem)
        .map_err(|_| RoostyError::InvalidInput("invalid remote actor public key".to_owned()))?;
    let signature = RsaSignature::try_from(signature_bytes.as_slice())
        .map_err(|_| RoostyError::InvalidInput("invalid HTTP signature".to_owned()))?;
    Ok(VerifyingKey::<Sha256>::new(public_key)
        .verify(signed.join("\n").as_bytes(), &signature)
        .is_ok())
}

fn signature_attributes(value: &str) -> std::collections::BTreeMap<String, String> {
    value
        .split(',')
        .filter_map(|part| {
            let (key, value) = part.trim().split_once('=')?;
            Some((key.to_owned(), value.trim_matches('"').to_owned()))
        })
        .collect()
}

#[derive(Deserialize, Serialize)]
struct FollowResponseDelivery {
    local_account_id: AccountId,
    remote_actor_id: AccountId,
    follow: JsonValue,
    response_type: FollowResponseType,
}

/// ActivityPub response types emitted for an inbound Follow request.
#[derive(Deserialize, Serialize)]
enum FollowResponseType {
    Accept,
    Reject,
}

/// Local state assigned to an inbound Follow before a manual approval decision.
#[derive(Clone, Copy)]
enum InboundFollowState {
    Pending,
    Accepted,
}

impl InboundFollowState {
    /// Return the persisted remote-follow state.
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Accepted => "accepted",
        }
    }
}

impl FollowResponseType {
    /// Return the ActivityStreams spelling used for identifiers and payloads.
    fn as_str(&self) -> &'static str {
        match self {
            Self::Accept => "Accept",
            Self::Reject => "Reject",
        }
    }
}

/// Durable payload for one activity delivery to one accepted remote follower.
#[derive(Deserialize, Serialize)]
struct StatusDelivery {
    local_account_id: AccountId,
    remote_actor_id: AccountId,
    activity: JsonValue,
    #[serde(default)]
    personal_inbox: bool,
}

/// Durable payload for one local actor Update delivery to an accepted remote follower.
#[derive(Deserialize, Serialize)]
struct ActorUpdateDelivery {
    local_account_id: AccountId,
    remote_actor_id: AccountId,
    activity: JsonValue,
}

/// Durable payload for a local Follow or Undo(Follow) delivery.
#[derive(Deserialize, Serialize)]
struct FollowDelivery {
    local_account_id: AccountId,
    remote_actor_id: AccountId,
    activity: JsonValue,
}

/// Durable payload for a local Like or Undo(Like) delivery.
#[derive(Deserialize, Serialize)]
struct FavouriteDelivery {
    local_account_id: AccountId,
    remote_actor_id: AccountId,
    activity: JsonValue,
}

/// Durable payload for a local Announce or Undo(Announce) delivery.
#[derive(Deserialize, Serialize)]
struct ReblogDelivery {
    local_account_id: AccountId,
    remote_actor_id: AccountId,
    activity: JsonValue,
}

/// Closed ActivityStreams activity types emitted for boost federation.
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "PascalCase")]
enum OutboundReblogType {
    Announce,
    Undo,
}

/// Typed embedded Announce reference used by Undo(Announce).
#[derive(Serialize)]
struct OutboundAnnounceReference {
    id: String,
    #[serde(rename = "type")]
    r#type: OutboundReblogType,
    actor: String,
    object: String,
}

/// Typed ActivityPub Announce or Undo(Announce) payload.
#[derive(Serialize)]
struct OutboundReblogActivity<T> {
    #[serde(rename = "@context")]
    context: &'static str,
    id: String,
    #[serde(rename = "type")]
    r#type: OutboundReblogType,
    actor: String,
    object: T,
}

/// Queue a signed Like activity for a cached remote Note.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) async fn enqueue_remote_favourite(
    state: &AppState,
    local_account_id: AccountId,
    remote_status: &roosty_db::RemoteStatus,
) -> Result<String, RoostyError> {
    let (id, job) =
        prepare_remote_favourite(state, &state.db, local_account_id, remote_status).await?;
    roosty_db::enqueue_job(
        &state.db,
        job.kind,
        job.payload,
        job.deduplication_key.as_deref(),
        job.run_after,
    )
    .await?;
    Ok(id)
}

/// Build the durable Like delivery without inserting it.
pub(crate) async fn prepare_remote_favourite(
    state: &AppState,
    db: &impl ConnectionTrait,
    local_account_id: AccountId,
    remote_status: &roosty_db::RemoteStatus,
) -> Result<(String, roosty_db::NewJob), RoostyError> {
    let local = roosty_db::find_local_account_by_id(db, local_account_id)
        .await?
        .ok_or_else(|| {
            RoostyError::InvalidInput("local favourite actor does not exist".to_owned())
        })?;
    let remote = roosty_db::find_remote_actor_by_id(db, remote_status.remote_actor_id)
        .await?
        .ok_or_else(|| {
            RoostyError::InvalidInput("remote status author does not exist".to_owned())
        })?;
    let actor = actor_url(state, &local.username);
    let id = format!("{actor}#like-{}", Uuid::now_v7());
    let activity = serde_json::json!({"@context": ACTIVITYSTREAMS_CONTEXT, "id": id, "type": "Like", "actor": actor, "object": remote_status.activitypub_id});
    Ok((
        id.clone(),
        favourite_delivery_job(local_account_id, remote.id, activity, &id)?,
    ))
}

/// Queue a signed Undo(Like) activity for a cached remote Note.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) async fn enqueue_remote_unfavourite(
    state: &AppState,
    favourite: roosty_db::LocalRemoteStatusFavourite,
) -> Result<(), RoostyError> {
    let job = prepare_remote_unfavourite(state, favourite).await?;
    roosty_db::enqueue_job(
        &state.db,
        job.kind,
        job.payload,
        job.deduplication_key.as_deref(),
        job.run_after,
    )
    .await?;
    Ok(())
}

/// Build the durable Undo(Like) delivery without inserting it.
pub(crate) async fn prepare_remote_unfavourite(
    state: &AppState,
    favourite: roosty_db::LocalRemoteStatusFavourite,
) -> Result<roosty_db::NewJob, RoostyError> {
    let local = roosty_db::find_local_account_by_id(&state.db, favourite.local_account_id)
        .await?
        .ok_or_else(|| {
            RoostyError::InvalidInput("local favourite actor does not exist".to_owned())
        })?;
    let remote_status = roosty_db::find_remote_status_by_id(&state.db, favourite.remote_status_id)
        .await?
        .ok_or_else(|| {
            RoostyError::InvalidInput("remote favourite status does not exist".to_owned())
        })?;
    let remote = roosty_db::find_remote_actor_by_id(&state.db, remote_status.remote_actor_id)
        .await?
        .ok_or_else(|| {
            RoostyError::InvalidInput("remote status author does not exist".to_owned())
        })?;
    let actor = actor_url(state, &local.username);
    let id = format!("{actor}#undo-like-{}", Uuid::now_v7());
    let like = serde_json::json!({"id": favourite.activity_id, "type": "Like", "actor": actor, "object": remote_status.activitypub_id});
    let activity = serde_json::json!({"@context": ACTIVITYSTREAMS_CONTEXT, "id": id, "type": "Undo", "actor": actor, "object": like});
    favourite_delivery_job(favourite.local_account_id, remote.id, activity, &id)
}

/// Serialize one Like-family delivery for transactional outbox insertion.
fn favourite_delivery_job(
    local_account_id: AccountId,
    remote_actor_id: AccountId,
    activity: JsonValue,
    activity_id: &str,
) -> Result<roosty_db::NewJob, RoostyError> {
    let payload = serde_json::to_value(FavouriteDelivery {
        local_account_id,
        remote_actor_id,
        activity,
    })
    .map_err(|error| RoostyError::InvalidInput(error.to_string()))?;
    Ok(roosty_db::NewJob {
        kind: roosty_db::JobKind::FederationFavouriteDelivery,
        payload,
        deduplication_key: Some(activity_id.to_owned()),
        run_after: OffsetDateTime::now_utc(),
    })
}

/// Dispatch a durable Like or Undo(Like) delivery job.
pub(crate) async fn deliver_favourite_activity(
    state: &AppState,
    payload: JsonValue,
) -> Result<(), RoostyError> {
    let payload: FavouriteDelivery = serde_json::from_value(payload)
        .map_err(|_| RoostyError::InvalidInput("invalid favourite delivery payload".to_owned()))?;
    deliver_activity(
        state,
        payload.local_account_id,
        payload.remote_actor_id,
        &payload.activity,
        false,
    )
    .await
}

/// Queue a signed Announce activity for a cached remote Note.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) async fn enqueue_remote_reblog(
    state: &AppState,
    local_account_id: AccountId,
    remote_status: &roosty_db::RemoteStatus,
) -> Result<String, RoostyError> {
    let (id, job) = prepare_remote_reblog(state, local_account_id, remote_status).await?;
    roosty_db::enqueue_job(
        &state.db,
        job.kind,
        job.payload,
        job.deduplication_key.as_deref(),
        job.run_after,
    )
    .await?;
    Ok(id)
}

/// Build an Announce delivery without inserting it.
pub(crate) async fn prepare_remote_reblog(
    state: &AppState,
    local_account_id: AccountId,
    remote_status: &roosty_db::RemoteStatus,
) -> Result<(String, roosty_db::NewJob), RoostyError> {
    let local = roosty_db::find_local_account_by_id(&state.db, local_account_id)
        .await?
        .ok_or_else(|| RoostyError::InvalidInput("local boost actor does not exist".to_owned()))?;
    let remote = roosty_db::find_remote_actor_by_id(&state.db, remote_status.remote_actor_id)
        .await?
        .ok_or_else(|| {
            RoostyError::InvalidInput("remote boost status author does not exist".to_owned())
        })?;
    let actor = actor_url(state, &local.username);
    let id = format!("{actor}#announce-{}", Uuid::now_v7());
    let activity = OutboundReblogActivity {
        context: ACTIVITYSTREAMS_CONTEXT,
        id: id.clone(),
        r#type: OutboundReblogType::Announce,
        actor,
        object: remote_status.activitypub_id.clone(),
    };
    Ok((
        id.clone(),
        reblog_delivery_job(local_account_id, remote.id, activity, &id)?,
    ))
}

/// Queue a signed Undo(Announce) activity for a cached remote Note.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) async fn enqueue_remote_unreblog(
    state: &AppState,
    reblog: roosty_db::LocalRemoteStatusReblog,
) -> Result<(), RoostyError> {
    let job = prepare_remote_unreblog(state, reblog).await?;
    roosty_db::enqueue_job(
        &state.db,
        job.kind,
        job.payload,
        job.deduplication_key.as_deref(),
        job.run_after,
    )
    .await?;
    Ok(())
}

/// Build an Undo(Announce) delivery without inserting it.
pub(crate) async fn prepare_remote_unreblog(
    state: &AppState,
    reblog: roosty_db::LocalRemoteStatusReblog,
) -> Result<roosty_db::NewJob, RoostyError> {
    let local = roosty_db::find_local_account_by_id(&state.db, reblog.local_account_id)
        .await?
        .ok_or_else(|| RoostyError::InvalidInput("local boost actor does not exist".to_owned()))?;
    let remote_status = roosty_db::find_remote_status_by_id(&state.db, reblog.remote_status_id)
        .await?
        .ok_or_else(|| {
            RoostyError::InvalidInput("remote boosted status does not exist".to_owned())
        })?;
    let remote = roosty_db::find_remote_actor_by_id(&state.db, remote_status.remote_actor_id)
        .await?
        .ok_or_else(|| {
            RoostyError::InvalidInput("remote boost status author does not exist".to_owned())
        })?;
    let actor = actor_url(state, &local.username);
    let id = format!("{actor}#undo-announce-{}", Uuid::now_v7());
    let activity = OutboundReblogActivity {
        context: ACTIVITYSTREAMS_CONTEXT,
        id: id.clone(),
        r#type: OutboundReblogType::Undo,
        actor: actor.clone(),
        object: OutboundAnnounceReference {
            id: reblog.activity_id,
            r#type: OutboundReblogType::Announce,
            actor,
            object: remote_status.activitypub_id,
        },
    };
    reblog_delivery_job(reblog.local_account_id, remote.id, activity, &id)
}

/// Serialize an Announce-family delivery for transactional outbox insertion.
fn reblog_delivery_job<T: Serialize>(
    local_account_id: AccountId,
    remote_actor_id: AccountId,
    activity: T,
    activity_id: &str,
) -> Result<roosty_db::NewJob, RoostyError> {
    let activity = serde_json::to_value(activity)
        .map_err(|error| RoostyError::InvalidInput(error.to_string()))?;
    let payload = serde_json::to_value(ReblogDelivery {
        local_account_id,
        remote_actor_id,
        activity,
    })
    .map_err(|error| RoostyError::InvalidInput(error.to_string()))?;
    Ok(roosty_db::NewJob {
        kind: roosty_db::JobKind::FederationReblogDelivery,
        payload,
        deduplication_key: Some(activity_id.to_owned()),
        run_after: OffsetDateTime::now_utc(),
    })
}

/// Dispatch a durable Announce or Undo(Announce) delivery job.
pub(crate) async fn deliver_reblog_activity(
    state: &AppState,
    payload: JsonValue,
) -> Result<(), RoostyError> {
    let payload: ReblogDelivery = serde_json::from_value(payload)
        .map_err(|_| RoostyError::InvalidInput("invalid reblog delivery payload".to_owned()))?;
    deliver_activity(
        state,
        payload.local_account_id,
        payload.remote_actor_id,
        &payload.activity,
        false,
    )
    .await
}

/// Queue a signed Follow activity for a remote actor and return its stable activity ID.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) async fn enqueue_remote_follow(
    state: &AppState,
    local_account_id: AccountId,
    remote_actor_id: AccountId,
) -> Result<String, RoostyError> {
    let (id, job) = prepare_remote_follow(state, local_account_id, remote_actor_id).await?;
    roosty_db::enqueue_job(
        &state.db,
        job.kind,
        job.payload,
        job.deduplication_key.as_deref(),
        job.run_after,
    )
    .await?;
    Ok(id)
}

/// Build the durable Follow delivery without inserting it.
pub(crate) async fn prepare_remote_follow(
    state: &AppState,
    local_account_id: AccountId,
    remote_actor_id: AccountId,
) -> Result<(String, roosty_db::NewJob), RoostyError> {
    let local = roosty_db::find_local_account_by_id(&state.db, local_account_id)
        .await?
        .ok_or_else(|| RoostyError::InvalidInput("local follow actor does not exist".to_owned()))?;
    let remote = roosty_db::find_remote_actor_by_id(&state.db, remote_actor_id)
        .await?
        .ok_or_else(|| {
            RoostyError::InvalidInput("remote follow actor does not exist".to_owned())
        })?;
    let actor = actor_url(state, &local.username);
    let id = format!("{actor}#follow-{}", Uuid::now_v7());
    let activity = serde_json::json!({"@context": ACTIVITYSTREAMS_CONTEXT, "id": id, "type": "Follow", "actor": actor, "object": remote.activitypub_id});
    Ok((
        id.clone(),
        follow_delivery_job(local_account_id, remote_actor_id, activity, &id)?,
    ))
}

/// Queue an Undo(Follow) activity for a relationship removed locally.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) async fn enqueue_remote_unfollow(
    state: &AppState,
    following: roosty_db::RemoteFollowing,
) -> Result<(), RoostyError> {
    let job = prepare_remote_unfollow(state, following).await?;
    roosty_db::enqueue_job(
        &state.db,
        job.kind,
        job.payload,
        job.deduplication_key.as_deref(),
        job.run_after,
    )
    .await?;
    Ok(())
}

/// Build the durable Undo(Follow) delivery without inserting it.
pub(crate) async fn prepare_remote_unfollow(
    state: &AppState,
    following: roosty_db::RemoteFollowing,
) -> Result<roosty_db::NewJob, RoostyError> {
    let local = roosty_db::find_local_account_by_id(&state.db, following.local_account_id)
        .await?
        .ok_or_else(|| RoostyError::InvalidInput("local follow actor does not exist".to_owned()))?;
    let remote = roosty_db::find_remote_actor_by_id(&state.db, following.remote_actor_id)
        .await?
        .ok_or_else(|| {
            RoostyError::InvalidInput("remote follow actor does not exist".to_owned())
        })?;
    let actor = actor_url(state, &local.username);
    let id = format!("{actor}#undo-follow-{}", Uuid::now_v7());
    let follow = serde_json::json!({"id": following.activity_id, "type": "Follow", "actor": actor, "object": remote.activitypub_id});
    let activity = serde_json::json!({"@context": ACTIVITYSTREAMS_CONTEXT, "id": id, "type": "Undo", "actor": actor, "object": follow});
    follow_delivery_job(
        following.local_account_id,
        following.remote_actor_id,
        activity,
        &id,
    )
}

/// Serialize one Follow-family delivery for transactional outbox insertion.
fn follow_delivery_job(
    local_account_id: AccountId,
    remote_actor_id: AccountId,
    activity: JsonValue,
    activity_id: &str,
) -> Result<roosty_db::NewJob, RoostyError> {
    let payload = serde_json::to_value(FollowDelivery {
        local_account_id,
        remote_actor_id,
        activity,
    })
    .map_err(|error| RoostyError::InvalidInput(error.to_string()))?;
    Ok(roosty_db::NewJob {
        kind: roosty_db::JobKind::FederationFollowDelivery,
        payload,
        deduplication_key: Some(activity_id.to_owned()),
        run_after: OffsetDateTime::now_utc(),
    })
}

/// Dispatch a durable local Follow or Undo(Follow) delivery job.
pub(crate) async fn deliver_follow_activity(
    state: &AppState,
    payload: JsonValue,
) -> Result<(), RoostyError> {
    let payload: FollowDelivery = serde_json::from_value(payload)
        .map_err(|_| RoostyError::InvalidInput("invalid follow delivery payload".to_owned()))?;
    deliver_activity(
        state,
        payload.local_account_id,
        payload.remote_actor_id,
        &payload.activity,
        false,
    )
    .await
}

/// Queue a public or unlisted local status activity for every accepted remote follower.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) async fn enqueue_status_activity(
    state: &AppState,
    status: &roosty_db::LocalStatus,
    kind: StatusActivityKind,
) -> Result<(), RoostyError> {
    if !state.config.federation_enabled
        || !matches!(
            status.visibility,
            StatusVisibility::Public | StatusVisibility::Unlisted | StatusVisibility::Direct
        )
    {
        return Ok(());
    }
    let local = roosty_db::find_local_account_by_id(&state.db, status.account_id)
        .await?
        .ok_or_else(|| RoostyError::InvalidInput("local status actor does not exist".to_owned()))?;
    let activity = status_activity(state, &local.username, status, kind).await?;
    let activity_id = activity
        .get("id")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| RoostyError::InvalidInput("status activity has no ID".to_owned()))?;
    let mut recipients = if status.visibility == StatusVisibility::Direct {
        Vec::new()
    } else {
        roosty_db::accepted_remote_followers(&state.db, local.id).await?
    };
    for actor in roosty_db::remote_mentions_for_local_status(&state.db, status.id).await? {
        if !recipients.iter().any(|recipient| recipient.id == actor.id) {
            recipients.push(actor);
        }
    }
    for remote in recipients {
        let payload = serde_json::to_value(StatusDelivery {
            local_account_id: local.id,
            remote_actor_id: remote.id,
            activity: activity.clone(),
            personal_inbox: status.visibility == StatusVisibility::Direct,
        })
        .map_err(|error| RoostyError::InvalidInput(error.to_string()))?;
        roosty_db::enqueue_job(
            &state.db,
            roosty_db::JobKind::FederationStatusDelivery,
            payload,
            Some(&format!("{activity_id}:{}", remote.id.0)),
            OffsetDateTime::now_utc(),
        )
        .await?;
    }
    Ok(())
}

/// Insert public status deliveries into a caller-owned transaction.
pub(crate) async fn enqueue_status_activity_in_transaction(
    state: &AppState,
    txn: &sea_orm::DatabaseTransaction,
    status: &roosty_db::LocalStatus,
    kind: StatusActivityKind,
) -> Result<(), RoostyError> {
    if !state.config.federation_enabled
        || !matches!(
            status.visibility,
            StatusVisibility::Public | StatusVisibility::Unlisted | StatusVisibility::Direct
        )
    {
        return Ok(());
    }
    let local = roosty_db::find_local_account_by_id(&state.db, status.account_id)
        .await?
        .ok_or_else(|| RoostyError::InvalidInput("local status actor does not exist".to_owned()))?;
    let activity = status_activity(state, &local.username, status, kind).await?;
    let activity_id = activity
        .get("id")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| RoostyError::InvalidInput("status activity has no ID".to_owned()))?;
    let mut recipients = if status.visibility == StatusVisibility::Direct {
        Vec::new()
    } else {
        roosty_db::accepted_remote_followers(&state.db, local.id).await?
    };
    for actor in roosty_db::remote_mentions_for_local_status(txn, status.id).await? {
        if !recipients.iter().any(|recipient| recipient.id == actor.id) {
            recipients.push(actor);
        }
    }
    for remote in recipients {
        let payload = serde_json::to_value(StatusDelivery {
            local_account_id: local.id,
            remote_actor_id: remote.id,
            activity: activity.clone(),
            personal_inbox: status.visibility == StatusVisibility::Direct,
        })
        .map_err(|error| RoostyError::InvalidInput(error.to_string()))?;
        roosty_db::enqueue_job_in_transaction(
            txn,
            roosty_db::NewJob {
                kind: roosty_db::JobKind::FederationStatusDelivery,
                payload,
                deduplication_key: Some(format!("{activity_id}:{}", remote.id.0)),
                run_after: OffsetDateTime::now_utc(),
            },
        )
        .await?;
    }
    Ok(())
}

/// Queue a refreshed local actor document for every accepted remote follower.
pub(crate) async fn enqueue_actor_update_in_transaction(
    state: &AppState,
    txn: &sea_orm::DatabaseTransaction,
    account: roosty_db::LocalAccount,
) -> Result<(), RoostyError> {
    if !state.config.federation_enabled {
        return Ok(());
    }

    let public_key_pem = ensure_actor_key(state, account.id).await?;
    let actor = actor_url(state, &account.username);
    let activity = serde_json::to_value(ActorUpdate {
        context: ACTIVITYSTREAMS_CONTEXT,
        id: format!("{actor}#update-{}", Uuid::now_v7()),
        r#type: UpdateType::Update,
        actor: actor.clone(),
        to: vec![format!("{actor}/followers")],
        object: actor_document(state, account.clone(), public_key_pem),
    })
    .map_err(|error| RoostyError::InvalidInput(error.to_string()))?;
    let activity_id = activity
        .get("id")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| RoostyError::InvalidInput("actor update has no ID".to_owned()))?;

    for remote in roosty_db::accepted_remote_followers(&state.db, account.id).await? {
        let payload = serde_json::to_value(ActorUpdateDelivery {
            local_account_id: account.id,
            remote_actor_id: remote.id,
            activity: activity.clone(),
        })
        .map_err(|error| RoostyError::InvalidInput(error.to_string()))?;
        roosty_db::enqueue_job_in_transaction(
            txn,
            roosty_db::NewJob {
                kind: roosty_db::JobKind::FederationActorUpdateDelivery,
                payload,
                deduplication_key: Some(format!("{activity_id}:{}", remote.id.0)),
                run_after: OffsetDateTime::now_utc(),
            },
        )
        .await?;
    }

    Ok(())
}

/// Resolve syntactically valid remote handles for a local status without making posting fail.
pub(crate) async fn resolve_remote_mentions(
    state: &AppState,
    content: &str,
) -> Vec<roosty_db::RemoteActor> {
    let mut actors = Vec::new();
    for handle in remote_mention_handles(content) {
        match discovery::resolve_remote_actor(state, &handle).await {
            Ok(actor)
                if !actors
                    .iter()
                    .any(|existing: &roosty_db::RemoteActor| existing.id == actor.id) =>
            {
                actors.push(actor)
            }
            Ok(_) => {}
            Err(error) => tracing::debug!(%error, %handle, "could not resolve remote mention"),
        }
    }
    actors
}

/// Return syntactic remote `@user@domain` handles in first-seen order.
fn remote_mention_handles(content: &str) -> Vec<String> {
    crate::statuses::remote_mention_handles(content)
}

/// Kinds of status lifecycle activities emitted to remote followers.
#[derive(Clone, Copy)]
pub(crate) enum StatusActivityKind {
    Create,
    Update,
    Delete,
}

/// Accept a pending remote Follow while atomically creating its durable Accept job.
pub(crate) async fn accept_remote_follow_request(
    state: &AppState,
    local_account_id: AccountId,
    remote_actor_id: AccountId,
) -> Result<bool, RoostyError> {
    let follow = roosty_db::pending_remote_follows(&state.db, local_account_id)
        .await?
        .into_iter()
        .find(|follow| follow.remote_actor_id == remote_actor_id);
    let Some(follow) = follow else {
        return Ok(false);
    };
    let payload = serde_json::to_value(FollowResponseDelivery {
        local_account_id,
        remote_actor_id,
        follow: follow.activity.clone(),
        response_type: FollowResponseType::Accept,
    })
    .map_err(|error| RoostyError::InvalidInput(error.to_string()))?;
    let txn = state.db.begin().await?;
    let accepted = roosty_db::accept_remote_follow_with_response_job(
        &txn,
        local_account_id,
        remote_actor_id,
        &follow.activity_id,
        roosty_db::RemoteFollowResponseJob {
            kind: roosty_db::JobKind::FederationFollowResponse,
            payload,
            deduplication_key: format!(
                "{}:{}",
                FollowResponseType::Accept.as_str(),
                follow.activity_id
            ),
        },
    )
    .await?;
    txn.commit().await?;
    Ok(accepted)
}

/// Reject a pending remote Follow while atomically creating its durable Reject job.
pub(crate) async fn reject_remote_follow_request(
    state: &AppState,
    local_account_id: AccountId,
    remote_actor_id: AccountId,
) -> Result<bool, RoostyError> {
    let follow = roosty_db::pending_remote_follows(&state.db, local_account_id)
        .await?
        .into_iter()
        .find(|follow| follow.remote_actor_id == remote_actor_id);
    let Some(follow) = follow else {
        return Ok(false);
    };
    let payload = serde_json::to_value(FollowResponseDelivery {
        local_account_id,
        remote_actor_id,
        follow: follow.activity.clone(),
        response_type: FollowResponseType::Reject,
    })
    .map_err(|error| RoostyError::InvalidInput(error.to_string()))?;
    let txn = state.db.begin().await?;
    let rejected = roosty_db::delete_remote_follow_with_response_job(
        &txn,
        local_account_id,
        remote_actor_id,
        &follow.activity_id,
        roosty_db::RemoteFollowResponseJob {
            kind: roosty_db::JobKind::FederationFollowResponse,
            payload,
            deduplication_key: format!(
                "{}:{}",
                FollowResponseType::Reject.as_str(),
                follow.activity_id
            ),
        },
    )
    .await?;
    txn.commit().await?;
    Ok(rejected)
}

/// Dispatch one durable follow-response delivery job.
pub(crate) async fn deliver_follow_response(
    state: &AppState,
    payload: JsonValue,
) -> Result<(), RoostyError> {
    let payload: FollowResponseDelivery = serde_json::from_value(payload)
        .map_err(|_| RoostyError::InvalidInput("invalid federation delivery payload".to_owned()))?;
    let local = roosty_db::find_local_account_by_id(&state.db, payload.local_account_id)
        .await?
        .ok_or_else(|| {
            RoostyError::InvalidInput("local delivery actor does not exist".to_owned())
        })?;
    let remote = roosty_db::find_remote_actor_by_id(&state.db, payload.remote_actor_id)
        .await?
        .ok_or_else(|| {
            RoostyError::InvalidInput("remote delivery actor does not exist".to_owned())
        })?;
    let key = roosty_db::find_local_actor_key(&state.db, local.id)
        .await?
        .ok_or_else(|| {
            RoostyError::InvalidInput("local delivery actor has no signing key".to_owned())
        })?;
    let private_key = decrypt_private_key(state, &key)?;
    let actor = actor_url(state, &local.username);
    let response_type = payload.response_type.as_str();
    let activity = serde_json::json!({"@context": ACTIVITYSTREAMS_CONTEXT, "id": format!("{actor}#{}-{}", response_type.to_ascii_lowercase(), Uuid::now_v7()), "type": response_type, "actor": actor, "object": payload.follow});
    signed_post(
        state,
        &remote.inbox_url,
        &private_key,
        &format!("{}#main-key", actor_url(state, &local.username)),
        &activity,
    )
    .await
}

/// Dispatch one durable local status activity delivery job.
pub(crate) async fn deliver_status_activity(
    state: &AppState,
    payload: JsonValue,
) -> Result<(), RoostyError> {
    let payload: StatusDelivery = serde_json::from_value(payload)
        .map_err(|_| RoostyError::InvalidInput("invalid status delivery payload".to_owned()))?;
    deliver_activity(
        state,
        payload.local_account_id,
        payload.remote_actor_id,
        &payload.activity,
        payload.personal_inbox,
    )
    .await
}

/// Dispatch one durable local actor Update delivery job.
pub(crate) async fn deliver_actor_update(
    state: &AppState,
    payload: JsonValue,
) -> Result<(), RoostyError> {
    let payload: ActorUpdateDelivery = serde_json::from_value(payload).map_err(|_| {
        RoostyError::InvalidInput("invalid actor update delivery payload".to_owned())
    })?;
    deliver_activity(
        state,
        payload.local_account_id,
        payload.remote_actor_id,
        &payload.activity,
        false,
    )
    .await
}

/// Sign and deliver one already-persisted activity to a remote actor's inbox.
async fn deliver_activity(
    state: &AppState,
    local_account_id: AccountId,
    remote_actor_id: AccountId,
    activity: &JsonValue,
    personal_inbox: bool,
) -> Result<(), RoostyError> {
    let local = roosty_db::find_local_account_by_id(&state.db, local_account_id)
        .await?
        .ok_or_else(|| {
            RoostyError::InvalidInput("local delivery actor does not exist".to_owned())
        })?;
    let remote = roosty_db::find_remote_actor_by_id(&state.db, remote_actor_id)
        .await?
        .ok_or_else(|| {
            RoostyError::InvalidInput("remote delivery actor does not exist".to_owned())
        })?;
    let key = roosty_db::find_local_actor_key(&state.db, local.id)
        .await?
        .ok_or_else(|| {
            RoostyError::InvalidInput("local delivery actor has no signing key".to_owned())
        })?;
    let private_key = decrypt_private_key(state, &key)?;
    signed_post(
        state,
        if personal_inbox {
            &remote.inbox_url
        } else {
            remote
                .shared_inbox_url
                .as_deref()
                .unwrap_or(&remote.inbox_url)
        },
        &private_key,
        &format!("{}#main-key", actor_url(state, &local.username)),
        activity,
    )
    .await
}

fn decrypt_private_key(
    state: &AppState,
    key: &roosty_db::LocalActorKey,
) -> Result<RsaPrivateKey, RoostyError> {
    let secret = state
        .config
        .federation_key_encryption_secret
        .as_deref()
        .ok_or_else(|| {
            RoostyError::Configuration("federation key encryption secret is unavailable".to_owned())
        })?;
    if key.private_key_nonce.len() != 12 {
        return Err(RoostyError::InvalidInput(
            "stored actor key nonce is invalid".to_owned(),
        ));
    }
    let mut nonce = [0_u8; 12];
    nonce.copy_from_slice(&key.private_key_nonce);
    let key_bytes = digest::digest(&digest::SHA256, secret.as_bytes());
    let cipher = aead::LessSafeKey::new(
        aead::UnboundKey::new(&aead::AES_256_GCM, key_bytes.as_ref()).map_err(|_| {
            RoostyError::InvalidInput("invalid federation encryption key".to_owned())
        })?,
    );
    let mut bytes = key.private_key_ciphertext.clone();
    let plain = cipher
        .open_in_place(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::empty(),
            &mut bytes,
        )
        .map_err(|_| RoostyError::InvalidInput("could not decrypt actor key".to_owned()))?;
    let pem = std::str::from_utf8(plain)
        .map_err(|_| RoostyError::InvalidInput("stored actor key is invalid".to_owned()))?;
    RsaPrivateKey::from_pkcs8_pem(pem)
        .map_err(|_| RoostyError::InvalidInput("stored actor key is invalid".to_owned()))
}

async fn signed_post(
    state: &AppState,
    inbox: &str,
    private_key: &RsaPrivateKey,
    key_id: &str,
    activity: &JsonValue,
) -> Result<(), RoostyError> {
    let url = url::Url::parse(inbox)
        .map_err(|_| RoostyError::InvalidInput("remote inbox URL is invalid".to_owned()))?;
    let host = url
        .host_str()
        .ok_or_else(|| RoostyError::InvalidInput("remote inbox has no host".to_owned()))?
        .to_owned();
    let body = serde_json::to_vec(activity)
        .map_err(|error| RoostyError::InvalidInput(error.to_string()))?;
    let digest = format!("SHA-256={}", STANDARD.encode(Sha256::digest(&body)));
    let date = httpdate::fmt_http_date(std::time::SystemTime::now());
    let path = match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_owned(),
    };
    let signing =
        format!("(request-target): post {path}\nhost: {host}\ndate: {date}\ndigest: {digest}");
    let signature = SigningKey::<Sha256>::new(private_key.clone()).sign(signing.as_bytes());
    let signature = format!(
        "keyId=\"{key_id}\",algorithm=\"rsa-sha256\",headers=\"(request-target) host date digest\",signature=\"{}\"",
        STANDARD.encode(signature.to_vec())
    );
    #[cfg(test)]
    if let Some(result) =
        test_transport::deliver_if_registered(&url, &host, &date, &digest, &signature, body.clone())
            .await
    {
        return result;
    }
    let address = discovery::validate_remote_url(state, &url).await?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(15))
        .resolve(&host, address)
        .build()
        .map_err(|error| RoostyError::InvalidInput(error.to_string()))?;
    let response = client
        .post(url)
        .header("content-type", ACTIVITYSTREAMS_CONTENT_TYPE)
        .header("host", host)
        .header("date", date)
        .header("digest", digest)
        .header("signature", signature)
        .body(body)
        .send()
        .await
        .map_err(|error| RoostyError::InvalidInput(error.to_string()))?;
    if response.status().is_success() {
        Ok(())
    } else if matches!(
        response.status().as_u16(),
        400 | 401 | 403 | 404 | 405 | 410
    ) {
        Err(RoostyError::InvalidInput(format!(
            "permanent federation delivery failure: remote inbox returned {}",
            response.status()
        )))
    } else {
        Err(RoostyError::InvalidInput(format!(
            "remote inbox returned {}",
            response.status()
        )))
    }
}

async fn create(
    state: &AppState,
    username: &str,
    status: roosty_db::LocalStatus,
) -> Result<Create, RoostyError> {
    let object = note_object(state, username, status).await?;
    Ok(Create {
        context: ACTIVITYSTREAMS_CONTEXT,
        r#type: CreateType::Create,
        id: format!("{}#create", object.id),
        actor: object.attributed_to.clone(),
        published: object.published.clone(),
        to: object.to.clone(),
        cc: object.cc.clone(),
        object,
    })
}

async fn status_activity(
    state: &AppState,
    username: &str,
    status: &roosty_db::LocalStatus,
    kind: StatusActivityKind,
) -> Result<JsonValue, RoostyError> {
    let note = note_object(state, username, status.clone()).await?;
    let actor = note.attributed_to.clone();
    let activity = match kind {
        StatusActivityKind::Create => serde_json::to_value(Create {
            context: ACTIVITYSTREAMS_CONTEXT,
            r#type: CreateType::Create,
            id: format!("{}#create-{}", note.id, Uuid::now_v7()),
            actor,
            published: note.published.clone(),
            to: note.to.clone(),
            cc: note.cc.clone(),
            object: note,
        }),
        StatusActivityKind::Update => serde_json::to_value(Update {
            context: ACTIVITYSTREAMS_CONTEXT,
            id: format!("{}#update-{}", note.id, Uuid::now_v7()),
            r#type: UpdateType::Update,
            actor,
            to: note.to.clone(),
            cc: note.cc.clone(),
            object: note,
        }),
        StatusActivityKind::Delete => serde_json::to_value(Delete {
            context: ACTIVITYSTREAMS_CONTEXT,
            id: format!("{}#delete-{}", note.id, Uuid::now_v7()),
            r#type: DeleteType::Delete,
            actor,
            to: note.to,
            cc: note.cc,
            object: note.id,
        }),
    };
    activity.map_err(|error| RoostyError::InvalidInput(error.to_string()))
}

async fn note_object(
    state: &AppState,
    username: &str,
    status: roosty_db::LocalStatus,
) -> Result<Note, RoostyError> {
    let id = status_url(state, username, status.id);
    let (to, cc) = status_audience(state, username, (&status.visibility).into());
    let in_reply_to = match status.in_reply_to_remote_status_id {
        Some(id) => roosty_db::find_remote_status_by_id(&state.db, id)
            .await?
            .map(|status| status.activitypub_id),
        None => match status.in_reply_to_id {
            Some(parent_id) => match roosty_db::find_local_status_by_id(&state.db, parent_id)
                .await?
            {
                Some(parent) => {
                    match roosty_db::find_local_account_by_id(&state.db, parent.account_id).await? {
                        Some(parent_account) => {
                            Some(status_url(state, &parent_account.username, parent.id))
                        }
                        None => None,
                    }
                }
                None => None,
            },
            None => None,
        },
    };
    let mut tag = Vec::new();
    for username in crate::statuses::mention_usernames(&status.content) {
        if let Some(account) =
            roosty_db::find_local_account_by_username(&state.db, &username).await?
        {
            tag.push(MentionTag {
                r#type: MentionType::Mention,
                href: actor_url(state, &account.username),
                name: format!("@{}", account.username),
            });
        }
    }
    for actor in roosty_db::remote_mentions_for_local_status(&state.db, status.id).await? {
        tag.push(MentionTag {
            r#type: MentionType::Mention,
            href: actor.activitypub_id,
            name: format!("@{}@{}", actor.username, actor.domain),
        });
    }
    let (to, cc) = if status.visibility == StatusVisibility::Direct {
        (
            tag.iter().map(|mention| mention.href.clone()).collect(),
            Vec::new(),
        )
    } else {
        (to, cc)
    };
    Ok(Note {
        context: ACTIVITYSTREAMS_CONTEXT,
        id,
        r#type: NoteType::Note,
        attributed_to: actor_url(state, username),
        content: status.content,
        published: crate::statuses::format_timestamp(status.created_at),
        updated: crate::statuses::format_timestamp(status.updated_at),
        in_reply_to,
        tag,
        to,
        cc,
    })
}

fn status_audience(
    state: &AppState,
    username: &str,
    visibility: &str,
) -> (Vec<String>, Vec<String>) {
    let followers = format!("{}/followers", actor_url(state, username));
    match visibility {
        "unlisted" => (vec![followers], vec![PUBLIC_AUDIENCE.to_owned()]),
        _ => (vec![PUBLIC_AUDIENCE.to_owned()], vec![followers]),
    }
}
fn activity_response<T: Serialize>(value: T) -> Response {
    (
        [(header::CONTENT_TYPE, ACTIVITYSTREAMS_CONTENT_TYPE)],
        Json(value),
    )
        .into_response()
}
fn actor_url(state: &AppState, username: &str) -> String {
    public_url(state, &format!("users/{username}"))
}
fn status_url(state: &AppState, username: &str, status_id: StatusId) -> String {
    public_url(state, &format!("users/{username}/statuses/{}", status_id.0))
}
fn public_url(state: &AppState, path: &str) -> String {
    state
        .config
        .public_base_url
        .join(path)
        .map(|url| url.to_string())
        .unwrap_or_else(|_| format!("{}{path}", state.config.public_base_url))
}
fn parse_acct(resource: &str) -> Option<(&str, &str)> {
    let value = resource.strip_prefix("acct:")?;
    let (username, domain) = value.rsplit_once('@')?;
    (!username.is_empty() && !domain.is_empty() && !username.contains('/') && !domain.contains('/'))
        .then_some((username, domain))
}
fn internal_error(error: impl std::fmt::Display) -> Response {
    tracing::error!(%error, "federation request failed");
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
}

/// Return the public key, generating and encrypting a fresh key only once.
async fn ensure_actor_key(state: &AppState, account_id: AccountId) -> Result<String, RoostyError> {
    if let Some(key) = roosty_db::find_local_actor_key(&state.db, account_id).await? {
        return Ok(key.public_key_pem);
    }
    let private_key = RsaPrivateKey::new(&mut OsRng, 2048).map_err(|error| {
        RoostyError::Configuration(format!("could not generate actor key: {error}"))
    })?;
    let public_key_pem = private_key
        .to_public_key()
        .to_public_key_pem(LineEnding::LF)
        .map_err(|error| {
            RoostyError::Configuration(format!("could not encode actor public key: {error}"))
        })?;
    let private_key_pem = private_key.to_pkcs8_pem(LineEnding::LF).map_err(|error| {
        RoostyError::Configuration(format!("could not encode actor private key: {error}"))
    })?;
    let mut nonce = [0_u8; 12];
    OsRng.fill_bytes(&mut nonce);
    let mut ciphertext = private_key_pem.as_bytes().to_vec();
    let secret = state
        .config
        .federation_key_encryption_secret
        .as_deref()
        .ok_or_else(|| {
            RoostyError::Configuration("federation key encryption secret is unavailable".to_owned())
        })?;
    let key_bytes = digest::digest(&digest::SHA256, secret.as_bytes());
    let key = aead::LessSafeKey::new(
        aead::UnboundKey::new(&aead::AES_256_GCM, key_bytes.as_ref()).map_err(|_| {
            RoostyError::Configuration("invalid federation key encryption key".to_owned())
        })?,
    );
    key.seal_in_place_append_tag(
        aead::Nonce::assume_unique_for_key(nonce),
        aead::Aad::empty(),
        &mut ciphertext,
    )
    .map_err(|_| RoostyError::Configuration("could not encrypt actor key".to_owned()))?;
    let stored = roosty_db::LocalActorKey {
        public_key_pem: public_key_pem.clone(),
        private_key_ciphertext: ciphertext,
        private_key_nonce: nonce.to_vec(),
    };
    match roosty_db::create_local_actor_key(&state.db, account_id, &stored).await {
        Ok(()) => Ok(public_key_pem),
        Err(_) => roosty_db::find_local_actor_key(&state.db, account_id)
            .await?
            .map(|key| key.public_key_pem)
            .ok_or_else(|| {
                RoostyError::Configuration("actor key could not be persisted".to_owned())
            }),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        net::{IpAddr, Ipv4Addr, SocketAddr},
        sync::LazyLock,
    };

    use postgresql_embedded::PostgreSQL;
    use roosty_core::AccountId;
    use roosty_db::StatusVisibility;
    use roosty_migration::Migrator;
    use sea_orm::TransactionTrait;
    use sea_orm_migration::MigratorTrait;
    use serde_json::json;
    use tempfile::TempDir;

    use super::{
        Actor, ActorImage, ActorImageType, ActorType, CollectionType, Create, CreateType,
        InboundFollowActivity, InboundNote, InboundUndoAnnounceActivity, InboundUndoFollowActivity,
        MentionTag, MentionType, Note, NoteType, OrderedCollection, PublicKey, actor_context,
        actor_profile_fields, is_remote_actor_lifecycle_activity, local_actor_type, parse_acct,
        remote_status_visibility,
    };
    use crate::{config::Config, federation::test_transport, http::AppState};

    /// Serializes scenarios which share the in-process recipient registry.
    static FEDERATION_TEST_LOCK: LazyLock<tokio::sync::Mutex<()>> =
        LazyLock::new(|| tokio::sync::Mutex::new(()));

    /// Only an `acct:` resource with one non-empty local handle and domain is valid.
    #[test]
    fn parses_only_local_webfinger_resources() {
        assert_eq!(
            parse_acct("acct:alice@example.test"),
            Some(("alice", "example.test"))
        );
        assert_eq!(parse_acct("alice@example.test"), None);
        assert_eq!(parse_acct("acct:@example.test"), None);
        assert_eq!(parse_acct("acct:alice@"), None);
        assert_eq!(parse_acct("acct:alice@example.test/path"), None);
    }

    /// Both ActivityPub link forms identify the same Follow target.
    #[test]
    fn parses_follow_actor_reference_forms() {
        let string: InboundFollowActivity =
            serde_json::from_str(r#"{"object":"https://roosty.test/users/alice"}"#).unwrap();
        let object: InboundFollowActivity =
            serde_json::from_str(r#"{"object":{"id":"https://roosty.test/users/alice"}}"#).unwrap();

        assert_eq!(string.object.id(), "https://roosty.test/users/alice");
        assert_eq!(object.object.id(), "https://roosty.test/users/alice");
        assert!(serde_json::from_str::<InboundFollowActivity>(r#"{"object":{}}"#).is_err());
    }

    /// Actor lifecycle routing must not capture Note updates and deletes.
    #[test]
    fn distinguishes_remote_actor_lifecycle_activities() {
        let actor = "https://remote.test/users/alice";
        assert!(is_remote_actor_lifecycle_activity(
            &serde_json::json!({"type":"Update", "object":{"id":actor, "type":"Person"}}),
            actor,
        ));
        assert!(is_remote_actor_lifecycle_activity(
            &serde_json::json!({"type":"Delete", "object":{"id":actor, "type":"Tombstone"}}),
            actor,
        ));
        assert!(is_remote_actor_lifecycle_activity(
            &serde_json::json!({"type":"Move", "object":actor}),
            actor,
        ));
        assert!(!is_remote_actor_lifecycle_activity(
            &serde_json::json!({"type":"Update", "object":{"id":"https://remote.test/statuses/1", "type":"Note"}}),
            actor,
        ));
    }

    /// Undo may reference the original Follow directly or embed its typed activity object.
    #[test]
    fn parses_follow_undo_reference_forms() {
        let string: InboundUndoFollowActivity =
            serde_json::from_str(r#"{"object":"https://remote.test/follows/1"}"#).unwrap();
        let embedded: InboundUndoFollowActivity = serde_json::from_str(
            r#"{"object":{"id":"https://remote.test/follows/1","type":"Follow"}}"#,
        )
        .unwrap();
        let invalid: InboundUndoFollowActivity = serde_json::from_str(
            r#"{"object":{"id":"https://remote.test/follows/1","type":"Like"}}"#,
        )
        .unwrap();

        assert_eq!(
            string.object.follow_id().as_deref(),
            Some("https://remote.test/follows/1")
        );
        assert_eq!(
            embedded.object.follow_id().as_deref(),
            Some("https://remote.test/follows/1")
        );
        assert_eq!(invalid.object.follow_id(), None);
    }

    /// Undo accepts a link or a correctly typed embedded Announce, never another activity type.
    #[test]
    fn parses_announce_undo_reference_forms() {
        let string: InboundUndoAnnounceActivity =
            serde_json::from_str(r#"{"object":"https://remote.test/announces/1"}"#).unwrap();
        let embedded: InboundUndoAnnounceActivity = serde_json::from_str(
            r#"{"object":{"id":"https://remote.test/announces/1","type":"Announce"}}"#,
        )
        .unwrap();

        assert_eq!(
            string.object.announce_id().as_deref(),
            Some("https://remote.test/announces/1")
        );
        assert_eq!(
            embedded.object.announce_id().as_deref(),
            Some("https://remote.test/announces/1")
        );
        assert!(
            serde_json::from_str::<InboundUndoAnnounceActivity>(
                r#"{"object":{"id":"https://remote.test/announces/1","type":"Like"}}"#,
            )
            .is_err()
        );
    }

    /// Given isolated instances, when a remote account follows then unfollows, signed status
    /// delivery reaches it only while the accepted relationship exists.
    #[tokio::test]
    async fn remote_follow_handshake_delivers_then_stops_statuses() {
        let _guard = FEDERATION_TEST_LOCK.lock().await;
        let context = FederationTestContext::setup().await;
        test_transport::register_inbox("alpha.test", context.alpha.clone());
        test_transport::register_inbox("beta.test", context.beta.clone());

        let author = create_test_account(&context.alpha, "author").await;
        let follower = create_test_account(&context.beta, "follower").await;
        let alpha_key = super::ensure_actor_key(&context.alpha, author.id)
            .await
            .unwrap();
        let beta_key = super::ensure_actor_key(&context.beta, follower.id)
            .await
            .unwrap();
        let alpha_remote = cache_test_actor(&context.beta, "author", "alpha.test", alpha_key).await;
        let beta_remote = cache_test_actor(&context.alpha, "follower", "beta.test", beta_key).await;

        let follow_id = super::enqueue_remote_follow(&context.beta, follower.id, alpha_remote.id)
            .await
            .unwrap();
        roosty_db::create_remote_following(
            &context.beta.db,
            follower.id,
            alpha_remote.id,
            &follow_id,
        )
        .await
        .unwrap();
        deliver_test_job(&context.beta, roosty_db::JobKind::FederationFollowDelivery).await;
        assert!(
            roosty_db::remote_actor_follows_local_account(
                &context.alpha.db,
                beta_remote.id,
                author.id,
            )
            .await
            .unwrap()
        );

        deliver_test_job(&context.alpha, roosty_db::JobKind::FederationFollowResponse).await;
        assert_eq!(
            roosty_db::find_remote_following(&context.beta.db, follower.id, alpha_remote.id)
                .await
                .unwrap()
                .unwrap()
                .state,
            "accepted"
        );

        let first = create_public_test_status(&context.alpha, author.id, "first delivery").await;
        super::enqueue_status_activity(&context.alpha, &first, super::StatusActivityKind::Create)
            .await
            .unwrap();
        deliver_test_job(&context.alpha, roosty_db::JobKind::FederationStatusDelivery).await;
        assert!(
            roosty_db::find_remote_status_by_activitypub_id(
                &context.beta.db,
                &super::status_url(&context.alpha, "author", first.id),
            )
            .await
            .unwrap()
            .is_some()
        );

        let following =
            roosty_db::delete_remote_following(&context.beta.db, follower.id, alpha_remote.id)
                .await
                .unwrap()
                .unwrap();
        super::enqueue_remote_unfollow(&context.beta, following)
            .await
            .unwrap();
        deliver_test_job(&context.beta, roosty_db::JobKind::FederationFollowDelivery).await;
        assert!(
            !roosty_db::remote_actor_follows_local_account(
                &context.alpha.db,
                beta_remote.id,
                author.id,
            )
            .await
            .unwrap()
        );

        let second = create_public_test_status(&context.alpha, author.id, "not delivered").await;
        super::enqueue_status_activity(&context.alpha, &second, super::StatusActivityKind::Create)
            .await
            .unwrap();
        assert!(
            roosty_db::claim_due_job(
                &context.alpha.db,
                "federation-test",
                time::Duration::minutes(1),
            )
            .await
            .unwrap()
            .is_none()
        );

        test_transport::clear_inboxes();
        context.teardown().await;
    }

    /// Given locked local accounts, when remote requests are approved or rejected, then exactly
    /// the corresponding signed response is delivered and the remote relationship agrees.
    #[tokio::test]
    async fn locked_remote_follow_requests_deliver_accept_or_reject() {
        let _guard = FEDERATION_TEST_LOCK.lock().await;
        let context = FederationTestContext::setup().await;
        test_transport::register_inbox("alpha.test", context.alpha.clone());
        test_transport::register_inbox("beta.test", context.beta.clone());

        let approved = create_test_account(&context.alpha, "approved").await;
        let rejected = create_test_account(&context.alpha, "rejected").await;
        lock_test_account(&context.alpha, approved.id).await;
        lock_test_account(&context.alpha, rejected.id).await;
        let follower = create_test_account(&context.beta, "follower").await;
        let alpha_key = super::ensure_actor_key(&context.alpha, approved.id)
            .await
            .unwrap();
        let rejected_key = super::ensure_actor_key(&context.alpha, rejected.id)
            .await
            .unwrap();
        let beta_key = super::ensure_actor_key(&context.beta, follower.id)
            .await
            .unwrap();
        let approved_remote =
            cache_test_actor(&context.beta, "approved", "alpha.test", alpha_key.clone()).await;
        let rejected_remote =
            cache_test_actor(&context.beta, "rejected", "alpha.test", rejected_key).await;
        let beta_remote = cache_test_actor(&context.alpha, "follower", "beta.test", beta_key).await;

        follow_test_actor(&context.beta, follower.id, approved_remote.id).await;
        deliver_test_job(&context.beta, roosty_db::JobKind::FederationFollowDelivery).await;
        assert!(
            !roosty_db::remote_actor_follows_local_account(
                &context.alpha.db,
                beta_remote.id,
                approved.id,
            )
            .await
            .unwrap()
        );
        assert!(
            super::accept_remote_follow_request(&context.alpha, approved.id, beta_remote.id)
                .await
                .unwrap()
        );
        deliver_test_job(&context.alpha, roosty_db::JobKind::FederationFollowResponse).await;
        assert_eq!(
            roosty_db::find_remote_following(&context.beta.db, follower.id, approved_remote.id)
                .await
                .unwrap()
                .unwrap()
                .state,
            "accepted"
        );

        follow_test_actor(&context.beta, follower.id, rejected_remote.id).await;
        deliver_test_job(&context.beta, roosty_db::JobKind::FederationFollowDelivery).await;
        assert!(
            super::reject_remote_follow_request(&context.alpha, rejected.id, beta_remote.id)
                .await
                .unwrap()
        );
        deliver_test_job(&context.alpha, roosty_db::JobKind::FederationFollowResponse).await;
        assert!(
            roosty_db::find_remote_following(&context.beta.db, follower.id, rejected_remote.id,)
                .await
                .unwrap()
                .is_none()
        );

        test_transport::clear_inboxes();
        context.teardown().await;
    }

    /// Given an unreachable inbox, when a delivery fails, then the durable job is released and
    /// rescheduled with exponential backoff instead of being delivered twice immediately.
    #[tokio::test]
    async fn failed_delivery_is_rescheduled_for_retry() {
        let _guard = FEDERATION_TEST_LOCK.lock().await;
        let context = FederationTestContext::setup().await;
        let follower = create_test_account(&context.beta, "follower").await;
        let actor_key = super::ensure_actor_key(&context.beta, follower.id)
            .await
            .unwrap();
        let unreachable =
            cache_test_actor(&context.beta, "unreachable", "unreachable.test", actor_key).await;

        follow_test_actor(&context.beta, follower.id, unreachable.id).await;
        let job = roosty_db::claim_due_job(
            &context.beta.db,
            "federation-test",
            time::Duration::minutes(1),
        )
        .await
        .unwrap()
        .unwrap();
        let error = super::deliver_follow_activity(&context.beta, job.payload.clone())
            .await
            .unwrap_err();
        let retried_at = roosty_db::mark_job_failed(&context.beta.db, &job, &error.to_string())
            .await
            .unwrap()
            .unwrap();

        assert!(retried_at > time::OffsetDateTime::now_utc());
        assert!(
            roosty_db::claim_due_job(
                &context.beta.db,
                "federation-test",
                time::Duration::minutes(1),
            )
            .await
            .unwrap()
            .is_none()
        );

        context.teardown().await;
    }

    /// Given an accepted remote follower, when a local profile changes, then a durable Actor
    /// Update carrying its refreshed avatar and header is queued for that follower.
    #[tokio::test]
    async fn profile_updates_enqueue_actor_update_delivery() {
        let _guard = FEDERATION_TEST_LOCK.lock().await;
        let context = FederationTestContext::setup().await;
        let author = create_test_account(&context.alpha, "author").await;
        let follower = create_test_account(&context.beta, "follower").await;
        let follower_key = super::ensure_actor_key(&context.beta, follower.id)
            .await
            .unwrap();
        let remote_follower =
            cache_test_actor(&context.alpha, "follower", "beta.test", follower_key).await;
        roosty_db::upsert_remote_follow(
            &context.alpha.db,
            remote_follower.id,
            author.id,
            "https://beta.test/follows/profile-update",
            serde_json::json!({}),
            "accepted",
        )
        .await
        .unwrap();
        let updated = roosty_db::update_local_account_settings(
            &context.alpha.db,
            author.id,
            roosty_db::LocalAccountSettingsUpdate {
                avatar_file_path: Some("accounts/avatar.png".to_owned()),
                header_file_path: Some("accounts/header.png".to_owned()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let txn = context.alpha.db.begin().await.unwrap();
        super::enqueue_actor_update_in_transaction(&context.alpha, &txn, updated)
            .await
            .unwrap();
        txn.commit().await.unwrap();

        let job = roosty_db::claim_due_job(
            &context.alpha.db,
            "federation-test",
            time::Duration::minutes(1),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            job.kind,
            roosty_db::JobKind::FederationActorUpdateDelivery.as_str()
        );
        let activity = &job.payload["activity"];
        assert_eq!(activity["type"], "Update");
        assert_eq!(activity["actor"], activity["object"]["id"]);
        assert_eq!(
            activity["to"],
            serde_json::json!(["https://alpha.test/users/author/followers"])
        );
        assert_eq!(activity["object"]["type"], "Person");
        assert_eq!(
            activity["object"]["icon"]["url"],
            "https://alpha.test/media_attachments/files/accounts/avatar.png"
        );
        assert_eq!(activity["object"]["icon"]["mediaType"], "image/png");
        assert_eq!(
            activity["object"]["image"]["url"],
            "https://alpha.test/media_attachments/files/accounts/header.png"
        );
        assert_eq!(activity["object"]["image"]["mediaType"], "image/png");
        assert!(
            roosty_db::mark_job_completed(&context.alpha.db, &job)
                .await
                .unwrap()
        );

        context.teardown().await;
    }

    /// Given a cached public remote Note, when its local follower favourites then unfavourites it,
    /// then signed Like and Undo activities update the origin's favourite count.
    #[tokio::test]
    async fn remote_favourite_and_undo_are_delivered_to_the_status_origin() {
        let _guard = FEDERATION_TEST_LOCK.lock().await;
        let context = FederationTestContext::setup().await;
        test_transport::register_inbox("alpha.test", context.alpha.clone());
        test_transport::register_inbox("beta.test", context.beta.clone());

        let author = create_test_account(&context.alpha, "author").await;
        let liker = create_test_account(&context.beta, "liker").await;
        let alpha_key = super::ensure_actor_key(&context.alpha, author.id)
            .await
            .unwrap();
        let beta_key = super::ensure_actor_key(&context.beta, liker.id)
            .await
            .unwrap();
        let alpha_remote = cache_test_actor(&context.beta, "author", "alpha.test", alpha_key).await;
        let _beta_remote = cache_test_actor(&context.alpha, "liker", "beta.test", beta_key).await;
        let local_status =
            create_public_test_status(&context.alpha, author.id, "like target").await;
        let remote_status = cache_test_status(
            &context.beta,
            alpha_remote.id,
            &super::status_url(&context.alpha, "author", local_status.id),
        )
        .await;

        let activity_id = super::enqueue_remote_favourite(&context.beta, liker.id, &remote_status)
            .await
            .unwrap();
        roosty_db::favourite_remote_status(
            &context.beta.db,
            liker.id,
            remote_status.id,
            &activity_id,
        )
        .await
        .unwrap();
        deliver_test_job(
            &context.beta,
            roosty_db::JobKind::FederationFavouriteDelivery,
        )
        .await;
        assert_eq!(
            roosty_db::count_local_favourites(&context.alpha.db, local_status.id)
                .await
                .unwrap(),
            1
        );

        let favourite =
            roosty_db::unfavourite_remote_status(&context.beta.db, liker.id, remote_status.id)
                .await
                .unwrap()
                .unwrap();
        super::enqueue_remote_unfavourite(&context.beta, favourite)
            .await
            .unwrap();
        deliver_test_job(
            &context.beta,
            roosty_db::JobKind::FederationFavouriteDelivery,
        )
        .await;
        assert_eq!(
            roosty_db::count_local_favourites(&context.alpha.db, local_status.id)
                .await
                .unwrap(),
            0
        );

        test_transport::clear_inboxes();
        context.teardown().await;
    }

    /// Given a cached public remote Note, when its local follower boosts then unboosts it, then
    /// signed Announce and Undo activities update the origin's boost count.
    #[tokio::test]
    async fn remote_reblog_and_undo_are_delivered_to_the_status_origin() {
        let _guard = FEDERATION_TEST_LOCK.lock().await;
        let context = FederationTestContext::setup().await;
        test_transport::register_inbox("alpha.test", context.alpha.clone());
        test_transport::register_inbox("beta.test", context.beta.clone());

        let author = create_test_account(&context.alpha, "author").await;
        let booster = create_test_account(&context.beta, "booster").await;
        let alpha_key = super::ensure_actor_key(&context.alpha, author.id)
            .await
            .unwrap();
        let beta_key = super::ensure_actor_key(&context.beta, booster.id)
            .await
            .unwrap();
        let alpha_remote = cache_test_actor(&context.beta, "author", "alpha.test", alpha_key).await;
        let _beta_remote = cache_test_actor(&context.alpha, "booster", "beta.test", beta_key).await;
        let local_status =
            create_public_test_status(&context.alpha, author.id, "boost target").await;
        let remote_status = cache_test_status(
            &context.beta,
            alpha_remote.id,
            &super::status_url(&context.alpha, "author", local_status.id),
        )
        .await;

        let activity_id = super::enqueue_remote_reblog(&context.beta, booster.id, &remote_status)
            .await
            .unwrap();
        roosty_db::reblog_remote_status(
            &context.beta.db,
            booster.id,
            remote_status.id,
            &activity_id,
        )
        .await
        .unwrap();
        deliver_test_job(&context.beta, roosty_db::JobKind::FederationReblogDelivery).await;
        assert_eq!(
            roosty_db::count_local_reblogs(&context.alpha.db, local_status.id)
                .await
                .unwrap(),
            1
        );

        let reblog =
            roosty_db::unreblog_remote_status(&context.beta.db, booster.id, remote_status.id)
                .await
                .unwrap()
                .unwrap();
        super::enqueue_remote_unreblog(&context.beta, reblog)
            .await
            .unwrap();
        deliver_test_job(&context.beta, roosty_db::JobKind::FederationReblogDelivery).await;
        assert_eq!(
            roosty_db::count_local_reblogs(&context.alpha.db, local_status.id)
                .await
                .unwrap(),
            0
        );

        test_transport::clear_inboxes();
        context.teardown().await;
    }

    /// Public is addressed in `to`, while unlisted retains the public audience in `cc`.
    #[test]
    fn classifies_only_public_and_unlisted_remote_notes() {
        let note = |to: Vec<&str>, cc: Vec<&str>| InboundNote {
            id: "https://remote.example/notes/1".to_owned(),
            r#type: "Note".to_owned(),
            attributed_to: "https://remote.example/users/alice".to_owned(),
            content: "hello".to_owned(),
            published: "2026-07-13T12:00:00Z".to_owned(),
            updated: None,
            to: to.into_iter().map(str::to_owned).collect(),
            cc: cc.into_iter().map(str::to_owned).collect(),
            in_reply_to: None,
            tag: Vec::new(),
            attachment: Vec::new(),
        };
        let public = "https://www.w3.org/ns/activitystreams#Public";

        assert_eq!(
            remote_status_visibility(&note(vec![public], vec![])),
            Some("public")
        );
        assert_eq!(
            remote_status_visibility(&note(vec![], vec![public])),
            Some("unlisted")
        );
        assert_eq!(remote_status_visibility(&note(vec![], vec![])), None);
    }

    /// Given public ActivityStreams payloads, when serialized, then their property names use the
    /// ActivityStreams camelCase spelling required by Mastodon.
    #[test]
    fn serializes_activitystreams_property_names() {
        let actor = Actor {
            context: actor_context(),
            id: "https://example.test/users/alice".to_owned(),
            r#type: ActorType::Person,
            preferred_username: "alice".to_owned(),
            name: "Alice".to_owned(),
            summary: String::new(),
            inbox: "https://example.test/users/alice/inbox".to_owned(),
            outbox: "https://example.test/users/alice/outbox".to_owned(),
            followers: "https://example.test/users/alice/followers".to_owned(),
            following: "https://example.test/users/alice/following".to_owned(),
            url: "https://example.test/@alice".to_owned(),
            manually_approves_followers: false,
            discoverable: true,
            published: "2026-07-13T00:00:00.000Z".to_owned(),
            attachment: actor_profile_fields(&serde_json::json!([
                { "name": "Website", "value": "https://example.test/?a=<b>" }
            ])),
            icon: Some(ActorImage {
                r#type: ActorImageType::Image,
                media_type: "image/png".to_owned(),
                url: "https://example.test/media_attachments/files/accounts/alice-avatar.png"
                    .to_owned(),
            }),
            image: Some(ActorImage {
                r#type: ActorImageType::Image,
                media_type: "image/png".to_owned(),
                url: "https://example.test/media_attachments/files/accounts/alice-header.png"
                    .to_owned(),
            }),
            public_key: PublicKey {
                id: "https://example.test/users/alice#main-key".to_owned(),
                owner: "https://example.test/users/alice".to_owned(),
                public_key_pem: "public-key".to_owned(),
            },
        };
        let note = Note {
            context: "https://www.w3.org/ns/activitystreams",
            id: "https://example.test/users/alice/statuses/1".to_owned(),
            r#type: NoteType::Note,
            attributed_to: "https://example.test/users/alice".to_owned(),
            content: "Hello".to_owned(),
            published: "2026-07-13T00:00:00Z".to_owned(),
            updated: "2026-07-13T00:00:00Z".to_owned(),
            in_reply_to: None,
            tag: vec![MentionTag {
                r#type: MentionType::Mention,
                href: "https://example.test/users/bob".to_owned(),
                name: "@bob".to_owned(),
            }],
            to: vec!["https://www.w3.org/ns/activitystreams#Public".to_owned()],
            cc: Vec::new(),
        };
        let collection = OrderedCollection {
            context: "https://www.w3.org/ns/activitystreams",
            r#type: CollectionType::OrderedCollection,
            total_items: 1,
            ordered_items: vec![Create {
                context: "https://www.w3.org/ns/activitystreams",
                r#type: CreateType::Create,
                id: "https://example.test/users/alice/statuses/1#create".to_owned(),
                actor: "https://example.test/users/alice".to_owned(),
                published: "2026-07-13T00:00:00Z".to_owned(),
                to: vec!["https://www.w3.org/ns/activitystreams#Public".to_owned()],
                cc: Vec::new(),
                object: note,
            }],
        };

        let actor = serde_json::to_value(actor).unwrap();
        let collection = serde_json::to_value(collection).unwrap();

        assert_eq!(actor["preferredUsername"], "alice");
        assert_eq!(
            actor["@context"][0],
            "https://www.w3.org/ns/activitystreams"
        );
        assert_eq!(actor["@context"][1], "https://w3id.org/security/v1");
        assert_eq!(
            actor["@context"][2]["manuallyApprovesFollowers"],
            "as:manuallyApprovesFollowers"
        );
        assert_eq!(actor["@context"][2]["discoverable"], "toot:discoverable");
        assert_eq!(actor["@context"][2]["schema"], "http://schema.org#");
        assert_eq!(
            actor["@context"][2]["PropertyValue"],
            "schema:PropertyValue"
        );
        assert_eq!(actor["@context"][2]["value"], "schema:value");
        assert_eq!(actor["url"], "https://example.test/@alice");
        assert!(actor["discoverable"].as_bool().unwrap());
        assert_eq!(actor["published"], "2026-07-13T00:00:00.000Z");
        assert_eq!(actor["attachment"][0]["type"], "PropertyValue");
        assert_eq!(actor["attachment"][0]["name"], "Website");
        assert_eq!(
            actor["attachment"][0]["value"],
            "https://example.test/?a=&lt;b&gt;"
        );
        assert_eq!(local_actor_type(false), ActorType::Person);
        assert_eq!(local_actor_type(true), ActorType::Service);
        assert!(actor.get("preferred_username").is_none());
        assert_eq!(actor["icon"]["type"], "Image");
        assert_eq!(
            actor["icon"]["url"],
            "https://example.test/media_attachments/files/accounts/alice-avatar.png"
        );
        assert_eq!(actor["image"]["type"], "Image");
        assert_eq!(
            actor["image"]["url"],
            "https://example.test/media_attachments/files/accounts/alice-header.png"
        );
        assert_eq!(collection["totalItems"], 1);
        assert!(collection.get("total_items").is_none());
        assert!(collection.get("ordered_items").is_none());
        assert_eq!(
            collection["orderedItems"][0]["object"]["attributedTo"],
            "https://example.test/users/alice"
        );
        assert_eq!(
            collection["orderedItems"][0]["object"]["tag"][0]["type"],
            "Mention"
        );
        assert!(
            collection["orderedItems"][0]["object"]
                .get("attributed_to")
                .is_none()
        );
    }

    struct FederationTestContext {
        postgresql: PostgreSQL,
        alpha: AppState,
        beta: AppState,
        alpha_database: String,
        beta_database: String,
        _temp_dir: TempDir,
    }

    impl FederationTestContext {
        /// Start two migrated databases with distinct public federation identities.
        async fn setup() -> Self {
            let temp_dir = tempfile::Builder::new()
                .prefix("roosty-federation-")
                .tempdir()
                .unwrap();
            let alpha_database = format!("alpha_{}", uuid::Uuid::now_v7().simple());
            let beta_database = format!("beta_{}", uuid::Uuid::now_v7().simple());
            let data_dir = temp_dir.path().join("data");
            let password_file = temp_dir.path().join("passwords").join("pgpass");
            std::fs::create_dir_all(password_file.parent().unwrap()).unwrap();

            let settings = crate::test_postgres::settings(&data_dir, password_file);
            let mut postgresql = PostgreSQL::new(settings);
            postgresql.setup().await.unwrap();
            postgresql.start().await.unwrap();
            postgresql.create_database(&alpha_database).await.unwrap();
            postgresql.create_database(&beta_database).await.unwrap();
            let alpha_url = postgresql.settings().url(&alpha_database);
            let beta_url = postgresql.settings().url(&beta_database);
            let alpha_db = roosty_db::connect(&alpha_url).await.unwrap();
            let beta_db = roosty_db::connect(&beta_url).await.unwrap();
            Migrator::up(&alpha_db, None).await.unwrap();
            Migrator::up(&beta_db, None).await.unwrap();

            Self {
                postgresql,
                alpha: AppState::new(test_config(alpha_url, "https://alpha.test"), alpha_db),
                beta: AppState::new(test_config(beta_url, "https://beta.test"), beta_db),
                alpha_database,
                beta_database,
                _temp_dir: temp_dir,
            }
        }

        /// Stop both databases after the transport registry has been cleared.
        async fn teardown(self) {
            self.alpha.db.close().await.unwrap();
            self.beta.db.close().await.unwrap();
            self.postgresql
                .drop_database(&self.alpha_database)
                .await
                .unwrap();
            self.postgresql
                .drop_database(&self.beta_database)
                .await
                .unwrap();
            self.postgresql.stop().await.unwrap();
        }
    }

    fn test_config(database_url: String, public_base_url: &str) -> Config {
        Config {
            database_url,
            public_base_url: public_base_url.parse().unwrap(),
            listen_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            infra_listen_addr: None,
            session_secret: "test-session-secret-change-me-000".to_owned(),
            token_pepper: "test-token-pepper-change-me-0000".to_owned(),
            object_storage_backend: "local".to_owned(),
            media_root: "./media".to_owned(),
            registration_mode: "closed".to_owned(),
            federation_enabled: true,
            federation_key_encryption_secret: Some(
                "test-federation-key-encryption-secret-000".to_owned(),
            ),
            federation_allowed_domains: vec!["*".to_owned()],
            federation_blocked_domains: Vec::new(),
            federation_delivery_max_age: time::Duration::days(7),
            remote_media_cache_ttl: time::Duration::days(30),
            remote_media_max_bytes: 40 * 1024 * 1024,
            remote_media_fetch_concurrency: 5,
            worker_concurrency: 4,
            instance_name: "Federation test".to_owned(),
            instance_description: None,
        }
    }

    async fn create_test_account(state: &AppState, username: &str) -> roosty_db::LocalAccount {
        roosty_db::create_local_account(
            &state.db,
            username,
            &format!("{username}@example.test"),
            "not-a-login-password",
        )
        .await
        .unwrap();
        roosty_db::find_local_account_by_username(&state.db, username)
            .await
            .unwrap()
            .unwrap()
    }

    async fn lock_test_account(state: &AppState, account_id: AccountId) {
        roosty_db::update_local_account_settings(
            &state.db,
            account_id,
            roosty_db::LocalAccountSettingsUpdate {
                locked: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }

    async fn follow_test_actor(
        state: &AppState,
        local_account_id: AccountId,
        remote_actor_id: AccountId,
    ) {
        let activity_id = super::enqueue_remote_follow(state, local_account_id, remote_actor_id)
            .await
            .unwrap();
        roosty_db::create_remote_following(
            &state.db,
            local_account_id,
            remote_actor_id,
            &activity_id,
        )
        .await
        .unwrap();
    }

    async fn cache_test_actor(
        state: &AppState,
        username: &str,
        domain: &str,
        public_key_pem: String,
    ) -> roosty_db::RemoteActor {
        let actor = roosty_db::RemoteActor {
            id: AccountId(uuid::Uuid::now_v7()),
            activitypub_id: format!("https://{domain}/users/{username}"),
            username: username.to_owned(),
            domain: domain.to_owned(),
            display_name: username.to_owned(),
            summary: String::new(),
            emojis: json!([]),
            inbox_url: format!("https://{domain}/inbox"),
            shared_inbox_url: None,
            public_key_id: format!("https://{domain}/users/{username}#main-key"),
            public_key_pem,
            expires_at: time::OffsetDateTime::now_utc() + time::Duration::hours(1),
            profile_created_at: None,
            first_seen_at: time::OffsetDateTime::now_utc(),
            deleted_at: None,
            moved_to_remote_actor_id: None,
        };
        roosty_db::upsert_remote_actor(&state.db, &actor)
            .await
            .unwrap()
    }

    async fn create_public_test_status(
        state: &AppState,
        account_id: AccountId,
        content: &str,
    ) -> roosty_db::LocalStatus {
        roosty_db::create_local_status(
            &state.db,
            roosty_db::NewLocalStatus {
                account_id,
                content: content.to_owned(),
                visibility: StatusVisibility::Public,
                sensitive: false,
                spoiler_text: String::new(),
                language: None,
                in_reply_to_id: None,
                in_reply_to_remote_status_id: None,
            },
        )
        .await
        .unwrap()
    }

    async fn cache_test_status(
        state: &AppState,
        remote_actor_id: AccountId,
        activitypub_id: &str,
    ) -> roosty_db::RemoteStatus {
        roosty_db::upsert_remote_status(
            &state.db,
            roosty_db::NewRemoteStatus {
                activitypub_id: activitypub_id.to_owned(),
                remote_actor_id,
                content: "cached remote status".to_owned(),
                visibility: StatusVisibility::Public,
                published_at: time::OffsetDateTime::now_utc(),
                updated_at: time::OffsetDateTime::now_utc(),
                in_reply_to: None,
                in_reply_to_local_status_id: None,
                in_reply_to_remote_status_id: None,
                object: serde_json::json!({}),
            },
        )
        .await
        .unwrap()
    }

    async fn deliver_test_job(state: &AppState, kind: roosty_db::JobKind) {
        let job =
            roosty_db::claim_due_job(&state.db, "federation-test", time::Duration::minutes(1))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(job.kind, kind.as_str());
        match kind {
            roosty_db::JobKind::FederationFollowResponse => {
                super::deliver_follow_response(state, job.payload.clone())
                    .await
                    .unwrap();
            }
            roosty_db::JobKind::FederationStatusDelivery => {
                super::deliver_status_activity(state, job.payload.clone())
                    .await
                    .unwrap();
            }
            roosty_db::JobKind::FederationFollowDelivery => {
                super::deliver_follow_activity(state, job.payload.clone())
                    .await
                    .unwrap();
            }
            roosty_db::JobKind::FederationFavouriteDelivery => {
                super::deliver_favourite_activity(state, job.payload.clone())
                    .await
                    .unwrap();
            }
            roosty_db::JobKind::FederationReblogDelivery => {
                super::deliver_reblog_activity(state, job.payload.clone())
                    .await
                    .unwrap();
            }
            roosty_db::JobKind::FederationActorUpdateDelivery => {
                super::deliver_actor_update(state, job.payload.clone())
                    .await
                    .unwrap();
            }
            roosty_db::JobKind::FederationRemoteMediaFetch => {
                crate::media::fetch_remote_media(state, job.payload.clone())
                    .await
                    .unwrap();
            }
        }
        assert!(
            roosty_db::mark_job_completed(&state.db, &job)
                .await
                .unwrap()
        );
    }
}
