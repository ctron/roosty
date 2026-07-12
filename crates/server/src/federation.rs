//! ActivityPub discovery and public-object endpoints for local actors.

pub(crate) mod discovery;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use rand_core::{OsRng, RngCore};
use ring::{aead, digest};
use roost_core::{AccountId, RoostError, StatusId};
use rsa::{
    RsaPrivateKey,
    pkcs8::{EncodePrivateKey, EncodePublicKey, LineEnding},
};
use serde::{Deserialize, Serialize};

use crate::http::AppState;

const ACTIVITYSTREAMS_CONTENT_TYPE: &str = "application/activity+json";
const JRD_CONTENT_TYPE: &str = "application/jrd+json";
const ACTIVITYSTREAMS_CONTEXT: &str = "https://www.w3.org/ns/activitystreams";
const PUBLIC_AUDIENCE: &str = "https://www.w3.org/ns/activitystreams#Public";

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
    #[serde(rename = "type")]
    media_type: &'static str,
    href: String,
}

#[derive(Serialize)]
struct PublicKey {
    id: String,
    owner: String,
    #[serde(rename = "publicKeyPem")]
    public_key_pem: String,
}

#[derive(Serialize)]
struct Actor {
    #[serde(rename = "@context")]
    context: &'static str,
    id: String,
    #[serde(rename = "type")]
    actor_type: &'static str,
    preferred_username: String,
    name: String,
    summary: String,
    inbox: String,
    outbox: String,
    followers: String,
    following: String,
    #[serde(rename = "publicKey")]
    public_key: PublicKey,
}

#[derive(Serialize)]
struct Note {
    #[serde(rename = "@context")]
    context: &'static str,
    id: String,
    #[serde(rename = "type")]
    note_type: &'static str,
    attributed_to: String,
    content: String,
    published: String,
    updated: String,
    to: Vec<&'static str>,
}

#[derive(Serialize)]
struct Create {
    #[serde(rename = "type")]
    activity_type: &'static str,
    id: String,
    actor: String,
    published: String,
    to: Vec<&'static str>,
    object: Note,
}

#[derive(Serialize)]
struct OrderedCollection {
    #[serde(rename = "@context")]
    context: &'static str,
    #[serde(rename = "type")]
    collection_type: &'static str,
    total_items: u64,
    ordered_items: Vec<Create>,
}

