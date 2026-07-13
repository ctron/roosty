//! ActivityPub discovery and public-object endpoints for local actors.

pub(crate) mod discovery;

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
use rsa::{
    RsaPrivateKey,
    pkcs1v15::SigningKey,
    pkcs1v15::{Signature as RsaSignature, VerifyingKey},
    pkcs8::{DecodePrivateKey, DecodePublicKey, EncodePrivateKey, EncodePublicKey, LineEnding},
    signature::{SignatureEncoding, Signer, Verifier},
};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::http::AppState;

const ACTIVITYSTREAMS_CONTENT_TYPE: &str = "application/activity+json";
const JRD_CONTENT_TYPE: &str = "application/jrd+json";
const ACTIVITYSTREAMS_CONTEXT: &str = "https://www.w3.org/ns/activitystreams";
const PUBLIC_AUDIENCE: &str = "https://www.w3.org/ns/activitystreams#Public";
const DELIVERY_JOB_KIND: &str = "federation_follow_response";

/// ActivityStreams actor types accepted and emitted by Roosty.
#[derive(Deserialize, Serialize, PartialEq, Eq)]
enum ActorType {
    Person,
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
    context: &'static str,
    id: String,
    r#type: ActorType,
    preferred_username: String,
    name: String,
    summary: String,
    inbox: String,
    outbox: String,
    followers: String,
    following: String,
    manually_approves_followers: bool,
    public_key: PublicKey,
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
    to: Vec<&'static str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Create {
    r#type: CreateType,
    id: String,
    actor: String,
    published: String,
    to: Vec<&'static str>,
    object: Note,
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
    let id = actor_url(&state, &account.username);
    activity_response(Actor {
        context: ACTIVITYSTREAMS_CONTEXT,
        id: id.clone(),
        r#type: ActorType::Person,
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
        manually_approves_followers: account.locked,
        public_key: PublicKey {
            id: format!("{id}#main-key"),
            owner: id,
            public_key_pem,
        },
    })
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
            let items = statuses
                .into_iter()
                .map(|status| create(&state, &account.username, status))
                .collect();
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
        Ok(Some(status)) if status.visibility == "public" => {
            match roosty_db::find_local_account_by_id(&state.db, status.account_id).await {
                Ok(Some(account)) if account.username == username => {
                    activity_response(note_object(&state, &username, status))
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
async fn followers(State(state): State<AppState>, Path(username): Path<String>) -> Response {
    let Some(account) = account_for_collection(&state, &username).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match (
        roosty_db::count_local_followers(&state.db, account.id).await,
        roosty_db::count_remote_followers(&state.db, account.id).await,
    ) {
        (Ok(local), Ok(remote)) => activity_response(Collection {
            context: ACTIVITYSTREAMS_CONTEXT,
            id: format!("{}/followers", actor_url(&state, &username)),
            r#type: CollectionType::Collection,
            total_items: local + remote,
        }),
        (Err(error), _) | (_, Err(error)) => internal_error(error),
    }
}

/// Serve the actor's following collection metadata without leaking local-only details.
async fn following(State(state): State<AppState>, Path(username): Path<String>) -> Response {
    let Some(account) = account_for_collection(&state, &username).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match roosty_db::count_local_following(&state.db, account.id).await {
        Ok(total_items) => activity_response(Collection {
            context: ACTIVITYSTREAMS_CONTEXT,
            id: format!("{}/following", actor_url(&state, &username)),
            r#type: CollectionType::Collection,
            total_items,
        }),
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
    if !matches!(
        activity.get("type").and_then(JsonValue::as_str),
        Some("Follow") | Some("Undo")
    ) {
        return StatusCode::ACCEPTED.into_response();
    }
    if !roosty_db::record_processed_inbox_activity(&state.db, &activity_id, remote_actor.id)
        .await
        .unwrap_or(false)
    {
        return StatusCode::ACCEPTED.into_response();
    }
    if activity.get("type").and_then(JsonValue::as_str) == Some("Undo") {
        if let Some(original_id) = activity.get("object").and_then(JsonValue::as_str) {
            let _ = roosty_db::delete_remote_follow_by_activity(&state.db, original_id).await;
        }
        return StatusCode::ACCEPTED.into_response();
    }
    let Some(target_url) = activity.get("object").and_then(JsonValue::as_str) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
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
    let state_name = if local_account.locked {
        "pending"
    } else {
        "accepted"
    };
    match roosty_db::upsert_remote_follow(
        &state.db,
        remote_actor.id,
        local_account.id,
        &activity_id,
        activity.clone(),
        state_name,
    )
    .await
    {
        Ok(_) => {
            if let Err(error) = crate::notifications::create_and_stream_remote_follow_notification(
                state,
                local_account.id,
                remote_actor.id,
            )
            .await
            {
                tracing::warn!(%error, "failed to create remote follow notification");
            }
            if state_name == "accepted"
                && let Err(error) = enqueue_follow_response(
                    state,
                    local_account.id,
                    remote_actor.id,
                    activity.clone(),
                    "Accept",
                )
                .await
            {
                tracing::warn!(%error, "failed to enqueue follow Accept");
            }
            StatusCode::ACCEPTED.into_response()
        }
        Err(error) => internal_error(error),
    }
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
    response_type: String,
}

/// Queue a signed Accept or Reject response after a local follow decision.
pub(crate) async fn enqueue_follow_response(
    state: &AppState,
    local_account_id: AccountId,
    remote_actor_id: AccountId,
    follow: JsonValue,
    response_type: &str,
) -> Result<(), RoostyError> {
    let follow_id = follow
        .get("id")
        .and_then(JsonValue::as_str)
        .map(str::to_owned)
        .ok_or_else(|| RoostyError::InvalidInput("follow activity has no ID".to_owned()))?;
    let payload = serde_json::to_value(FollowResponseDelivery {
        local_account_id,
        remote_actor_id,
        follow,
        response_type: response_type.to_owned(),
    })
    .map_err(|error| RoostyError::InvalidInput(error.to_string()))?;
    roosty_db::enqueue_job(
        &state.db,
        DELIVERY_JOB_KIND,
        payload,
        Some(&format!("{response_type}:{follow_id}")),
        OffsetDateTime::now_utc(),
    )
    .await?;
    Ok(())
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
    let activity = serde_json::json!({"@context": ACTIVITYSTREAMS_CONTEXT, "id": format!("{actor}#{}-{}", payload.response_type.to_ascii_lowercase(), Uuid::now_v7()), "type": payload.response_type, "actor": actor, "object": payload.follow});
    signed_post(
        state,
        &remote.inbox_url,
        &private_key,
        &format!("{}#main-key", actor_url(state, &local.username)),
        &activity,
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
    let address = discovery::validate_remote_url(state, &url).await?;
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

fn create(state: &AppState, username: &str, status: roosty_db::LocalStatus) -> Create {
    let object = note_object(state, username, status);
    Create {
        r#type: CreateType::Create,
        id: format!("{}#create", object.id),
        actor: object.attributed_to.clone(),
        published: object.published.clone(),
        to: vec![PUBLIC_AUDIENCE],
        object,
    }
}
fn note_object(state: &AppState, username: &str, status: roosty_db::LocalStatus) -> Note {
    let id = status_url(state, username, status.id);
    Note {
        context: ACTIVITYSTREAMS_CONTEXT,
        id,
        r#type: NoteType::Note,
        attributed_to: actor_url(state, username),
        content: status.content,
        published: crate::statuses::format_timestamp(status.created_at),
        updated: crate::statuses::format_timestamp(status.updated_at),
        to: vec![PUBLIC_AUDIENCE],
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
    use super::{
        Actor, ActorType, CollectionType, Create, CreateType, Note, NoteType, OrderedCollection,
        PublicKey, parse_acct,
    };

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

    /// Given public ActivityStreams payloads, when serialized, then their property names use the
    /// ActivityStreams camelCase spelling required by Mastodon.
    #[test]
    fn serializes_activitystreams_property_names() {
        let actor = Actor {
            context: "https://www.w3.org/ns/activitystreams",
            id: "https://example.test/users/alice".to_owned(),
            r#type: ActorType::Person,
            preferred_username: "alice".to_owned(),
            name: "Alice".to_owned(),
            summary: String::new(),
            inbox: "https://example.test/users/alice/inbox".to_owned(),
            outbox: "https://example.test/users/alice/outbox".to_owned(),
            followers: "https://example.test/users/alice/followers".to_owned(),
            following: "https://example.test/users/alice/following".to_owned(),
            manually_approves_followers: false,
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
            to: vec!["https://www.w3.org/ns/activitystreams#Public"],
        };
        let collection = OrderedCollection {
            context: "https://www.w3.org/ns/activitystreams",
            r#type: CollectionType::OrderedCollection,
            total_items: 1,
            ordered_items: vec![Create {
                r#type: CreateType::Create,
                id: "https://example.test/users/alice/statuses/1#create".to_owned(),
                actor: "https://example.test/users/alice".to_owned(),
                published: "2026-07-13T00:00:00Z".to_owned(),
                to: vec!["https://www.w3.org/ns/activitystreams#Public"],
                object: note,
            }],
        };

        let actor = serde_json::to_value(actor).unwrap();
        let collection = serde_json::to_value(collection).unwrap();

        assert_eq!(actor["preferredUsername"], "alice");
        assert!(actor.get("preferred_username").is_none());
        assert_eq!(collection["totalItems"], 1);
        assert!(collection.get("total_items").is_none());
        assert!(collection.get("ordered_items").is_none());
        assert_eq!(
            collection["orderedItems"][0]["object"]["attributedTo"],
            "https://example.test/users/alice"
        );
        assert!(
            collection["orderedItems"][0]["object"]
                .get("attributed_to")
                .is_none()
        );
    }
}
