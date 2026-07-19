use axum::{Json, Router, extract::State, routing::get};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{config::Config, http::AppState, media};

const NODEINFO_REL_2_1: &str = "http://nodeinfo.diaspora.software/ns/schema/2.1";
const IMAGE_MATRIX_LIMIT: u64 = 4096 * 4096;

/// Build public instance discovery and metadata routes.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/.well-known/nodeinfo", get(nodeinfo_discovery))
        .route("/nodeinfo/2.0", get(nodeinfo))
        .route("/nodeinfo/2.1", get(nodeinfo))
        .route("/api/v2/instance", get(instance_v2))
        .route("/api/v1/instance", get(instance_v1))
}

async fn nodeinfo_discovery(State(state): State<AppState>) -> Json<NodeInfoDiscovery> {
    Json(nodeinfo_discovery_response(&state.config))
}

async fn nodeinfo(State(state): State<AppState>) -> Json<Value> {
    Json(nodeinfo_response(&state.config))
}

async fn instance_v2(State(state): State<AppState>) -> Json<Value> {
    Json(instance_v2_response(&state.config))
}

async fn instance_v1(State(state): State<AppState>) -> Json<Value> {
    Json(instance_v1_response(&state.config))
}

#[derive(Serialize)]
struct NodeInfoDiscovery {
    links: Vec<NodeInfoLink>,
}

#[derive(Serialize)]
struct NodeInfoLink {
    rel: &'static str,
    href: String,
}

/// Build the NodeInfo discovery document for the configured public base URL.
fn nodeinfo_discovery_response(config: &Config) -> NodeInfoDiscovery {
    NodeInfoDiscovery {
        links: vec![NodeInfoLink {
            rel: NODEINFO_REL_2_1,
            href: public_url(config, "/nodeinfo/2.1"),
        }],
    }
}

/// Build a minimal NodeInfo document for clients probing federation metadata.
fn nodeinfo_response(config: &Config) -> Value {
    json!({
        "version": "2.1",
        "software": {
            "name": "roosty",
            "version": env!("CARGO_PKG_VERSION"),
            "repository": env!("CARGO_PKG_REPOSITORY"),
            "homepage": env!("CARGO_PKG_REPOSITORY"),
        },
        "protocols": ["activitypub"],
        "services": {
            "inbound": [],
            "outbound": [],
        },
        "openRegistrations": registrations_enabled(config),
        "usage": {
            "users": {
                "total": 0,
                "activeHalfyear": 0,
                "activeMonth": 0,
            },
            "localPosts": 0,
            "localComments": 0,
        },
        "metadata": {
            "nodeName": config.instance_name,
            "nodeDescription": config.instance_description.as_deref().unwrap_or_default(),
        },
    })
}

/// Build the Mastodon v2 instance response from static configuration.
fn instance_v2_response(config: &Config) -> Value {
    json!({
        "domain": domain(config),
        "title": config.instance_name,
        "version": roosty_version(),
        "source_url": env!("CARGO_PKG_REPOSITORY"),
        "description": config.instance_description.as_deref().unwrap_or_default(),
        "usage": {
            "users": {
                "active_month": 0,
            },
        },
        "thumbnail": {
            "url": null,
            "description": null,
            "blurhash": null,
            "versions": {},
        },
        "icon": [],
        "languages": ["en"],
        "configuration": configuration(config),
        "registrations": {
            "enabled": registrations_enabled(config),
            "approval_required": registrations_approval_required(config),
            "reason_required": false,
            "message": null,
            "min_age": null,
            "url": null,
        },
        "api_versions": {
            "mastodon": 6,
        },
        "contact": {
            "email": "",
            "account": null,
        },
        "rules": [],
    })
}

/// Build the legacy Mastodon v1 instance response from static configuration.
fn instance_v1_response(config: &Config) -> Value {
    json!({
        "uri": domain(config),
        "title": config.instance_name,
        "short_description": config.instance_description.as_deref().unwrap_or_default(),
        "description": config.instance_description.as_deref().unwrap_or_default(),
        "email": "",
        "version": roosty_version(),
        "urls": {
            "streaming_api": streaming_url(config),
        },
        "stats": {
            "user_count": 0,
            "status_count": 0,
            "domain_count": 0,
        },
        "thumbnail": null,
        "languages": ["en"],
        "registrations": registrations_enabled(config),
        "approval_required": registrations_approval_required(config),
        "invites_enabled": false,
        "configuration": configuration(config),
        "contact_account": null,
        "rules": [],
    })
}

/// Build shared Mastodon instance capability and limit metadata.
fn configuration(config: &Config) -> Value {
    json!({
        "urls": {
            "streaming": streaming_url(config),
            "status": null,
            "about": null,
            "privacy_policy": null,
            "terms_of_service": null,
        },
        "vapid": {
            "public_key": vapid_public_key(config),
        },
        "accounts": {
            "max_featured_tags": 10,
            "max_pinned_statuses": crate::statuses::MAX_PINNED_STATUSES,
        },
        "statuses": {
            "max_characters": 500,
            "max_media_attachments": 4,
            "characters_reserved_per_url": 23,
        },
        "media_attachments": {
            "supported_mime_types": media::supported_image_mime_types(),
            "image_size_limit": 10485760,
            "image_matrix_limit": IMAGE_MATRIX_LIMIT,
            "video_size_limit": 0,
            "video_frame_rate_limit": 0,
            "video_matrix_limit": 0,
            "description_limit": 1500,
        },
        "polls": {
            "max_options": 4,
            "max_characters_per_option": 50,
            "min_expiration": 300,
            "max_expiration": 2629746,
        },
        "translation": {
            "enabled": false,
        },
        "limited_federation": !config.federation_enabled,
    })
}

