use std::collections::HashMap;

use axum::{
    Json, Router,
    extract::{
        Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Serialize;
use serde_json::Value;
use tracing::{debug, warn};

use roosty_core::AccountId;

use crate::{
    auth::{self, AuthenticatedAccount},
    http::AppState,
};

/// Build compatibility routes probed by Mastodon browser clients.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/push/subscription", get(push_subscription))
        .route("/api/v1/followed_tags", get(followed_tags))
        .route("/api/v1/streaming", get(streaming))
        .route("/api/v1/streaming/direct", get(streaming_direct))
        .route("/api/v1/streaming/health", get(streaming_health))
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

async fn push_subscription(AuthenticatedAccount(_account): AuthenticatedAccount) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorResponse {
            error: "Record not found".to_owned(),
        }),
    )
        .into_response()
}

async fn followed_tags(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
) -> Response {
    match roosty_db::followed_local_tags(&state.db, account.id).await {
        Ok(tags) => {
            let mut response = Vec::with_capacity(tags.len());
            for tag in tags {
                match crate::statuses::tag_response_model(&state, tag, Some(true)).await {
                    Ok(tag) => response.push(tag),
                    Err(error) => return server_error(error),
                }
            }
            Json(response).into_response()
        }
        Err(error) => server_error(error),
    }
}

async fn streaming_health() -> &'static str {
    "OK"
}

async fn streaming_direct(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    websocket: WebSocketUpgrade,
) -> Response {
    streaming_response(state, headers, query, websocket, Some("direct".to_owned())).await
}

async fn streaming(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    websocket: WebSocketUpgrade,
) -> Response {
    let stream = query.get("stream").cloned();
    streaming_response(state, headers, query, websocket, stream).await
}

async fn streaming_response(
    state: AppState,
    headers: HeaderMap,
    query: HashMap<String, String>,
    websocket: WebSocketUpgrade,
    stream: Option<String>,
) -> Response {
    let Some(token) = streaming_token(&headers, &query) else {
        return unauthorized().into_response();
    };
    let account = match auth::account_from_bearer_token(&state, &token).await {
        Ok(account) => account,
        Err(response) => return response,
    };

    let events = state.streaming_events.clone();
    websocket
        .on_upgrade(move |socket| handle_streaming_socket(socket, account.id, stream, events))
        .into_response()
}

/// Keep a validated streaming socket open until the client closes it.
async fn handle_streaming_socket(
    mut socket: WebSocket,
    account_id: AccountId,
    initial_stream: Option<String>,
    events: crate::streaming::StreamingEvents,
) {
    let mut streams = initial_stream.map_or_else(|| vec!["user".to_owned()], |stream| vec![stream]);
    debug!(?streams, "streaming client subscribed");

    let mut receiver = events.subscribe();
    loop {
        tokio::select! {
            message = socket.recv() => {
                let Some(message) = message else {
                    break;
                };
                match message {
                    Ok(Message::Text(text)) => handle_streaming_text(&text, &mut streams),
                    Ok(Message::Close(_)) => break,
                    Ok(Message::Ping(payload)) => {
                        if socket.send(Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(error) => {
                        warn!(%error, "streaming websocket error");
                        break;
                    }
                }
            }
            event = receiver.recv() => {
                match event {
                    Ok(event) => {
                        let message = match event.to_socket_message(account_id, &streams) {
                            Ok(Some(message)) => message,
                            Ok(None) => continue,
                            Err(error) => {
                                warn!(%error, "failed to serialize streaming socket message");
                                continue;
                            }
                        };
                        if socket.send(Message::Text(message.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!(skipped, "streaming websocket receiver lagged");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        break;
                    }
                }
            }
        }
    }
}

/// Log subscribe and unsubscribe messages until real event fan-out exists.
fn handle_streaming_text(text: &str, streams: &mut Vec<String>) {
    match serde_json::from_str::<Value>(text) {
        Ok(value) => {
            let message_type = value.get("type").and_then(Value::as_str);
            let stream = value.get("stream").and_then(Value::as_str);
            update_stream_subscription(message_type, stream, streams);
            debug!(?message_type, ?stream, "streaming control message");
        }
        Err(error) => debug!(%error, "ignored non-json streaming message"),
    }
}

/// Apply browser WebSocket subscribe and unsubscribe control messages.
fn update_stream_subscription(
    message_type: Option<&str>,
    stream: Option<&str>,
    streams: &mut Vec<String>,
) {
    let Some(stream) = stream.map(str::trim).filter(|stream| !stream.is_empty()) else {
        return;
    };
    match message_type {
        Some("subscribe") if !streams.iter().any(|current| current == stream) => {
            streams.push(stream.to_owned());
        }
        Some("unsubscribe") => {
            streams.retain(|current| current != stream);
        }
        _ => {}
    }
    if streams.is_empty() {
        streams.push("user".to_owned());
    }
}

fn streaming_token(headers: &HeaderMap, query: &HashMap<String, String>) -> Option<String> {
    query
        .get("access_token")
        .cloned()
        .or_else(|| bearer_token(headers).map(str::to_owned))
        .or_else(|| websocket_protocol_token(headers))
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
}

fn websocket_protocol_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value
                .split(',')
                .map(str::trim)
                .find(|part| !part.is_empty() && *part != "Bearer")
                .map(str::to_owned)
        })
}

fn unauthorized() -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::UNAUTHORIZED,
        Json(ErrorResponse {
            error: "The access token is invalid".to_owned(),
        }),
    )
}

