use std::sync::Arc;
use tokio::sync::Semaphore;

use axum::{
    Router,
    extract::State,
    http::{Method, StatusCode, header},
    response::IntoResponse,
    routing::get,
};
use tower_http::{
    cors::{Any, CorsLayer},
    trace::{DefaultMakeSpan, DefaultOnResponse, HttpMakeClassifier, TraceLayer},
};
use tracing::Level;

use crate::{config::Config, streaming::StreamingEvents};

/// Shared Axum application state.
#[derive(Clone)]
pub struct AppState {
    /// Validated application configuration.
    pub config: Arc<Config>,
    /// Database connection pool.
    pub db: roosty_db::DbConnection,
    /// Bounded local and cross-process Mastodon streaming event bus.
    pub streaming_events: StreamingEvents,
    /// Per-process permit pool held for each upgraded streaming socket.
    pub streaming_connections: Arc<Semaphore>,
}

impl AppState {
    /// Create shared application state from config and database connection.
    pub fn new(config: Config, db: roosty_db::DbConnection) -> Self {
        let streaming_events = StreamingEvents::new(
            db.clone(),
            config.database_url.clone(),
            config.streaming.event_retention,
        );
        let streaming_connections = Arc::new(Semaphore::new(config.streaming.max_connections));
        Self {
            config: Arc::new(config),
            db,
            streaming_events,
            streaming_connections,
        }
    }
}

/// Build the public application router.
pub fn app_router(state: AppState, include_infra_routes: bool) -> Router {
    let public_router = Router::<AppState>::new()
        .merge(crate::accounts::router())
        .merge(crate::auth::router())
        .merge(crate::compat::router())
        .merge(crate::conversations::router())
        .merge(crate::federation::router())
        .merge(crate::instance::router())
        .merge(crate::media::router())
        .merge(crate::markers::router())
        .merge(crate::notifications::router())
        .merge(crate::search::router())
        .merge(crate::statuses::router())
        .merge(crate::version::router())
        .fallback(public_fallback)
        .layer(request_trace_layer())
        .layer(public_cors_layer());
    let router = if include_infra_routes {
        public_router.merge(infra_routes())
    } else {
        public_router
    };

    router.with_state(state)
}

/// Build the infrastructure-only router.
pub fn infra_router(state: AppState) -> Router {
    infra_routes().with_state(state)
}

/// Build routes intended for infrastructure probes and scraping.
fn infra_routes() -> Router<AppState> {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics))
        .layer(request_trace_layer())
}

/// Build request tracing that emits one completion event per HTTP request.
fn request_trace_layer()
-> TraceLayer<HttpMakeClassifier, DefaultMakeSpan, (), DefaultOnResponse, (), (), ()> {
    TraceLayer::new_for_http()
        .make_span_with(DefaultMakeSpan::new().level(Level::INFO))
        .on_request(())
        .on_response(DefaultOnResponse::new().level(Level::INFO))
        .on_body_chunk(())
        .on_eos(())
        .on_failure(())
}

/// Build the public CORS policy used by browser-based Mastodon clients.
fn public_cors_layer() -> CorsLayer {
    // Browser clients call API routes cross-origin with bearer tokens. Do not
    // enable credentialed CORS here; browser login cookies stay same-site.
    CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([header::ACCEPT, header::AUTHORIZATION, header::CONTENT_TYPE])
}

/// Handle public fallback responses while allowing CORS preflight requests.
async fn public_fallback(method: Method) -> impl IntoResponse {
    if method == Method::OPTIONS {
        StatusCode::NO_CONTENT.into_response()
    } else {
        (StatusCode::NOT_FOUND, "not found\n").into_response()
    }
}

async fn healthz() -> &'static str {
    "ok\n"
}

/// Check whether the server can reach its configured database.
async fn readyz(State(state): State<AppState>) -> impl IntoResponse {
    if !state.streaming_events.listener_is_ready() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "streaming listener unavailable\n",
        )
            .into_response();
    }
    match roosty_db::ping(&state.db).await {
        Ok(()) => (StatusCode::OK, "ok\n").into_response(),
        Err(error) => (
            StatusCode::SERVICE_UNAVAILABLE,
            format!("database unavailable: {error}\n"),
        )
            .into_response(),
    }
}

/// Render Prometheus-compatible process and configuration metrics.
async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    let federation_enabled = u8::from(state.config.federation_enabled);
    let mut body = format!(
        concat!(
            "# HELP roosty_process_up Process liveness marker.\n",
            "# TYPE roosty_process_up gauge\n",
            "roosty_process_up 1\n",
            "# HELP roosty_federation_enabled Federation enabled configuration flag.\n",
            "# TYPE roosty_federation_enabled gauge\n",
            "roosty_federation_enabled {}\n",
        ),
        federation_enabled
    );
    body.push_str(&crate::federation::metrics_text());
    body.push_str(&state.streaming_events.metrics().text());

    ([(header::CONTENT_TYPE, "text/plain; version=0.0.4")], body)
}

#[cfg(test)]
mod tests {
    use axum::{
        Router,
        body::Body,
        http::{
            Request, StatusCode,
            header::{
                ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN,
                ACCESS_CONTROL_REQUEST_METHOD, ORIGIN,
            },
        },
    };
    use tower::ServiceExt;

    use super::{public_cors_layer, public_fallback};

    #[tokio::test]
    async fn cors_headers_are_added_to_public_preflight_fallback() {
        let response = public_test_router()
            .oneshot(
                Request::builder()
                    .method("OPTIONS")
                    .uri("/api/v1/preferences")
                    .header(ORIGIN, "https://localhost:4001")
                    .header(ACCESS_CONTROL_REQUEST_METHOD, "GET")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert!(response.status().is_success());
        assert_eq!(
            response.headers().get(ACCESS_CONTROL_ALLOW_ORIGIN).unwrap(),
            "*"
        );
    }

    /// Given a browser preflight for a status edit, the public API permits PUT.
    #[tokio::test]
    async fn cors_preflight_allows_put_requests() {
        let response = public_test_router()
            .oneshot(
                Request::builder()
                    .method("OPTIONS")
                    .uri("/api/v1/statuses/status-id")
                    .header(ORIGIN, "https://localhost:4001")
                    .header(ACCESS_CONTROL_REQUEST_METHOD, "PUT")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert!(response.status().is_success());
        let allowed_methods = response
            .headers()
            .get(ACCESS_CONTROL_ALLOW_METHODS)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            allowed_methods
                .split(',')
                .any(|method| method.trim() == "PUT")
        );
    }

    #[tokio::test]
    async fn cors_headers_are_added_to_public_not_found_responses() {
        let response = public_test_router()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/missing")
                    .header(ORIGIN, "https://localhost:4001")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response.headers().get(ACCESS_CONTROL_ALLOW_ORIGIN).unwrap(),
            "*"
        );
    }

    fn public_test_router() -> Router {
        Router::new()
            .fallback(public_fallback)
            .layer(public_cors_layer())
    }
}