#[derive(Serialize)]
struct Collection {
    #[serde(rename = "@context")]
    context: &'static str,
    id: String,
    #[serde(rename = "type")]
    collection_type: &'static str,
    #[serde(rename = "totalItems")]
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
    match roost_db::find_local_account_by_username(&state.db, username).await {
        Ok(Some(_)) => {
            let subject = format!("acct:{username}@{domain}");
            (
                [(header::CONTENT_TYPE, JRD_CONTENT_TYPE)],
                Json(WebFinger {
                    subject,
                    links: vec![WebFingerLink {
                        rel: "self",
                        media_type: ACTIVITYSTREAMS_CONTENT_TYPE,
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
    let account = match roost_db::find_local_account_by_username(&state.db, &username).await {
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
        actor_type: "Person",
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
    let account = match roost_db::find_local_account_by_username(&state.db, &username).await {
        Ok(Some(account)) => account,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(error) => return internal_error(error),
    };
    match roost_db::public_local_statuses_by_account(&state.db, account.id, 20).await {
        Ok(statuses) => {
            let items = statuses
                .into_iter()
                .map(|status| create(&state, &account.username, status))
                .collect();
            match roost_db::count_public_local_statuses_by_account(&state.db, account.id).await {
                Ok(total_items) => activity_response(OrderedCollection {
                    context: ACTIVITYSTREAMS_CONTEXT,
                    collection_type: "OrderedCollection",
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
    match roost_db::find_local_status_by_id(&state.db, StatusId(id)).await {
        Ok(Some(status)) if status.visibility == "public" => {
            match roost_db::find_local_account_by_id(&state.db, status.account_id).await {
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
    match roost_db::count_local_followers(&state.db, account.id).await {
        Ok(total_items) => activity_response(Collection {
            context: ACTIVITYSTREAMS_CONTEXT,
            id: format!("{}/followers", actor_url(&state, &username)),
            collection_type: "Collection",
            total_items,
        }),
        Err(error) => internal_error(error),
    }
}

/// Serve the actor's following collection metadata without leaking local-only details.
async fn following(State(state): State<AppState>, Path(username): Path<String>) -> Response {
    let Some(account) = account_for_collection(&state, &username).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match roost_db::count_local_following(&state.db, account.id).await {
        Ok(total_items) => activity_response(Collection {
            context: ACTIVITYSTREAMS_CONTEXT,
            id: format!("{}/following", actor_url(&state, &username)),
            collection_type: "Collection",
            total_items,
        }),
        Err(error) => internal_error(error),
    }
}

async fn account_for_collection(
    state: &AppState,
    username: &str,
) -> Option<roost_db::LocalAccount> {
    if !state.config.federation_enabled {
        return None;
    }
    match roost_db::find_local_account_by_username(&state.db, username).await {
        Ok(account) => account,
        Err(error) => {
            tracing::error!(%error, "could not load ActivityPub collection actor");
            None
        }
    }
}

/// Reject inbound delivery until signature verification and inbox processing are enabled.
async fn inbox(State(state): State<AppState>) -> Response {
    if state.config.federation_enabled {
        StatusCode::NOT_IMPLEMENTED.into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

fn create(state: &AppState, username: &str, status: roost_db::LocalStatus) -> Create {
    let object = note_object(state, username, status);
    Create {
        activity_type: "Create",
        id: format!("{}#create", object.id),
        actor: object.attributed_to.clone(),
        published: object.published.clone(),
        to: vec![PUBLIC_AUDIENCE],
        object,
    }
}
fn note_object(state: &AppState, username: &str, status: roost_db::LocalStatus) -> Note {
    let id = status_url(state, username, status.id);
    Note {
        context: ACTIVITYSTREAMS_CONTEXT,
        id,
        note_type: "Note",
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
async fn ensure_actor_key(state: &AppState, account_id: AccountId) -> Result<String, RoostError> {
    if let Some(key) = roost_db::find_local_actor_key(&state.db, account_id).await? {
        return Ok(key.public_key_pem);
    }
    let private_key = RsaPrivateKey::new(&mut OsRng, 2048).map_err(|error| {
        RoostError::Configuration(format!("could not generate actor key: {error}"))
    })?;
    let public_key_pem = private_key
        .to_public_key()
        .to_public_key_pem(LineEnding::LF)
        .map_err(|error| {
            RoostError::Configuration(format!("could not encode actor public key: {error}"))
        })?;
    let private_key_pem = private_key.to_pkcs8_pem(LineEnding::LF).map_err(|error| {
        RoostError::Configuration(format!("could not encode actor private key: {error}"))
    })?;
    let mut nonce = [0_u8; 12];
    OsRng.fill_bytes(&mut nonce);
    let mut ciphertext = private_key_pem.as_bytes().to_vec();
    let secret = state
        .config
        .federation_key_encryption_secret
        .as_deref()
        .ok_or_else(|| {
            RoostError::Configuration("federation key encryption secret is unavailable".to_owned())
        })?;
    let key_bytes = digest::digest(&digest::SHA256, secret.as_bytes());
    let key = aead::LessSafeKey::new(
        aead::UnboundKey::new(&aead::AES_256_GCM, key_bytes.as_ref()).map_err(|_| {
            RoostError::Configuration("invalid federation key encryption key".to_owned())
        })?,
    );
    key.seal_in_place_append_tag(
        aead::Nonce::assume_unique_for_key(nonce),
        aead::Aad::empty(),
        &mut ciphertext,
    )
    .map_err(|_| RoostError::Configuration("could not encrypt actor key".to_owned()))?;
    let stored = roost_db::LocalActorKey {
        public_key_pem: public_key_pem.clone(),
        private_key_ciphertext: ciphertext,
        private_key_nonce: nonce.to_vec(),
    };
    match roost_db::create_local_actor_key(&state.db, account_id, &stored).await {
        Ok(()) => Ok(public_key_pem),
        Err(_) => roost_db::find_local_actor_key(&state.db, account_id)
            .await?
            .map(|key| key.public_key_pem)
            .ok_or_else(|| {
                RoostError::Configuration("actor key could not be persisted".to_owned())
            }),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_acct;

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
}
