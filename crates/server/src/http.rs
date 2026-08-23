use std::{borrow::Cow, ops::Deref, sync::Arc};
use tokio::sync::Semaphore;

use axum::{
    Extension, Json, Router,
    extract::{FromRef, State, rejection::ExtensionRejection},
    http::{Method, StatusCode, header, header::InvalidHeaderValue},
    response::{IntoResponse, Response},
    routing::get,
};
use roosty_core::{
    AccountId, AccountRelationshipError, FederationDiscoveryError, Result as RoostyResult,
    RoostyError,
};
use roosty_db::{DbConnection, StatusCreationReservation};
use roosty_db::{begin_status_creation, ping};
use sea_orm::{AccessMode, DatabaseTransaction, DbErr, IsolationLevel, TransactionTrait};
use serde_json::Error as JsonError;
use thiserror::Error;
use tower_http::{
    cors::{Any, CorsLayer},
    trace::{DefaultMakeSpan, DefaultOnResponse, HttpMakeClassifier, TraceLayer},
};
use tracing::Level;

use crate::{
    accounts, admin, auth, compat, config::Config, conversations, explore, featured_tags,
    federation, instance, lists, markers, media, notifications, polls, push, push::PushService,
    reports, search, statuses, streaming::StreamingEvents, version, web,
};
use leptos::config::LeptosOptions;

pub(crate) type ApiResult<T> = Result<T, ApiError>;

