use std::{collections::HashMap, sync::Arc};

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
use tokio::{sync::OwnedSemaphorePermit, time::Instant};
use tracing::{debug, warn};

use roosty_core::AccountId;

use crate::{
    auth::{self, AuthenticatedAccount},
    config::StreamingConfig,
    http::AppState,
    streaming::{StreamingEvents, StreamingMetrics},
};

/// Build compatibility routes probed by Mastodon browser clients.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/custom_emojis", get(custom_emojis))
        .route("/api/v1/followed_tags", get(followed_tags))
        .route("/api/v1/streaming", get(streaming))
        .route("/api/v1/streaming/direct", get(streaming_direct))
        .route("/api/v1/streaming/health", get(streaming_health))
}

/// Roosty exposes the standard picker API but does not host local custom emoji.
async fn custom_emojis() -> Json<Vec<Value>> {
    Json(Vec::new())
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
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

    let permit = match state.streaming_connections.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            state.streaming_events.metrics().connection_rejected();
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ErrorResponse {
                    error: "Streaming connection limit reached".to_owned(),
                }),
            )
                .into_response();
        }
    };

    let events = state.streaming_events.clone();
    let config = state.config.streaming.clone();
    websocket
        .on_upgrade(move |socket| {
            handle_streaming_socket(socket, account.id, stream, events, config, permit)
        })
        .into_response()
}