fn server_error(error: roosty_core::RoostyError) -> Response {
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
        http::{HeaderMap, Request, StatusCode, header::AUTHORIZATION},
    };
    use postgresql_embedded::PostgreSQL;
    use roosty_core::AccountId;
    use roosty_migration::Migrator;
    use sea_orm_migration::MigratorTrait;
    use serde_json::Value;
    use tempfile::TempDir;
    use test_context::{AsyncTestContext, test_context};
    use tower::ServiceExt;

    use super::{streaming_token, update_stream_subscription};
    use crate::{config::Config, http::AppState, password};

    #[test_context(CompatContext)]
    #[tokio::test]
    async fn empty_startup_collections_require_valid_tokens(context: &mut CompatContext) {
        // These Elk startup probes are account-specific and must keep the same
        // token requirements even while they return empty compatibility data.
        let token = context.access_token().await;

        for uri in [
            "/api/v1/followed_tags",
            "/api/v1/markers?timeline[]=notifications",
            "/api/v1/notifications?limit=30",
            "/api/v1/timelines/home?limit=30",
        ] {
            let response = context.authenticated_get(uri, &token).await;
            assert_eq!(response.status(), StatusCode::OK);
            let body = json_body(response).await;
            if uri.starts_with("/api/v1/markers") {
                assert_eq!(body, serde_json::json!({}));
            } else {
                assert_eq!(body, serde_json::json!([]));
            }

            let unauthorized = context.get(uri).await;
            assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
        }
    }

    #[test_context(CompatContext)]
    #[tokio::test]
    async fn markers_persist_requested_home_and_notification_positions(
        context: &mut CompatContext,
    ) {
        // Given an authenticated account, when it saves both timeline positions,
        // then Mastodon clients can retrieve the same complete marker objects.
        let token = context.access_token().await;
        let home_id = uuid::Uuid::now_v7();
        let notification_id = uuid::Uuid::now_v7();
        let body = format!(
            "home%5Blast_read_id%5D={home_id}&notifications%5Blast_read_id%5D={notification_id}"
        );

        let created = json_body(
            context
                .authenticated_form_post("/api/v1/markers", &token, body)
                .await,
        )
        .await;

        assert_eq!(
            created,
            serde_json::json!({
                "home": {
                    "last_read_id": home_id.to_string(),
                    "version": 1,
                    "updated_at": created["home"]["updated_at"].clone(),
                },
                "notifications": {
                    "last_read_id": notification_id.to_string(),
                    "version": 1,
                    "updated_at": created["notifications"]["updated_at"].clone(),
                },
            })
        );

        let fetched = json_body(
            context
                .authenticated_get(
                    "/api/v1/markers?timeline[]=notifications&timeline[]=home",
                    &token,
                )
                .await,
        )
        .await;
        assert_eq!(fetched, created);

        let next_home_id = uuid::Uuid::now_v7();
        let updated = json_body(
            context
                .authenticated_form_post(
                    "/api/v1/markers",
                    &token,
                    format!("home%5Blast_read_id%5D={next_home_id}"),
                )
                .await,
        )
        .await;
        assert_eq!(
            updated,
            serde_json::json!({
                "home": {
                    "last_read_id": next_home_id.to_string(),
                    "version": 2,
                    "updated_at": updated["home"]["updated_at"].clone(),
                },
            })
        );
    }

    #[test_context(CompatContext)]
    #[tokio::test]
    async fn public_timeline_returns_an_empty_collection(context: &mut CompatContext) {
        let response = context
            .get("/api/v1/timelines/public?limit=30&local=true")
            .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(json_body(response).await, serde_json::json!([]));
    }

    #[test_context(CompatContext)]
    #[tokio::test]
    async fn push_subscription_reports_missing_subscription(context: &mut CompatContext) {
        // Push delivery is not implemented yet, but authenticated clients expect
        // the probe to distinguish "no subscription" from "unknown endpoint".
        let token = context.access_token().await;

        let response = context
            .authenticated_get("/api/v1/push/subscription", &token)
            .await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(json_body(response).await["error"], "Record not found");
    }

    #[test_context(CompatContext)]
    #[tokio::test]
    async fn streaming_health_works(context: &mut CompatContext) {
        let health = context.get("/api/v1/streaming/health").await;
        assert_eq!(health.status(), StatusCode::OK);
        let body = to_bytes(health.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], b"OK");
    }

    #[test]
    fn streaming_token_accepts_browser_compatible_locations() {
        // Browser WebSocket clients cannot set arbitrary Authorization headers,
        // so Mastodon-compatible clients may use query or protocol locations.
        let mut headers = HeaderMap::new();
        let mut query = std::collections::HashMap::new();
        query.insert("access_token".to_owned(), "query-token".to_owned());
        assert_eq!(
            streaming_token(&headers, &query),
            Some("query-token".to_owned())
        );

        query.clear();
        headers.insert(AUTHORIZATION, "Bearer header-token".parse().unwrap());
        assert_eq!(
            streaming_token(&headers, &query),
            Some("header-token".to_owned())
        );

        headers.clear();
        headers.insert(
            axum::http::header::SEC_WEBSOCKET_PROTOCOL,
            "Bearer, protocol-token".parse().unwrap(),
        );
        assert_eq!(
            streaming_token(&headers, &query),
            Some("protocol-token".to_owned())
        );
    }

    #[test]
    fn streaming_subscription_updates_keep_a_default_stream() {
        // Some browser clients open the socket first and subscribe through
        // control messages; outgoing events still need a defined stream array.
        let mut streams = vec!["user".to_owned()];

        update_stream_subscription(Some("subscribe"), Some("public"), &mut streams);
        assert_eq!(streams, vec!["user".to_owned(), "public".to_owned()]);

        update_stream_subscription(Some("unsubscribe"), Some("user"), &mut streams);
        assert_eq!(streams, vec!["public".to_owned()]);

        update_stream_subscription(Some("unsubscribe"), Some("public"), &mut streams);
        assert_eq!(streams, vec!["user".to_owned()]);
    }

    struct CompatContext {
        postgresql: PostgreSQL,
        db: roosty_db::DbConnection,
        database_name: String,
        config: Config,
        account_id: AccountId,
        application_id: uuid::Uuid,
        _temp_dir: TempDir,
    }

    impl AsyncTestContext for CompatContext {
        async fn setup() -> Self {
            let temp_dir = tempfile::Builder::new()
                .prefix("roosty-compat-")
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
                instance_name: "Roosty Test".to_owned(),
                instance_description: Some("Endpoint test instance".to_owned()),
            };

            Self {
                postgresql,
                db,
                database_name,
                config,
                account_id,
                application_id: application.id,
                _temp_dir: temp_dir,
            }
        }

        async fn teardown(self) {
            self.db.close().await.unwrap();
            self.postgresql
                .drop_database(&self.database_name)
                .await
                .unwrap();
            self.postgresql.stop().await.unwrap();
        }
    }

    impl CompatContext {
        fn app(&self) -> Router {
            crate::http::app_router(AppState::new(self.config.clone(), self.db.clone()), false)
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
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
        }

        /// Submit a URL-encoded form as an authenticated Mastodon API client.
        async fn authenticated_form_post(
            &self,
            uri: &str,
            token: &str,
            body: String,
        ) -> axum::http::Response<Body> {
            self.request(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .header(
                        axum::http::header::CONTENT_TYPE,
                        "application/x-www-form-urlencoded",
                    )
                    .body(Body::from(body))
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

        format!("roosty_compat_{}_{}", std::process::id(), timestamp)
    }
}
