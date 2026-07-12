//! Safe WebFinger and ActivityPub actor discovery for remote accounts.

use std::{
    net::{IpAddr, SocketAddr},
    time::Duration,
};

use reqwest::{
    Client,
    header::{ACCEPT, CONTENT_TYPE},
};
use roost_core::{Result, RoostError};
use serde::Deserialize;
use time::{Duration as TimeDuration, OffsetDateTime};
use url::Url;
use uuid::Uuid;

use crate::http::AppState;

const MAX_FEDERATION_RESPONSE_BYTES: usize = 1_048_576;

#[derive(Deserialize)]
struct WebFingerResponse {
    subject: String,
    links: Vec<WebFingerLink>,
}

#[derive(Deserialize)]
struct WebFingerLink {
    rel: String,
    #[serde(rename = "type")]
    media_type: Option<String>,
    href: String,
}

#[derive(Deserialize)]
struct RemoteActorDocument {
    id: String,
    #[serde(rename = "type")]
    actor_type: String,
    #[serde(default)]
    preferred_username: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    summary: String,
    inbox: String,
    #[serde(default)]
    endpoints: RemoteEndpoints,
    #[serde(rename = "publicKey")]
    public_key: RemotePublicKey,
}

#[derive(Default, Deserialize)]
struct RemoteEndpoints {
    #[serde(rename = "sharedInbox")]
    shared_inbox: Option<String>,
}

#[derive(Deserialize)]
struct RemotePublicKey {
    id: String,
    owner: String,
    #[serde(rename = "publicKeyPem")]
    public_key_pem: String,
}

/// Resolve and cache a remote actor after applying the configured network policy.
pub async fn resolve_remote_actor(state: &AppState, handle: &str) -> Result<roost_db::RemoteActor> {
    let (username, domain) = parse_remote_handle(handle)?;
    let domain = domain.to_ascii_lowercase();
    if let Some(actor) = roost_db::find_remote_actor_by_handle(&state.db, username, &domain).await?
        && actor.expires_at > OffsetDateTime::now_utc()
    {
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
    let actor_url = webfinger
        .links
        .into_iter()
        .find(|link| {
            link.rel == "self"
                && link.media_type.as_deref().is_none_or(|media_type| {
                    media_type == "application/activity+json" || media_type == "application/ld+json"
                })
        })
        .map(|link| link.href)
        .ok_or_else(|| invalid("WebFinger response does not include an ActivityPub actor link"))?;
    let actor_url =
        Url::parse(&actor_url).map_err(|_| invalid("WebFinger actor URL is invalid"))?;
    let document: RemoteActorDocument = fetch_json(state, actor_url.clone(), None).await?;
    validate_actor_document(&document, &actor_url, username, &domain)?;
    let actor = roost_db::RemoteActor {
        id: roost_core::AccountId(Uuid::now_v7()),
        activitypub_id: document.id,
        username: document.preferred_username,
        domain,
        display_name: document.name,
        summary: document.summary,
        inbox_url: document.inbox,
        shared_inbox_url: document.endpoints.shared_inbox,
        public_key_id: document.public_key.id,
        public_key_pem: document.public_key.public_key_pem,
        expires_at: OffsetDateTime::now_utc() + TimeDuration::hours(24),
    };
    roost_db::upsert_remote_actor(&state.db, &actor).await
}

/// Fetch a JSON document with policy revalidation before every request.
async fn fetch_json<T: for<'de> Deserialize<'de>>(
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

/// Enforce HTTPS, exact domain policy, and public DNS resolution before connecting.
async fn validate_remote_url(state: &AppState, url: &Url) -> Result<SocketAddr> {
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return Err(invalid("remote URL must be an unauthenticated HTTPS URL"));
    }
    let host = url
        .host_str()
        .ok_or_else(|| invalid("remote URL has no host"))?
        .to_ascii_lowercase();
    if !state
        .config
        .federation_allowed_domains
        .iter()
        .any(|domain| domain == &host)
        || state
            .config
            .federation_blocked_domains
            .iter()
            .any(|domain| domain == &host)
    {
        return Err(invalid("remote domain is disallowed by federation policy"));
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
    domain: &str,
) -> Result<()> {
    if document.actor_type != "Person"
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
        if url.scheme() != "https"
            || url
                .host_str()
                .is_none_or(|host| !host.eq_ignore_ascii_case(domain))
        {
            return Err(invalid(
                "remote actor inbox is outside its WebFinger domain",
            ));
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

fn invalid(message: &str) -> RoostError {
    RoostError::InvalidInput(message.to_owned())
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use super::{is_json_content_type, is_unsafe_address, parse_remote_handle};

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
}