/// Keep a validated streaming socket open until the client closes it.
async fn handle_streaming_socket(
    mut socket: WebSocket,
    account_id: AccountId,
    initial_stream: Option<String>,
    events: StreamingEvents,
    config: StreamingConfig,
    _permit: OwnedSemaphorePermit,
) {
    let metrics = events.metrics();
    let _connection = ActiveConnection::new(metrics.clone());
    let mut streams = initial_stream.map_or_else(|| vec!["user".to_owned()], |stream| vec![stream]);
    debug!(?streams, "streaming client subscribed");

    let mut receiver = events.subscribe();
    let mut ping_interval = tokio::time::interval(config.ping_interval);
    ping_interval.tick().await;
    let idle_timer = tokio::time::sleep(config.idle_timeout);
    tokio::pin!(idle_timer);
    loop {
        tokio::select! {
            message = socket.recv() => {
                let Some(message) = message else {
                    break;
                };
                idle_timer.as_mut().reset(Instant::now() + config.idle_timeout);
                match message {
                    Ok(Message::Text(text)) => handle_streaming_text(&text, &mut streams),
                    Ok(Message::Close(_)) => break,
                    Ok(Message::Ping(payload)) => {
                        if !send_socket_message(
                            &mut socket,
                            Message::Pong(payload),
                            config.send_timeout,
                            &metrics,
                        ).await {
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
                        if !send_socket_message(
                            &mut socket,
                            Message::Text(message.into()),
                            config.send_timeout,
                            &metrics,
                        ).await {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        metrics.receiver_lagged();
                        warn!(skipped, "streaming websocket receiver lagged");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        break;
                    }
                }
            }
            _ = ping_interval.tick() => {
                if !send_socket_message(
                    &mut socket,
                    Message::Ping(Vec::new().into()),
                    config.send_timeout,
                    &metrics,
                ).await {
                    break;
                }
            }
            () = idle_timer.as_mut() => {
                metrics.idle_disconnected();
                let _ = send_socket_message(
                    &mut socket,
                    Message::Close(None),
                    config.send_timeout,
                    &metrics,
                ).await;
                break;
            }
        }
    }
}

struct ActiveConnection(Arc<StreamingMetrics>);

impl ActiveConnection {
    fn new(metrics: Arc<StreamingMetrics>) -> Self {
        metrics.connection_opened();
        Self(metrics)
    }
}

impl Drop for ActiveConnection {
    fn drop(&mut self) {
        self.0.connection_closed();
    }
}

/// Apply the configured deadline to every server-to-client frame.
async fn send_socket_message(
    socket: &mut WebSocket,
    message: Message,
    timeout: std::time::Duration,
    metrics: &StreamingMetrics,
) -> bool {
    match tokio::time::timeout(timeout, socket.send(message)).await {
        Ok(Ok(())) => true,
        Ok(Err(error)) => {
            debug!(%error, "streaming socket send failed");
            false
        }
        Err(_) => {
            metrics.send_timed_out();
            warn!(?timeout, "streaming socket send timed out");
            false
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
        future::Future,
        net::{IpAddr, Ipv4Addr, SocketAddr},
        pin::Pin,
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };

    use axum::{
        Json, Router,
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

    use super::{custom_emojis, streaming_token, update_stream_subscription};
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

    /// Roosty advertises the standard public picker API without hosting local emoji.
    #[tokio::test]
    async fn custom_emoji_picker_is_public_and_empty() {
        let Json(emojis) = custom_emojis().await;
        assert!(emojis.is_empty());
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
        let token = context.access_token().await;

        let response = context
            .authenticated_get("/api/v1/push/subscription", &token)
            .await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(json_body(response).await["error"], "Record not found");
    }

    #[test_context(CompatContext)]
    #[tokio::test]
    async fn push_subscription_lifecycle_accepts_valid_typed_data(context: &mut CompatContext) {
        let token = context.access_token().await;
        let response = context
            .authenticated_form_request(
                axum::http::Method::POST,
                "/api/v1/push/subscription",
                &token,
                valid_push_form("https://1.1.1.1/push"),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let created = json_body(response).await;
        assert_eq!(created["endpoint"], "https://1.1.1.1/push");
        assert_eq!(created["standard"], true);
        assert_eq!(created["policy"], "all");
        assert_eq!(created["alerts"]["mention"], true);
        assert!(
            created["server_key"]
                .as_str()
                .is_some_and(|key| !key.is_empty())
        );
        let created_id = uuid::Uuid::parse_str(created["id"].as_str().unwrap()).unwrap();
        assert_eq!(created_id.get_version_num(), 7);

        let first = context.authenticated_form_request(
            axum::http::Method::POST,
            "/api/v1/push/subscription",
            &token,
            valid_push_form("https://8.8.8.8/push"),
        );
        let second = context.authenticated_form_request(
            axum::http::Method::POST,
            "/api/v1/push/subscription",
            &token,
            valid_push_form("https://9.9.9.9/push"),
        );
        let (first, second) = tokio::join!(first, second);
        assert_eq!(first.status(), StatusCode::OK);
        assert_eq!(second.status(), StatusCode::OK);

        let response = context
            .authenticated_get("/api/v1/push/subscription", &token)
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let current = json_body(response).await;
        assert_eq!(current["id"], created["id"]);
        assert!(matches!(
            current["endpoint"].as_str(),
            Some("https://8.8.8.8/push") | Some("https://9.9.9.9/push")
        ));

        let actor_id = AccountId(
            roosty_db::create_local_account(
                &context.db,
                "push_actor",
                "push-actor@example.com",
                &password::hash_password("password").unwrap(),
            )
            .await
            .unwrap(),
        );
        let notification = roosty_db::notify_local_account(
            &context.db,
            context.account_id,
            roosty_db::LocalNotificationType::Follow,
            actor_id,
            None,
        )
        .await
        .unwrap();
        for notification_type in [
            roosty_db::LocalNotificationType::Mention,
            roosty_db::LocalNotificationType::Favourite,
            roosty_db::LocalNotificationType::Reblog,
            roosty_db::LocalNotificationType::Follow,
            roosty_db::LocalNotificationType::FollowRequest,
            roosty_db::LocalNotificationType::Status,
            roosty_db::LocalNotificationType::Update,
            roosty_db::LocalNotificationType::Quote,
            roosty_db::LocalNotificationType::QuotedUpdate,
        ] {
            let mut typed_notification = notification.clone();
            typed_notification.notification_type = notification_type;
            let payload = crate::notifications::push_payload(
                &context.db,
                &context.config.public_base_url,
                typed_notification,
                token.clone(),
            )
            .await
            .unwrap();
            let payload = serde_json::to_value(payload).unwrap();
            assert_eq!(payload["access_token"], token);
            assert_eq!(payload["notification_id"], notification.id.to_string());
            assert_eq!(payload["notification_type"], notification_type.as_str());
            assert_eq!(payload["preferred_locale"], "en");
            assert!(
                payload["body"]
                    .as_str()
                    .is_some_and(|body| !body.is_empty())
            );
        }
        let mut invalid_actor = notification.clone();
        invalid_actor.remote_actor_id = Some(AccountId(uuid::Uuid::now_v7()));
        assert!(matches!(
            crate::notifications::push_payload(
                &context.db,
                &context.config.public_base_url,
                invalid_actor,
                token.clone(),
            )
            .await,
            Err(roosty_core::RoostyError::InvalidInput(_))
        ));
        assert!(
            roosty_db::push_policy_allows(&context.db, &notification, roosty_db::PushPolicy::All)
                .await
                .unwrap()
        );
        assert!(
            !roosty_db::push_policy_allows(
                &context.db,
                &notification,
                roosty_db::PushPolicy::None,
            )
            .await
            .unwrap()
        );
        assert!(
            !roosty_db::push_policy_allows(
                &context.db,
                &notification,
                roosty_db::PushPolicy::Followed,
            )
            .await
            .unwrap()
        );
        roosty_db::follow_local_account(&context.db, context.account_id, actor_id, true, false)
            .await
            .unwrap();
        assert!(
            roosty_db::push_policy_allows(
                &context.db,
                &notification,
                roosty_db::PushPolicy::Followed,
            )
            .await
            .unwrap()
        );
        assert!(
            !roosty_db::push_policy_allows(
                &context.db,
                &notification,
                roosty_db::PushPolicy::Follower,
            )
            .await
            .unwrap()
        );
        roosty_db::follow_local_account(&context.db, actor_id, context.account_id, true, false)
            .await
            .unwrap();
        assert!(
            roosty_db::push_policy_allows(
                &context.db,
                &notification,
                roosty_db::PushPolicy::Follower,
            )
            .await
            .unwrap()
        );
        let job = roosty_db::claim_due_job(&context.db, "push-test", time::Duration::minutes(1))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(job.kind, "web_push_delivery");
        assert_eq!(job.id.0.get_version_num(), 7);
        assert_eq!(job.payload["notification_id"], notification.id.to_string());
        assert_eq!(job.payload["subscription_id"], created["id"]);
        assert!(
            roosty_db::mark_job_completed(&context.db, &job)
                .await
                .unwrap()
        );

        let response = context
            .authenticated_form_request(
                axum::http::Method::PUT,
                "/api/v1/push/subscription",
                &token,
                "data%5Bpolicy%5D=followed&data%5Balerts%5D%5Bmention%5D=false&data%5Balerts%5D%5Bfollow%5D=1".to_owned(),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let updated = json_body(response).await;
        assert_eq!(updated["policy"], "followed");
        assert_eq!(updated["alerts"]["mention"], false);
        assert_eq!(updated["alerts"]["follow"], true);

        for _ in 0..2 {
            let response = context
                .authenticated_request(
                    axum::http::Method::DELETE,
                    "/api/v1/push/subscription",
                    &token,
                    Body::empty(),
                    None,
                )
                .await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(json_body(response).await, serde_json::json!({}));
        }
        let response = context
            .authenticated_form_request(
                axum::http::Method::POST,
                "/api/v1/push/subscription",
                &token,
                valid_push_form("https://1.0.0.1/push"),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let grant =
            roosty_db::find_access_token_grant(&context.db, &context.config.token_pepper, &token)
                .await
                .unwrap()
                .unwrap();
        roosty_db::revoke_access_token(&context.db, &context.config.token_pepper, &token)
            .await
            .unwrap();
        assert!(
            roosty_db::push_subscription_for_access_token(&context.db, grant.id)
                .await
                .unwrap()
                .is_none()
        );
    }

    /// Given Elk's Masto.js payload, creating and partially updating a subscription succeeds.
    #[test_context(CompatContext)]
    #[tokio::test]
    async fn push_subscription_accepts_elk_json(context: &mut CompatContext) {
        let token = context.access_token().await;
        let response = context
            .authenticated_json_request(
                axum::http::Method::POST,
                "/api/v1/push/subscription",
                &token,
                serde_json::json!({
                    "policy": "all",
                    "subscription": {
                        "endpoint": "https://1.1.1.1/push",
                        "keys": {
                            "p256dh": PUSH_P256DH,
                            "auth": PUSH_AUTH,
                        },
                    },
                    "data": {
                        "alerts": {
                            "follow": true,
                            "favourite": true,
                            "reblog": true,
                            "mention": true,
                            "poll": true,
                        },
                    },
                }),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let created = json_body(response).await;
        assert_eq!(created["standard"], false);
        assert_eq!(created["policy"], "all");
        assert_eq!(created["alerts"]["mention"], true);
        assert_eq!(created["alerts"]["follow"], true);
        assert!(created["alerts"].get("poll").is_none());

        let response = context
            .authenticated_json_request(
                axum::http::Method::PUT,
                "/api/v1/push/subscription",
                &token,
                serde_json::json!({
                    "data": {
                        "alerts": {
                            "mention": false,
                        },
                    },
                }),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let updated = json_body(response).await;
        assert_eq!(updated["alerts"]["mention"], false);
        assert_eq!(updated["alerts"]["follow"], true);
        assert_eq!(updated["policy"], "all");
    }

    /// Invalid typed JSON is rejected before a push subscription is persisted.
    #[test_context(CompatContext)]
    #[tokio::test]
    async fn push_subscription_rejects_invalid_json(context: &mut CompatContext) {
        let token = context.access_token().await;
        for body in [
            serde_json::json!({
                "policy": "somebody",
                "subscription": {
                    "endpoint": "https://1.1.1.1/push",
                    "keys": { "p256dh": PUSH_P256DH, "auth": PUSH_AUTH },
                },
            }),
            serde_json::json!({
                "policy": "all",
                "subscription": {
                    "endpoint": "https://1.1.1.1/push",
                    "keys": { "p256dh": PUSH_P256DH, "auth": "AQID" },
                },
            }),
        ] {
            let response = context
                .authenticated_json_request(
                    axum::http::Method::POST,
                    "/api/v1/push/subscription",
                    &token,
                    body,
                )
                .await;
            assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
            assert!(json_body(response).await["error"].as_str().is_some());
        }
        let grant =
            roosty_db::find_access_token_grant(&context.db, &context.config.token_pepper, &token)
                .await
                .unwrap()
                .unwrap();
        assert!(
            roosty_db::push_subscription_for_access_token(&context.db, grant.id)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[test_context(CompatContext)]
    #[tokio::test]
    async fn push_subscription_rejects_invalid_data_without_persisting(
        context: &mut CompatContext,
    ) {
        let token = context.access_token().await;
        let valid = valid_push_form("https://1.1.1.1/push");
        let cases = [
            valid.replace(
                "subscription%5Bendpoint%5D=https%3A%2F%2F1.1.1.1%2Fpush&",
                "",
            ),
            valid.replace(
                &format!("subscription%5Bkeys%5D%5Bp256dh%5D={PUSH_P256DH}&"),
                "",
            ),
            valid.replace(
                &format!("subscription%5Bkeys%5D%5Bauth%5D={PUSH_AUTH}&"),
                "",
            ),
            valid.replace(PUSH_AUTH, "not-base64!"),
            valid.replace(PUSH_AUTH, "AQID"),
            valid.replace("data%5Bpolicy%5D=all", "data%5Bpolicy%5D=somebody"),
            valid.replace(
                "data%5Balerts%5D%5Bmention%5D=true",
                "data%5Balerts%5D%5Bmention%5D=maybe",
            ),
            valid.replace(
                "subscription%5Bstandard%5D=true",
                "subscription%5Bstandard%5D=maybe",
            ),
            valid_push_form("https://127.0.0.1/push"),
            valid_push_form("http://1.1.1.1/push"),
            valid_push_form("https://user:password@1.1.1.1/push"),
        ];
        for body in cases {
            let response = context
                .authenticated_form_request(
                    axum::http::Method::POST,
                    "/api/v1/push/subscription",
                    &token,
                    body,
                )
                .await;
            assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
            assert!(json_body(response).await["error"].as_str().is_some());
        }
        assert!(
            roosty_db::find_access_token_grant(&context.db, &context.config.token_pepper, &token)
                .await
                .unwrap()
                .is_some()
        );
        let grant =
            roosty_db::find_access_token_grant(&context.db, &context.config.token_pepper, &token)
                .await
                .unwrap()
                .unwrap();
        assert!(
            roosty_db::push_subscription_for_access_token(&context.db, grant.id)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[test_context(CompatContext)]
    #[tokio::test]
    async fn push_subscription_requires_push_scope(context: &mut CompatContext) {
        let token = roosty_db::create_access_token(
            &context.db,
            &context.config.token_pepper,
            context.account_id,
            context.application_id,
            "read write",
        )
        .await
        .unwrap()
        .token;
        let response = context
            .authenticated_form_request(
                axum::http::Method::POST,
                "/api/v1/push/subscription",
                &token,
                valid_push_form("https://1.1.1.1/push"),
            )
            .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[test_context(CompatContext)]
    #[tokio::test]
    async fn push_delivery_classifies_success_retry_and_permanent_rejection(
        context: &mut CompatContext,
    ) {
        let token = context.access_token().await;
        let response = context
            .authenticated_form_request(
                axum::http::Method::POST,
                "/api/v1/push/subscription",
                &token,
                valid_push_form("https://1.1.1.1/push"),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let actor_id = AccountId(
            roosty_db::create_local_account(
                &context.db,
                "delivery_actor",
                "delivery-actor@example.com",
                &password::hash_password("password").unwrap(),
            )
            .await
            .unwrap(),
        );
        roosty_db::notify_local_account(
            &context.db,
            context.account_id,
            roosty_db::LocalNotificationType::Follow,
            actor_id,
            None,
        )
        .await
        .unwrap();
        let job =
            roosty_db::claim_due_job(&context.db, "delivery-test", time::Duration::minutes(1))
                .await
                .unwrap()
                .unwrap();

        let service = crate::push::PushService::with_sender(
            &context.config,
            context.db.clone(),
            Arc::new(StaticPushSender(roosty_web_push::DeliveryOutcome::Success)),
        );
        service.deliver(job.payload.clone()).await.unwrap();

        let retry = crate::push::PushService::with_sender(
            &context.config,
            context.db.clone(),
            Arc::new(StaticPushSender(
                roosty_web_push::DeliveryOutcome::Retryable {
                    status: Some(429),
                    retry_after: Some(std::time::Duration::from_secs(30)),
                },
            )),
        );
        assert!(matches!(
            retry.deliver(job.payload.clone()).await,
            Err(crate::push::PushDeliveryError::Retryable { status: Some(429) })
        ));

        let permanent = crate::push::PushService::with_sender(
            &context.config,
            context.db.clone(),
            Arc::new(StaticPushSender(
                roosty_web_push::DeliveryOutcome::PermanentFailure { status: 410 },
            )),
        );
        permanent.deliver(job.payload).await.unwrap();
        let grant =
            roosty_db::find_access_token_grant(&context.db, &context.config.token_pepper, &token)
                .await
                .unwrap()
                .unwrap();
        assert!(
            roosty_db::push_subscription_for_access_token(&context.db, grant.id)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[test_context(CompatContext)]
    #[tokio::test]
    async fn streaming_health_works(context: &mut CompatContext) {
        let health = context.get("/api/v1/streaming/health").await;
        assert_eq!(health.status(), StatusCode::OK);
        let body = to_bytes(health.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], b"OK");
    }

    #[test_context(CompatContext)]
    #[tokio::test]
    /// Given two initialized processes, when all event kinds are published, then the remote process receives each once without startup replay.
    async fn streaming_events_cross_process_once_without_startup_replay(
        context: &mut CompatContext,
    ) {
        let alpha = AppState::new(context.config.clone(), context.db.clone());
        alpha.streaming_events.initialize_listener().await.unwrap();
        alpha
            .streaming_events
            .publish_delete("before-startup", context.account_id, "public", &[]);
        wait_for_streaming_sequence(&context.db, 1).await;

        let beta = AppState::new(context.config.clone(), context.db.clone());
        beta.streaming_events.initialize_listener().await.unwrap();
        let mut receiver = beta.streaming_events.subscribe();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(150), receiver.recv())
                .await
                .is_err()
        );
        assert_eq!(
            roosty_db::delete_streaming_events_before(
                &context.db,
                time::OffsetDateTime::now_utc() + time::Duration::seconds(1),
            )
            .await
            .unwrap(),
            1
        );

        alpha.streaming_events.publish_status_update(
            &serde_json::json!({"id": "update"}),
            context.account_id,
            "public",
            &[],
        );
        alpha.streaming_events.publish_status_edit(
            &serde_json::json!({"id": "status-update"}),
            context.account_id,
            "public",
            &[],
            &[context.account_id],
        );
        alpha.streaming_events.publish_notification(
            &serde_json::json!({"id": "notification"}),
            context.account_id,
        );
        alpha.streaming_events.publish_conversation(
            &serde_json::json!({"id": "conversation"}),
            context.account_id,
        );
        alpha
            .streaming_events
            .publish_delete("deleted-status", context.account_id, "public", &[]);

        let streams = [
            "user".to_owned(),
            "user:notification".to_owned(),
            "direct".to_owned(),
            "public".to_owned(),
        ];
        let mut event_names = Vec::new();
        for _ in 0..5 {
            let event = tokio::time::timeout(std::time::Duration::from_secs(5), receiver.recv())
                .await
                .unwrap()
                .unwrap();
            let message = event
                .to_socket_message(context.account_id, &streams)
                .unwrap()
                .unwrap();
            let message = serde_json::from_str::<Value>(&message).unwrap();
            if message["event"] == "status.update" {
                assert_eq!(
                    message["stream"],
                    serde_json::json!(["user", "user:notification", "public"])
                );
            }
            event_names.push(message["event"].clone());
        }
        assert_eq!(
            event_names,
            [
                "update",
                "status.update",
                "notification",
                "conversation",
                "delete"
            ]
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(150), receiver.recv())
                .await
                .is_err()
        );

        alpha.streaming_events.shutdown();
        beta.streaming_events.shutdown();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        drop(alpha);
        drop(beta);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
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
                // Fixed test-only PKCS#8 key. pragma: allowlist secret
                vapid_private_key: Some(TEST_VAPID_PRIVATE_KEY.to_owned()),
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
                db,
                config,
                account_id,
                application_id: application.id,
                _temp_dir: temp_dir,
            }
        }

        async fn teardown(self) {
            self.db.close().await.unwrap();
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

        async fn authenticated_request(
            &self,
            method: axum::http::Method,
            uri: &str,
            token: &str,
            body: Body,
            content_type: Option<&str>,
        ) -> axum::http::Response<Body> {
            let mut request = Request::builder()
                .method(method)
                .uri(uri)
                .header(AUTHORIZATION, format!("Bearer {token}"));
            if let Some(content_type) = content_type {
                request = request.header(axum::http::header::CONTENT_TYPE, content_type);
            }
            self.request(request.body(body).unwrap()).await
        }

        async fn authenticated_form_request(
            &self,
            method: axum::http::Method,
            uri: &str,
            token: &str,
            body: String,
        ) -> axum::http::Response<Body> {
            self.authenticated_request(
                method,
                uri,
                token,
                Body::from(body),
                Some("application/x-www-form-urlencoded"),
            )
            .await
        }

        async fn authenticated_json_request(
            &self,
            method: axum::http::Method,
            uri: &str,
            token: &str,
            body: Value,
        ) -> axum::http::Response<Body> {
            self.authenticated_request(
                method,
                uri,
                token,
                Body::from(serde_json::to_vec(&body).unwrap()),
                Some("application/json"),
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

    async fn wait_for_streaming_sequence(db: &roosty_db::DbConnection, expected: i64) {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if roosty_db::latest_streaming_event_sequence(db)
                    .await
                    .unwrap()
                    >= expected
                {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    fn unique_name() -> String {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();

        format!("roosty_compat_{}_{}", std::process::id(), timestamp)
    }

    const TEST_VAPID_PRIVATE_KEY: &str = "MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQg7ki2JNeU+GLhnNacatYTpVJNFd3uIKWr+Inj/vYFMAShRANCAAQyUFnxhJ7CSBxmKk5Qj6d0UWOBJ68nwsB+XAxsp4hAJ/mVfmeryWYGKx9JaZaAWBfSybFhK0inH6o1XIJH5CRW";
    const PUSH_AUTH: &str = "MTIzNDU2Nzg5MGFiY2RlZg";
    const PUSH_P256DH: &str =
        "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";

    fn valid_push_form(endpoint: &str) -> String {
        let endpoint: String = url::form_urlencoded::byte_serialize(endpoint.as_bytes()).collect();
        format!(
            "subscription%5Bendpoint%5D={endpoint}&subscription%5Bkeys%5D%5Bp256dh%5D={PUSH_P256DH}&subscription%5Bkeys%5D%5Bauth%5D={PUSH_AUTH}&subscription%5Bstandard%5D=true&data%5Bpolicy%5D=all&data%5Balerts%5D%5Bmention%5D=true&data%5Balerts%5D%5Bfollow%5D=true"
        )
    }

    struct StaticPushSender(roosty_web_push::DeliveryOutcome);

    impl crate::push::PushSender for StaticPushSender {
        fn send<'a>(
            &'a self,
            _subscription: &'a roosty_web_push::Subscription,
            _payload: &'a [u8],
            _options: roosty_web_push::SendOptions,
        ) -> Pin<
            Box<
                dyn Future<
                        Output = std::result::Result<
                            roosty_web_push::DeliveryOutcome,
                            roosty_web_push::WebPushError,
                        >,
                    > + Send
                    + 'a,
            >,
        > {
            Box::pin(async move { Ok(self.0) })
        }

        fn public_key(&self) -> String {
            "test-public-key".to_owned()
        }
    }
}