/// Shared Mastodon-compatible failures returned by HTTP API handlers.
#[derive(Debug, Error)]
pub(crate) enum ApiError {
    #[error("{0}")]
    BadRequest(Cow<'static, str>),
    #[error("{0}")]
    Unauthorized(Cow<'static, str>),
    #[error("{description}")]
    OAuth {
        error: Cow<'static, str>,
        description: Cow<'static, str>,
    },
    #[error("{0}")]
    Forbidden(Cow<'static, str>),
    #[error("{0}")]
    NotFound(Cow<'static, str>),
    #[error("{0}")]
    Unprocessable(Cow<'static, str>),
    #[error("{0}")]
    ServiceUnavailable(Cow<'static, str>),
    #[error(transparent)]
    Internal(RoostyError),
}

impl From<DbErr> for ApiError {
    fn from(error: DbErr) -> Self {
        Self::Internal(error.into())
    }
}

impl From<RoostyError> for ApiError {
    fn from(error: RoostyError) -> Self {
        match error {
            RoostyError::AccountRelationship(
                AccountRelationshipError::FollowTargetNotFound
                | AccountRelationshipError::ModerationTargetNotFound,
            ) => Self::NotFound("Record not found".into()),
            RoostyError::AccountRelationship(AccountRelationshipError::FollowBlocked) => {
                Self::Forbidden(error.to_string().into())
            }
            RoostyError::AccountRelationship(_) | RoostyError::InvalidInput(_) => {
                Self::BadRequest(error.to_string().into())
            }
            RoostyError::FederationDiscovery(FederationDiscoveryError::PolicyRejected(_)) => {
                Self::NotFound("Record not found".into())
            }
            error => Self::Internal(error),
        }
    }
}

impl From<AccountRelationshipError> for ApiError {
    fn from(error: AccountRelationshipError) -> Self {
        RoostyError::from(error).into()
    }
}

impl From<ExtensionRejection> for ApiError {
    fn from(error: ExtensionRejection) -> Self {
        Self::Internal(RoostyError::Configuration(error.to_string()))
    }
}

impl From<InvalidHeaderValue> for ApiError {
    fn from(error: InvalidHeaderValue) -> Self {
        Self::Internal(RoostyError::Configuration(error.to_string()))
    }
}

impl From<JsonError> for ApiError {
    fn from(error: JsonError) -> Self {
        Self::Internal(error.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, error, description) = match self {
            Self::BadRequest(error) => (StatusCode::BAD_REQUEST, error, None),
            Self::Unauthorized(error) => (StatusCode::UNAUTHORIZED, error, None),
            Self::OAuth { error, description } => {
                (StatusCode::UNAUTHORIZED, error, Some(description))
            }
            Self::Forbidden(error) => (StatusCode::FORBIDDEN, error, None),
            Self::NotFound(error) => (StatusCode::NOT_FOUND, error, None),
            Self::Unprocessable(error) => (StatusCode::UNPROCESSABLE_ENTITY, error, None),
            Self::ServiceUnavailable(error) => (StatusCode::SERVICE_UNAVAILABLE, error, None),
            Self::Internal(error) => {
                tracing::error!(%error, "API operation failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Cow::Borrowed("Internal server error"),
                    None,
                )
            }
        };
        let body = match description {
            Some(description) => serde_json::json!({
                "error": error,
                "error_description": description,
            }),
            None => serde_json::json!({ "error": error }),
        };
        (status, Json(body)).into_response()
    }
}

/// Infrastructure readiness failures retain the plain-text probe response.
#[derive(Debug, Error)]
enum ReadinessError {
    #[error("streaming listener unavailable")]
    Streaming,
    #[error("database unavailable: {0}")]
    Database(RoostyError),
}

impl From<DbErr> for ReadinessError {
    fn from(error: DbErr) -> Self {
        Self::Database(error.into())
    }
}

impl From<RoostyError> for ReadinessError {
    fn from(error: RoostyError) -> Self {
        Self::Database(error)
    }
}

impl IntoResponse for ReadinessError {
    fn into_response(self) -> Response {
        (StatusCode::SERVICE_UNAVAILABLE, format!("{self}\n")).into_response()
    }
}

/// Shared Axum application state.
#[derive(Clone)]
pub struct AppState {
    /// Validated application configuration.
    pub config: Arc<Config>,
    /// Bounded local and cross-process Mastodon streaming event bus.
    pub streaming_events: StreamingEvents,
    /// Per-process permit pool held for each upgraded streaming socket.
    pub streaming_connections: Arc<Semaphore>,
    /// Per-process bound on concurrent outbound preview-card requests.
    pub preview_card_fetches: Arc<Semaphore>,
    /// Web Push API, credential protection, and delivery service.
    pub push: PushService,
    /// Asset and hydration settings for the first-party web UI.
    pub leptos_options: LeptosOptions,
}

impl AppState {
    /// Create shared application state from config and database connection.
    pub fn new(config: Config, db: DbConnection) -> Self {
        let streaming_events = StreamingEvents::new(
            db.clone(),
            config.database_url.clone(),
            config.streaming.event_retention,
        );
        let streaming_connections = Arc::new(Semaphore::new(config.streaming.max_connections));
        let preview_card_fetches = Arc::new(Semaphore::new(config.preview_card_fetch_concurrency));
        let push = PushService::new(&config, db.clone());
        Self {
            config: Arc::new(config),
            streaming_events,
            streaming_connections,
            preview_card_fetches,
            push,
            leptos_options: LeptosOptions::builder()
                .output_name("roosty-web")
                .site_root("target/site")
                .site_pkg_dir("pkg")
                .build(),
        }
    }

    /// Override the default UI settings with Cargo Leptos build configuration.
    pub fn with_leptos_options(mut self, leptos_options: LeptosOptions) -> Self {
        self.leptos_options = leptos_options;
        self
    }
}

/// Transaction-only database access installed as an Axum extension.
#[derive(Clone)]
pub struct DatabaseContext {
    db: DbConnection,
}

/// Application services paired with one caller-owned database transaction.
pub(crate) struct TransactionContext<'a, C> {
    pub(crate) state: &'a AppState,
    pub(crate) db: &'a C,
}

impl<'a, C> TransactionContext<'a, C> {
    pub(crate) fn new(state: &'a AppState, db: &'a C) -> Self {
        Self { state, db }
    }
}

impl<C> Deref for TransactionContext<'_, C> {
    type Target = AppState;

    fn deref(&self) -> &Self::Target {
        self.state
    }
}

impl DatabaseContext {
    /// Wrap a connection pool without exposing transaction-free query access.
    pub fn new(db: DbConnection) -> Self {
        Self { db }
    }

    /// Start a short read-only transaction for an isolated lookup.
    pub async fn begin_read(&self) -> Result<DatabaseTransaction, DbErr> {
        self.db
            .begin_with_config(None, Some(AccessMode::ReadOnly))
            .await
    }

    /// Start a stable read-only snapshot for a multi-query projection.
    pub async fn begin_snapshot(&self) -> Result<DatabaseTransaction, DbErr> {
        self.db
            .begin_with_config(
                Some(IsolationLevel::RepeatableRead),
                Some(AccessMode::ReadOnly),
            )
            .await
    }

    /// Start a transaction for an application mutation.
    pub async fn begin_write(&self) -> Result<DatabaseTransaction, DbErr> {
        self.db.begin().await
    }

    /// Acquire the transaction-backed status idempotency reservation.
    pub async fn begin_status_creation(
        &self,
        account_id: AccountId,
        key: &str,
    ) -> RoostyResult<StatusCreationReservation> {
        begin_status_creation(&self.db, account_id, key).await
    }
}

impl FromRef<AppState> for LeptosOptions {
    fn from_ref(state: &AppState) -> Self {
        state.leptos_options.clone()
    }
}

/// Build the public application router.
pub fn app_router(
    state: AppState,
    database: DatabaseContext,
    include_infra_routes: bool,
) -> Router {
    let public_router = Router::<AppState>::new()
        .merge(accounts::router())
        .merge(admin::router())
        .merge(featured_tags::router())
        .merge(auth::router())
        .merge(compat::router())
        .merge(conversations::router())
        .merge(explore::router())
        .merge(federation::router())
        .merge(instance::router())
        .merge(lists::router())
        .merge(media::router())
        .merge(markers::router())
        .merge(notifications::router())
        .merge(polls::router())
        .merge(push::router())
        .merge(reports::router())
        .merge(search::router())
        .merge(statuses::router())
        .merge(version::router())
        .merge(web::router(&state, &database))
        .fallback(public_fallback)
        .layer(request_trace_layer())
        .layer(public_cors_layer());
    let router = if include_infra_routes {
        public_router.merge(infra_routes())
    } else {
        public_router
    };

    router.layer(Extension(database)).with_state(state)
}

/// Build the infrastructure-only router.
pub fn infra_router(state: AppState, database: DatabaseContext) -> Router {
    infra_routes().layer(Extension(database)).with_state(state)
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
async fn readyz(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
) -> Result<&'static str, ReadinessError> {
    if !state.streaming_events.listener_is_ready() {
        return Err(ReadinessError::Streaming);
    }
    let txn = database.begin_read().await?;
    ping(&txn).await?;
    txn.commit().await?;
    Ok("ok\n")
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
    body.push_str(&federation::metrics_text());
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