fn vapid_public_key(config: &Config) -> String {
    let Some(key) = config.vapid_private_key.as_deref() else {
        return String::new();
    };
    let subject = config.public_base_url.origin().ascii_serialization();
    roosty_web_push::VapidIdentity::from_base64_pkcs8(key, subject)
        .map_or_else(|_| String::new(), |identity| identity.public_key_base64())
}

/// Return whether the configured registration mode allows user signups.
fn registrations_enabled(config: &Config) -> bool {
    matches!(config.registration_mode.as_str(), "open" | "approval")
}

fn registrations_approval_required(config: &Config) -> bool {
    config.registration_mode == "approval"
}

/// Return the Roosty release version without build-specific metadata.
fn roosty_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Extract the instance domain from the configured public base URL.
fn domain(config: &Config) -> String {
    config
        .public_base_url
        .host_str()
        .unwrap_or("localhost")
        .to_owned()
}

/// Resolve an absolute public URL for an instance route path.
fn public_url(config: &Config, path: &str) -> String {
    config
        .public_base_url
        .join(path.trim_start_matches('/'))
        .map(|url| url.to_string())
        .unwrap_or_else(|_| format!("{}{}", config.public_base_url, path))
}

/// Build the public WebSocket URL advertised to Mastodon clients.
fn streaming_url(config: &Config) -> String {
    let mut url = config.public_base_url.clone();
    let scheme = match url.scheme() {
        "https" => "wss",
        "http" => "ws",
        _ => url.scheme(),
    }
    .to_owned();
    let _ = url.set_scheme(&scheme);
    url.join("api/v1/streaming")
        .map(|url| url.to_string())
        .unwrap_or_else(|_| public_url(config, "/api/v1/streaming"))
}

#[cfg(test)]
mod tests {
    use std::{
        net::{IpAddr, Ipv4Addr, SocketAddr},
        sync::Arc,
    };

    use super::*;

    #[test]
    fn nodeinfo_discovery_points_to_public_nodeinfo_url() {
        let config = test_config("closed");

        let discovery = nodeinfo_discovery_response(&config);

        assert_eq!(discovery.links.len(), 1);
        assert_eq!(discovery.links[0].rel, NODEINFO_REL_2_1);
        assert_eq!(
            discovery.links[0].href,
            "https://roosty.localhost:4000/nodeinfo/2.1"
        );
    }

    #[test]
    fn instance_v2_uses_configured_instance_metadata() {
        let config = test_config("closed");

        let body = instance_v2_response(&config);

        assert_eq!(body["domain"], "roosty.localhost");
        assert_eq!(body["title"], "Roosty Test");
        assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(body["description"], "Endpoint test instance");
        assert_eq!(body["registrations"]["enabled"], false);
        assert_eq!(body["registrations"]["approval_required"], false);
        assert_eq!(
            body["configuration"]["urls"]["streaming"],
            "wss://roosty.localhost:4000/api/v1/streaming"
        );
        assert_eq!(
            body["configuration"]["media_attachments"]["image_matrix_limit"],
            IMAGE_MATRIX_LIMIT
        );
        assert_eq!(body["configuration"]["accounts"]["max_pinned_statuses"], 5);
    }

    #[test]
    fn instance_v1_maps_legacy_field_names() {
        let config = test_config("approval");

        let body = instance_v1_response(&config);

        assert_eq!(body["uri"], "roosty.localhost");
        assert_eq!(body["short_description"], "Endpoint test instance");
        assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(body["registrations"], true);
        assert_eq!(body["approval_required"], true);
        assert_eq!(body["stats"]["user_count"], 0);
    }

    #[test]
    fn nodeinfo_reports_roosty_and_registration_status() {
        let config = test_config("open");

        let body = nodeinfo_response(&config);

        assert_eq!(body["software"]["name"], "roosty");
        assert_eq!(body["protocols"][0], "activitypub");
        assert_eq!(body["openRegistrations"], true);
        assert_eq!(body["metadata"]["nodeName"], "Roosty Test");
    }

    fn test_config(registration_mode: &str) -> Arc<Config> {
        Arc::new(Config {
            database_url: "postgres://roosty:roosty@localhost/roosty".to_owned(),
            public_base_url: "https://roosty.localhost:4000".parse().unwrap(),
            listen_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4000),
            infra_listen_addr: None,
            session_secret: "test-session-secret-change-me-000".to_owned(),
            token_pepper: "test-token-pepper-change-me-0000".to_owned(),
            vapid_private_key: None,
            object_storage_backend: "local".to_owned(),
            media_root: "./media".to_owned(),
            registration_mode: registration_mode.to_owned(),
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
        })
    }
}
