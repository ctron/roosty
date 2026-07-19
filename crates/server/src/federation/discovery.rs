//! Safe WebFinger and ActivityPub actor discovery for remote accounts.

use std::{
    net::{IpAddr, SocketAddr},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use reqwest::{
    Client,
    header::{ACCEPT, CONTENT_TYPE},
};
use roosty_core::{AccountId, FederationDiscoveryError, Result, RoostyError};
use roosty_db::{NewRemoteCustomEmoji, NewRemoteProfileMedia, RemoteActor};
use sea_orm::{AccessMode, ConnectionTrait, DatabaseBackend, Statement, TransactionTrait};
use serde::Deserialize;
use serde_json::Value as JsonValue;
use time::{Duration as TimeDuration, OffsetDateTime, format_description::well_known::Rfc3339};
use url::Url;
use uuid::Uuid;

use crate::{federation::ActorType, http::AppState};

const MAX_FEDERATION_RESPONSE_BYTES: usize = 1_048_576;
static DISCOVERY_CACHE_HIT: AtomicU64 = AtomicU64::new(0);
static DISCOVERY_RESOLVED: AtomicU64 = AtomicU64::new(0);
static DISCOVERY_POLICY_REJECTED: AtomicU64 = AtomicU64::new(0);
static DISCOVERY_FAILED: AtomicU64 = AtomicU64::new(0);

#[derive(Deserialize)]
struct WebFingerResponse {
    subject: String,
    links: Vec<WebFingerLink>,
}

#[derive(Deserialize)]
struct WebFingerLink {
    rel: String,
    r#type: Option<String>,
    href: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteActorDocument {
    id: String,
    r#type: ActorType,
    #[serde(default)]
    preferred_username: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    icon: Option<RemoteActorImage>,
    #[serde(default)]
    image: Option<RemoteActorImage>,
    #[serde(default)]
    tag: Vec<JsonValue>,
    #[serde(default)]
    also_known_as: Vec<String>,
    published: Option<String>,
    inbox: String,
    #[serde(default)]
    followers: Option<String>,
    #[serde(default)]
    featured: Option<String>,
    #[serde(default)]
    featured_tags: Option<String>,
    #[serde(default)]
    endpoints: RemoteEndpoints,
    public_key: RemotePublicKey,
}

/// ActivityStreams allows image references as either URLs or Image objects.
#[derive(Deserialize)]
#[serde(untagged)]
enum RemoteActorImage {
    Url(String),
    Object { url: String },
}

impl RemoteActorImage {
    fn into_url(self) -> String {
        match self {
            Self::Url(url) | Self::Object { url } => url,
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteEndpoints {
    shared_inbox: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemotePublicKey {
    id: String,
    owner: String,
    public_key_pem: String,
}

#[derive(Clone, Copy)]
enum RemoteActorStoreMode {
    DiscoveredHandle,
    RefreshedDocument,
}

/// Resolve and cache a remote actor after applying the configured network policy.
pub async fn resolve_remote_actor(state: &AppState, handle: &str) -> Result<RemoteActor> {
    let (username, domain) = parse_remote_handle(handle)?;
    let domain = domain.to_ascii_lowercase();
    let lock = state.db.begin().await?;
    lock.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        vec![format!("remote-account:{username}@{domain}").into()],
    ))
    .await?;
    if let Some(actor) = roosty_db::find_remote_actor_by_handle(&lock, username, &domain).await?
        && actor.expires_at > OffsetDateTime::now_utc()
    {
        lock.commit().await?;
        return Ok(actor);
    }
    let resource = format!("acct:{username}@{domain}");
    let webfinger_url = Url::parse(&format!("https://{domain}/.well-known/webfinger"))
        .map_err(|_| invalid("remote domain is invalid"))?;
    let webfinger: WebFingerResponse =
        fetch_json(state, webfinger_url, Some((&resource, "resource"))).await?;
    if !webfinger.subject.eq_ignore_ascii_case(&resource) {
        return Err(invalid(
            "WebFinger subject does not match requested account",
        ));
    }
    let actor_url = activitypub_actor_href(webfinger)
        .ok_or_else(|| invalid("WebFinger response does not include an ActivityPub actor link"))?;
    let actor_url =
        Url::parse(&actor_url).map_err(|_| invalid("WebFinger actor URL is invalid"))?;
    let document: RemoteActorDocument = fetch_json(state, actor_url.clone(), None).await?;
    validate_actor_document(&document, &actor_url, username)?;
    let profile_created_at = remote_profile_created_at(&document)?;
    let followers_url = validated_followers_url(&document.id, document.followers.as_deref())?;
    let featured_url = validated_featured_url(&document.id, document.featured.as_deref())?;
    let featured_tags_url =
        validated_featured_url(&document.id, document.featured_tags.as_deref())?;
    let actor = RemoteActor {
        id: AccountId(Uuid::now_v7()),
        activitypub_id: document.id,
        username: document.preferred_username,
        domain,
        display_name: document.name,
        summary: document.summary,
        emojis: JsonValue::Array(document.tag),
        inbox_url: document.inbox,
        shared_inbox_url: document.endpoints.shared_inbox,
        followers_url,
        featured_url,
        featured_tags_url,
        public_key_id: document.public_key.id,
        public_key_pem: document.public_key.public_key_pem,
        expires_at: OffsetDateTime::now_utc() + TimeDuration::hours(24),
        profile_created_at,
        first_seen_at: OffsetDateTime::now_utc(),
        deleted_at: None,
        moved_to_remote_actor_id: None,
        limited_at: None,
    };
    let actor = store_remote_actor_on(
        &lock,
        actor,
        document.icon,
        document.image,
        RemoteActorStoreMode::DiscoveredHandle,
    )
    .await?;
    lock.commit().await?;
    enqueue_profile_media_if_followed(state, actor.id).await?;
    Ok(actor)
}

/// Resolve an exact remote handle for account search with policy-aware failure semantics.
pub async fn resolve_remote_actor_for_search(
    state: &AppState,
    handle: &str,
) -> Result<RemoteActor> {
    let Some((username, domain)) = exact_remote_handle(handle) else {
        DISCOVERY_FAILED.fetch_add(1, Ordering::Relaxed);
        return Err(invalid("remote account handle is invalid"));
    };
    if !state.config.federation_domain_is_allowed(&domain) {
        DISCOVERY_POLICY_REJECTED.fetch_add(1, Ordering::Relaxed);
        return Err(FederationDiscoveryError::PolicyRejected(domain.into()).into());
    }
    let cached = match roosty_db::find_remote_actor_by_handle(&state.db, &username, &domain).await {
        Ok(cached) => cached,
        Err(error) => {
            DISCOVERY_FAILED.fetch_add(1, Ordering::Relaxed);
            return Err(error);
        }
    };
    let fresh = cached
        .as_ref()
        .is_some_and(|actor| actor.expires_at > OffsetDateTime::now_utc());
    match resolve_remote_actor(state, &format!("{username}@{domain}")).await {
        Ok(actor) => {
            if fresh {
                DISCOVERY_CACHE_HIT.fetch_add(1, Ordering::Relaxed);
            } else {
                DISCOVERY_RESOLVED.fetch_add(1, Ordering::Relaxed);
            }
            Ok(actor)
        }
        Err(
            error @ RoostyError::FederationDiscovery(FederationDiscoveryError::PolicyRejected(_)),
        ) => {
            DISCOVERY_POLICY_REJECTED.fetch_add(1, Ordering::Relaxed);
            Err(error)
        }
        Err(error) => {
            DISCOVERY_FAILED.fetch_add(1, Ordering::Relaxed);
            Err(error)
        }
    }
}

/// Parse only account-address syntax; URLs and local shorthand are deliberately excluded.
pub fn exact_remote_handle(handle: &str) -> Option<(String, String)> {
    let handle = handle.trim().strip_prefix('@').unwrap_or(handle.trim());
    let (username, domain) = handle.rsplit_once('@')?;
    if username.is_empty()
        || domain.is_empty()
        || username.contains('@')
        || !username
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "_.-".contains(character))
        || domain.contains(':')
        || domain.chars().any(char::is_whitespace)
    {
        return None;
    }
    let domain = match url::Host::parse(domain).ok()? {
        url::Host::Domain(domain) => domain.to_ascii_lowercase(),
        url::Host::Ipv4(_) | url::Host::Ipv6(_) => return None,
    };
    Some((username.to_owned(), domain))
}

/// Render bounded-label remote discovery counters for `/metrics`.
pub(crate) fn metrics_text() -> String {
    format!(
        concat!(
            "# HELP roosty_federation_discovery_total Remote account discovery outcomes.\n",
            "# TYPE roosty_federation_discovery_total counter\n",
            "roosty_federation_discovery_total{{outcome=\"cache_hit\"}} {}\n",
            "roosty_federation_discovery_total{{outcome=\"resolved\"}} {}\n",
            "roosty_federation_discovery_total{{outcome=\"policy_rejected\"}} {}\n",
            "roosty_federation_discovery_total{{outcome=\"failed\"}} {}\n"
        ),
        DISCOVERY_CACHE_HIT.load(Ordering::Relaxed),
        DISCOVERY_RESOLVED.load(Ordering::Relaxed),
        DISCOVERY_POLICY_REJECTED.load(Ordering::Relaxed),
        DISCOVERY_FAILED.load(Ordering::Relaxed),
    )
}

/// Resolve an actor by canonical ActivityPub ID for an authenticated inbox activity.
pub async fn resolve_remote_actor_by_id(
    state: &AppState,
    activitypub_id: &str,
) -> Result<RemoteActor> {
    if let Some(actor) =
        roosty_db::find_remote_actor_by_activitypub_id(&state.db, activitypub_id).await?
        && actor.expires_at > OffsetDateTime::now_utc()
    {
        return Ok(actor);
    }
    refresh_remote_actor_by_id(state, activitypub_id).await
}

/// Re-fetch an actor document after a signed lifecycle activity.
pub async fn refresh_remote_actor_by_id(
    state: &AppState,
    activitypub_id: &str,
) -> Result<RemoteActor> {
    let (actor, icon, image) = fetch_remote_actor_by_id(state, activitypub_id).await?;
    store_remote_actor(
        state,
        actor,
        icon,
        image,
        RemoteActorStoreMode::RefreshedDocument,
    )
    .await
}

/// Fetch and store a signed actor refresh inside an inbox-owned transaction.
pub async fn refresh_remote_actor_by_id_in_transaction(
    state: &AppState,
    activitypub_id: &str,
    txn: &sea_orm::DatabaseTransaction,
) -> Result<RemoteActor> {
    let (actor, icon, image) = fetch_remote_actor_by_id(state, activitypub_id).await?;
    store_remote_actor_on(
        txn,
        actor,
        icon,
        image,
        RemoteActorStoreMode::RefreshedDocument,
    )
    .await
}

async fn fetch_remote_actor_by_id(
    state: &AppState,
    activitypub_id: &str,
) -> Result<(
    RemoteActor,
    Option<RemoteActorImage>,
    Option<RemoteActorImage>,
)> {
    let actor_url =
        Url::parse(activitypub_id).map_err(|_| invalid("remote actor ID is invalid"))?;
    let domain = actor_url
        .host_str()
        .ok_or_else(|| invalid("remote actor ID has no host"))?
        .to_ascii_lowercase();
    let document: RemoteActorDocument = fetch_json(state, actor_url.clone(), None).await?;
    if document.r#type != ActorType::Person
        || document.id != activitypub_id
        || document.preferred_username.is_empty()
        || document.public_key.owner != document.id
        || document.public_key.id.is_empty()
        || document.public_key.public_key_pem.is_empty()
    {
        return Err(invalid("remote actor document is invalid"));
    }
    let profile_created_at = remote_profile_created_at(&document)?;
    let followers_url = validated_followers_url(&document.id, document.followers.as_deref())?;
    let featured_url = validated_featured_url(&document.id, document.featured.as_deref())?;
    let featured_tags_url =
        validated_featured_url(&document.id, document.featured_tags.as_deref())?;
    let inbox =
        Url::parse(&document.inbox).map_err(|_| invalid("remote actor inbox URL is invalid"))?;
    if inbox.scheme() != "https"
        || inbox
            .host_str()
            .is_none_or(|host| !host.eq_ignore_ascii_case(&domain))
    {
        return Err(invalid("remote actor inbox is outside its actor domain"));
    }
    Ok((
        RemoteActor {
            id: AccountId(Uuid::now_v7()),
            activitypub_id: document.id,
            username: document.preferred_username,
            domain,
            display_name: document.name,
            summary: document.summary,
            emojis: JsonValue::Array(document.tag),
            inbox_url: document.inbox,
            shared_inbox_url: document.endpoints.shared_inbox,
            followers_url,
            featured_url,
            featured_tags_url,
            public_key_id: document.public_key.id,
            public_key_pem: document.public_key.public_key_pem,
            expires_at: OffsetDateTime::now_utc() + TimeDuration::hours(24),
            profile_created_at,
            first_seen_at: OffsetDateTime::now_utc(),
            deleted_at: None,
            moved_to_remote_actor_id: None,
            limited_at: None,
        },
        document.icon,
        document.image,
    ))
}

/// Resolve a Move target only when it reciprocally declares the source actor.
pub async fn resolve_remote_move_target(
    state: &AppState,
    target_id: &str,
    source_id: &str,
) -> Result<RemoteActor> {
    let actor_url = Url::parse(target_id).map_err(|_| invalid("remote Move target is invalid"))?;
    let domain = actor_url
        .host_str()
        .ok_or_else(|| invalid("remote Move target has no host"))?
        .to_ascii_lowercase();
    let document: RemoteActorDocument = fetch_json(state, actor_url.clone(), None).await?;
    if document.r#type != ActorType::Person
        || document.id != target_id
        || document.preferred_username.is_empty()
        || document.public_key.owner != document.id
        || document.public_key.id.is_empty()
        || document.public_key.public_key_pem.is_empty()
        || !document.also_known_as.iter().any(|id| id == source_id)
    {
        return Err(invalid("remote Move target is invalid"));
    }
    let inbox =
        Url::parse(&document.inbox).map_err(|_| invalid("remote actor inbox URL is invalid"))?;
    if inbox.scheme() != "https"
        || inbox
            .host_str()
            .is_none_or(|host| !host.eq_ignore_ascii_case(&domain))
    {
        return Err(invalid("remote actor inbox is outside its actor domain"));
    }
    let profile_created_at = remote_profile_created_at(&document)?;
    let followers_url = validated_followers_url(&document.id, document.followers.as_deref())?;
    let featured_url = validated_featured_url(&document.id, document.featured.as_deref())?;
    let featured_tags_url =
        validated_featured_url(&document.id, document.featured_tags.as_deref())?;
    store_remote_actor(
        state,
        RemoteActor {
            id: AccountId(Uuid::now_v7()),
            activitypub_id: document.id,
            username: document.preferred_username,
            domain,
            display_name: document.name,
            summary: document.summary,
            emojis: JsonValue::Array(document.tag),
            inbox_url: document.inbox,
            shared_inbox_url: document.endpoints.shared_inbox,
            followers_url,
            featured_url,
            featured_tags_url,
            public_key_id: document.public_key.id,
            public_key_pem: document.public_key.public_key_pem,
            expires_at: OffsetDateTime::now_utc() + TimeDuration::hours(24),
            profile_created_at,
            first_seen_at: OffsetDateTime::now_utc(),
            deleted_at: None,
            moved_to_remote_actor_id: None,
            limited_at: None,
        },
        document.icon,
        document.image,
        RemoteActorStoreMode::RefreshedDocument,
    )
    .await
}

/// Store an actor and reconcile its optional ActivityStreams profile images.
async fn store_remote_actor(
    state: &AppState,
    actor: RemoteActor,
    icon: Option<RemoteActorImage>,
    image: Option<RemoteActorImage>,
    mode: RemoteActorStoreMode,
) -> Result<RemoteActor> {
    let txn = state.db.begin().await?;
    let actor = store_remote_actor_on(&txn, actor, icon, image, mode).await?;
    txn.commit().await?;
    enqueue_profile_media_if_followed(state, actor.id).await?;
    Ok(actor)
}

async fn enqueue_profile_media_if_followed(state: &AppState, actor_id: AccountId) -> Result<()> {
    let read_txn = state
        .db
        .begin_with_config(None, Some(AccessMode::ReadOnly))
        .await?;
    let has_accepted_followers =
        !roosty_db::accepted_local_followers_of_remote_actor(&read_txn, actor_id)
            .await?
            .is_empty();
    read_txn.commit().await?;
    if has_accepted_followers {
        crate::media::enqueue_remote_profile_media_fetches(state, actor_id).await?;
    }
    Ok(())
}

async fn store_remote_actor_on(
    txn: &sea_orm::DatabaseTransaction,
    actor: RemoteActor,
    icon: Option<RemoteActorImage>,
    image: Option<RemoteActorImage>,
    mode: RemoteActorStoreMode,
) -> Result<RemoteActor> {
    let actor = match mode {
        RemoteActorStoreMode::DiscoveredHandle => {
            roosty_db::upsert_remote_actor(txn, &actor).await?
        }
        RemoteActorStoreMode::RefreshedDocument => {
            roosty_db::refresh_remote_actor(txn, &actor).await?
        }
    };
    let emojis = crate::accounts::remote_custom_emojis(&actor.emojis)
        .into_iter()
        .filter_map(|emoji| {
            Some(NewRemoteCustomEmoji {
                shortcode: emoji.get("shortcode")?.as_str()?.to_owned(),
                remote_url: emoji.get("url")?.as_str()?.to_owned(),
            })
        })
        .collect::<Vec<_>>();
    roosty_db::upsert_remote_custom_emojis(txn, &emojis).await?;
    roosty_db::replace_remote_profile_media(
        txn,
        actor.id,
        NewRemoteProfileMedia {
            avatar_url: icon.map(RemoteActorImage::into_url),
            header_url: image.map(RemoteActorImage::into_url),
        },
    )
    .await?;
    if actor.featured_url.is_some() {
        roosty_db::enqueue_job_in_transaction(
            txn,
            roosty_db::NewJob {
                kind: roosty_db::JobKind::FederationFeaturedRefresh,
                payload: serde_json::json!({ "remote_actor_id": actor.id.0 }),
                deduplication_key: Some(actor.id.0.to_string()),
                run_after: OffsetDateTime::now_utc(),
            },
        )
        .await?;
    } else {
        roosty_db::replace_remote_status_pins(txn, actor.id, &[]).await?;
    }
    if actor.featured_tags_url.is_some() {
        roosty_db::enqueue_job_in_transaction(
            txn,
            roosty_db::NewJob {
                kind: roosty_db::JobKind::FederationFeaturedTagsRefresh,
                payload: serde_json::json!({ "remote_actor_id": actor.id.0 }),
                deduplication_key: Some(actor.id.0.to_string()),
                run_after: OffsetDateTime::now_utc(),
            },
        )
        .await?;
    } else {
        roosty_db::replace_remote_featured_tags(txn, actor.id, &[]).await?;
    }
    Ok(actor)
}

/// Parse the optional ActivityStreams profile creation timestamp from an actor document.
fn remote_profile_created_at(document: &RemoteActorDocument) -> Result<Option<OffsetDateTime>> {
    document
        .published
        .as_deref()
        .map(|published| {
            OffsetDateTime::parse(published, &Rfc3339)
                .map_err(|_| invalid("remote actor published timestamp is invalid"))
        })
        .transpose()
}

/// Validate a declared followers collection without synthesizing one when absent.
fn validated_followers_url(actor_id: &str, followers: Option<&str>) -> Result<Option<String>> {
    let Some(followers) = followers else {
        return Ok(None);
    };
    let actor = Url::parse(actor_id).map_err(|_| invalid("remote actor ID is invalid"))?;
    let followers =
        Url::parse(followers).map_err(|_| invalid("remote actor followers URL is invalid"))?;
    if followers.scheme() != "https" || followers.origin() != actor.origin() {
        return Err(invalid(
            "remote actor followers URL is outside its actor origin",
        ));
    }
    Ok(Some(followers.into()))
}

/// Validate a featured collection as an unauthenticated same-origin HTTPS URL.
fn validated_featured_url(actor_id: &str, featured: Option<&str>) -> Result<Option<String>> {
    let Some(featured) = featured else {
        return Ok(None);
    };
    let actor = Url::parse(actor_id).map_err(|_| invalid("remote actor ID is invalid"))?;
    let featured =
        Url::parse(featured).map_err(|_| invalid("remote actor featured URL is invalid"))?;
    if featured.scheme() != "https"
        || !featured.username().is_empty()
        || featured.password().is_some()
        || featured.origin() != actor.origin()
    {
        return Err(invalid(
            "remote actor featured URL is outside its actor origin",
        ));
    }
    Ok(Some(featured.into()))
}

/// Fetch a JSON document with policy revalidation before every request.
pub(crate) async fn fetch_json<T: for<'de> Deserialize<'de>>(
    state: &AppState,
    mut url: Url,
    query: Option<(&str, &str)>,
) -> Result<T> {
    if let Some((value, name)) = query {
        url.query_pairs_mut().append_pair(name, value);
    }
    let address = validate_remote_url(state, &url).await?;
    let host = url
        .host_str()
        .ok_or_else(|| invalid("remote URL has no host"))?;
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .resolve(host, address)
        .build()
        .map_err(|error| invalid(&format!("could not create federation client: {error}")))?;
    let response = client.get(url).header(ACCEPT, "application/activity+json, application/ld+json, application/jrd+json, application/json").send().await.map_err(|error| invalid(&format!("remote request failed: {error}")))?;
    if response.status().is_redirection() {
        return Err(invalid("remote redirects are not accepted"));
    }
    if !response.status().is_success() {
        return Err(invalid(&format!(
            "remote server returned {}",
            response.status()
        )));
    }
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !is_json_content_type(content_type) {
        return Err(invalid("remote response is not JSON"));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_FEDERATION_RESPONSE_BYTES as u64)
    {
        return Err(invalid("remote response exceeds the size limit"));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|error| invalid(&format!("could not read remote response: {error}")))?;
    if bytes.len() > MAX_FEDERATION_RESPONSE_BYTES {
        return Err(invalid("remote response exceeds the size limit"));
    }
    serde_json::from_slice(&bytes).map_err(|_| invalid("remote response is invalid JSON"))
}

/// Enforce HTTPS, domain policy, and public DNS resolution before connecting.
pub(crate) async fn validate_remote_url(state: &AppState, url: &Url) -> Result<SocketAddr> {
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return Err(invalid("remote URL must be an unauthenticated HTTPS URL"));
    }
    let host = url
        .host_str()
        .ok_or_else(|| invalid("remote URL has no host"))?
        .to_ascii_lowercase();
    if !state.config.federation_domain_is_allowed(&host) {
        return Err(FederationDiscoveryError::PolicyRejected(host.into()).into());
    }
    if host.parse::<IpAddr>().is_ok() {
        return Err(invalid(
            "literal IP addresses are not permitted for federation",
        ));
    }
    let port = url
        .port_or_known_default()
        .ok_or_else(|| invalid("remote URL has no port"))?;
    let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), port))
        .await
        .map_err(|_| invalid("remote domain could not be resolved"))?
        .collect();
    if addresses.is_empty()
        || addresses
            .iter()
            .any(|address| is_unsafe_address(address.ip()))
    {
        return Err(invalid("remote domain resolves to a non-public address"));
    }
    addresses
        .into_iter()
        .next()
        .ok_or_else(|| invalid("remote domain could not be resolved"))
}

fn validate_actor_document(
    document: &RemoteActorDocument,
    requested_url: &Url,
    username: &str,
) -> Result<()> {
    if document.r#type != ActorType::Person
        || document.preferred_username.is_empty()
        || !document.preferred_username.eq_ignore_ascii_case(username)
    {
        return Err(invalid(
            "remote actor identity does not match WebFinger account",
        ));
    }
    let actor_id = Url::parse(&document.id).map_err(|_| invalid("remote actor ID is invalid"))?;
    if actor_id.scheme() != "https" || actor_id != *requested_url {
        return Err(invalid(
            "remote actor ID does not match the requested actor URL",
        ));
    }
    for target in [
        &document.inbox,
        document
            .endpoints
            .shared_inbox
            .as_deref()
            .unwrap_or(&document.inbox),
    ] {
        let url = Url::parse(target).map_err(|_| invalid("remote actor inbox URL is invalid"))?;
        if url.scheme() != "https" || url.origin() != actor_id.origin() {
            return Err(invalid("remote actor inbox is outside its actor origin"));
        }
    }
    if document.public_key.owner != document.id
        || document.public_key.id.is_empty()
        || document.public_key.public_key_pem.is_empty()
    {
        return Err(invalid("remote actor public key is invalid"));
    }
    Ok(())
}

fn parse_remote_handle(handle: &str) -> Result<(&str, &str)> {
    let handle = handle.strip_prefix('@').unwrap_or(handle);
    let (username, domain) = handle
        .rsplit_once('@')
        .ok_or_else(|| invalid("remote account must use username@domain"))?;
    if username.is_empty()
        || domain.is_empty()
        || username.contains('/')
        || domain.contains('/')
        || domain.contains(':')
    {
        return Err(invalid("remote account handle is invalid"));
    }
    Ok((username, domain))
}

fn is_json_content_type(content_type: &str) -> bool {
    content_type.split(';').next().is_some_and(|value| {
        matches!(
            value.trim(),
            "application/json"
                | "application/activity+json"
                | "application/ld+json"
                | "application/jrd+json"
        )
    })
}

fn is_activitypub_media_type(media_type: &str) -> bool {
    media_type.split(';').next().is_some_and(|value| {
        value
            .trim()
            .eq_ignore_ascii_case("application/activity+json")
            || value.trim().eq_ignore_ascii_case("application/ld+json")
    })
}

/// Select an ActivityPub actor URL while ignoring template-only WebFinger links.
fn activitypub_actor_href(webfinger: WebFingerResponse) -> Option<String> {
    webfinger.links.into_iter().find_map(|link| {
        (link.rel == "self" && link.r#type.as_deref().is_none_or(is_activitypub_media_type))
            .then_some(link.href)
            .flatten()
    })
}

fn is_unsafe_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            address.is_private()
                || address.is_loopback()
                || address.is_link_local()
                || address.is_unspecified()
                || address.is_multicast()
                || address.is_broadcast()
                || address.is_documentation()
                || (address.octets()[0] == 198
                    && (address.octets()[1] == 18 || address.octets()[1] == 19))
                || address.octets()[0] == 0
        }
        IpAddr::V6(address) => {
            address.is_loopback()
                || address.is_unspecified()
                || address.is_unique_local()
                || address.is_unicast_link_local()
                || address.is_multicast()
        }
    }
}

fn invalid(message: &str) -> RoostyError {
    RoostyError::InvalidInput(message.to_owned())
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use super::{
        RemoteActorDocument, WebFingerResponse, activitypub_actor_href, is_activitypub_media_type,
        is_json_content_type, is_unsafe_address, parse_remote_handle, remote_profile_created_at,
        validate_actor_document, validated_featured_url, validated_followers_url,
    };

    #[test]
    fn remote_handles_require_a_nonempty_dns_domain() {
        assert_eq!(
            parse_remote_handle("@alice@example.test").ok(),
            Some(("alice", "example.test"))
        );
        assert!(parse_remote_handle("alice").is_err());
        assert!(parse_remote_handle("alice@127.0.0.1").is_ok());
        assert!(parse_remote_handle("alice@example.test:443").is_err());
    }

    #[test]
    fn unsafe_addresses_are_never_connectable() {
        assert!(is_unsafe_address(IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(is_unsafe_address(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
        assert!(is_unsafe_address(IpAddr::V6(Ipv6Addr::LOCALHOST)));
        assert!(!is_unsafe_address(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))));
    }

    #[test]
    fn only_expected_json_content_types_are_accepted() {
        assert!(is_json_content_type(
            "application/activity+json; charset=utf-8"
        ));
        assert!(is_json_content_type("application/jrd+json"));
        assert!(!is_json_content_type("text/html"));
    }

    /// Given a Mastodon-style actor document, when deserialized, then its canonical
    /// `preferredUsername` becomes the remote actor's local username.
    #[test]
    fn deserializes_activitystreams_preferred_username() {
        let actor: RemoteActorDocument = serde_json::from_str(
            r#"{
                "id": "https://social.example/users/alice",
                "type": "Person",
                "preferredUsername": "alice",
                "inbox": "https://social.example/users/alice/inbox",
                "publicKey": {
                    "id": "https://social.example/users/alice#main-key",
                    "owner": "https://social.example/users/alice",
                    "publicKeyPem": "public-key"
                }
            }"#,
        )
        .unwrap();

        assert_eq!(actor.preferred_username, "alice");
    }

    /// Reads both allowed ActivityStreams forms for remote profile images.
    #[test]
    fn deserializes_remote_profile_image_references() {
        let actor: RemoteActorDocument = serde_json::from_str(
            r#"{
                "id": "https://social.example/users/alice",
                "type": "Person",
                "preferredUsername": "alice",
                "icon": {"type": "Image", "url": "https://social.example/avatar.png"},
                "image": "https://social.example/header.png",
                "inbox": "https://social.example/users/alice/inbox",
                "publicKey": {
                    "id": "https://social.example/users/alice#main-key",
                    "owner": "https://social.example/users/alice",
                    "publicKeyPem": "public-key"
                }
            }"#,
        )
        .unwrap();

        assert_eq!(
            actor.icon.map(super::RemoteActorImage::into_url).as_deref(),
            Some("https://social.example/avatar.png")
        );
        assert_eq!(
            actor
                .image
                .map(super::RemoteActorImage::into_url)
                .as_deref(),
            Some("https://social.example/header.png")
        );
    }

    #[test]
    /// Reads a Mastodon actor's optional profile creation timestamp.
    fn parses_remote_profile_creation_time() {
        let actor: RemoteActorDocument = serde_json::from_str(
            r#"{
                "id": "https://social.example/users/alice",
                "type": "Person",
                "preferredUsername": "alice",
                "inbox": "https://social.example/users/alice/inbox",
                "published": "2026-07-13T12:00:00Z",
                "publicKey": {
                    "id": "https://social.example/users/alice#main-key",
                    "owner": "https://social.example/users/alice",
                    "publicKeyPem": "public-key"
                }
            }"#,
        )
        .unwrap();

        assert_eq!(
            remote_profile_created_at(&actor).unwrap(),
            Some(
                time::OffsetDateTime::UNIX_EPOCH
                    + time::Duration::days(20_647)
                    + time::Duration::hours(12)
            )
        );
    }

    #[test]
    /// Rejects a supplied remote actor profile creation timestamp when it is malformed.
    fn rejects_invalid_remote_profile_creation_time() {
        let actor: RemoteActorDocument = serde_json::from_str(
            r#"{
                "id": "https://social.example/users/alice",
                "type": "Person",
                "preferredUsername": "alice",
                "inbox": "https://social.example/users/alice/inbox",
                "published": "not-a-timestamp",
                "publicKey": {
                    "id": "https://social.example/users/alice#main-key",
                    "owner": "https://social.example/users/alice",
                    "publicKeyPem": "public-key"
                }
            }"#,
        )
        .unwrap();

        assert!(remote_profile_created_at(&actor).is_err());
    }

    /// Given profiled JSON-LD WebFinger metadata, when selecting an actor link, then the
    /// ActivityStreams media type is accepted.
    #[test]
    fn accepts_profiled_activitystreams_media_type() {
        assert!(is_activitypub_media_type("application/activity+json"));
        assert!(is_activitypub_media_type(
            "application/ld+json; profile=\"https://www.w3.org/ns/activitystreams\""
        ));
        assert!(!is_activitypub_media_type("application/jrd+json"));
    }

    /// Given a JRD document containing template-only relations, when selecting its actor link,
    /// then those relations do not make the otherwise valid WebFinger response fail to parse.
    #[test]
    fn ignores_webfinger_links_without_href() {
        let webfinger: WebFingerResponse = serde_json::from_str(
            r#"{
                "subject": "acct:ctron@dentrassi.de",
                "links": [
                    {
                        "rel": "http://ostatus.org/schema/1.0/subscribe",
                        "template": "https://mastodon.dentrassi.de/authorize_interaction?uri={uri}"
                    },
                    {
                        "rel": "self",
                        "type": "application/activity+json",
                        "href": "https://mastodon.dentrassi.de/users/ctron"
                    }
                ]
            }"#,
        )
        .unwrap();

        assert_eq!(
            activitypub_actor_href(webfinger).as_deref(),
            Some("https://mastodon.dentrassi.de/users/ctron")
        );
    }

    /// Given a WebFinger handle delegated to an actor host, when the actor and inbox share an
    /// origin, then discovery accepts the delegation while retaining the WebFinger domain.
    #[test]
    fn accepts_delegated_webfinger_actor_origin() {
        let actor: RemoteActorDocument = serde_json::from_str(
            r#"{
                "id": "https://mastodon.dentrassi.de/users/ctron",
                "type": "Person",
                "preferredUsername": "ctron",
                "inbox": "https://mastodon.dentrassi.de/users/ctron/inbox",
                "endpoints": {"sharedInbox": "https://mastodon.dentrassi.de/inbox"},
                "publicKey": {
                    "id": "https://mastodon.dentrassi.de/users/ctron#main-key",
                    "owner": "https://mastodon.dentrassi.de/users/ctron",
                    "publicKeyPem": "public-key"
                }
            }"#,
        )
        .unwrap();
        let actor_url = url::Url::parse("https://mastodon.dentrassi.de/users/ctron").unwrap();

        assert!(validate_actor_document(&actor, &actor_url, "ctron").is_ok());
    }

    /// Given a delegated actor, when its inbox leaves the actor origin, then discovery rejects it.
    #[test]
    fn rejects_delegated_actor_with_cross_origin_inbox() {
        let actor: RemoteActorDocument = serde_json::from_str(
            r#"{
                "id": "https://mastodon.dentrassi.de/users/ctron",
                "type": "Person",
                "preferredUsername": "ctron",
                "inbox": "https://attacker.example/inbox",
                "publicKey": {
                    "id": "https://mastodon.dentrassi.de/users/ctron#main-key",
                    "owner": "https://mastodon.dentrassi.de/users/ctron",
                    "publicKeyPem": "public-key"
                }
            }"#,
        )
        .unwrap();
        let actor_url = url::Url::parse("https://mastodon.dentrassi.de/users/ctron").unwrap();

        assert!(validate_actor_document(&actor, &actor_url, "ctron").is_err());
    }

    /// Only an explicitly declared same-origin HTTPS followers collection is cached.
    #[test]
    fn validates_declared_followers_collection() {
        let actor = "https://social.example/users/alice";
        assert_eq!(validated_followers_url(actor, None).unwrap(), None);
        assert_eq!(
            validated_followers_url(actor, Some("https://social.example/users/alice/followers"))
                .unwrap()
                .as_deref(),
            Some("https://social.example/users/alice/followers")
        );
        assert!(
            validated_followers_url(actor, Some("https://attacker.example/followers")).is_err()
        );
        assert!(validated_followers_url(actor, Some("http://social.example/followers")).is_err());
    }

    /// Featured collection URLs must be credential-free HTTPS URLs at the actor origin.
    #[test]
    fn validates_declared_featured_collection() {
        let actor = "https://social.example/users/alice";
        assert_eq!(validated_featured_url(actor, None).unwrap(), None);
        assert_eq!(
            validated_featured_url(
                actor,
                Some("https://social.example/users/alice/collections/featured")
            )
            .unwrap()
            .as_deref(),
            Some("https://social.example/users/alice/collections/featured")
        );
        assert!(validated_featured_url(actor, Some("https://attacker.example/featured")).is_err());
        assert!(
            validated_featured_url(actor, Some("https://alice:secret@social.example/featured"))
                .is_err()
        );
    }
}
