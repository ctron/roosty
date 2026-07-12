use std::sync::Arc;

use axum::{
    Router,
    extract::State,
    http::{StatusCode, header},
    response::IntoResponse,
    routing::get,
};

use crate::config::Config;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: roost_db::DbConnection,
}

impl AppState {
    pub fn new(config: Config, db: roost_db::DbConnection) -> Self {
        Self {
            config: Arc::new(config),
            db,
        }
    }
}

pub fn app_router(state: AppState, include_infra_routes: bool) -> Router {
    let router = Router::<AppState>::new();
    let router = if include_infra_routes {
        router.merge(infra_routes())
    } else {
        router
    };

    router.with_state(state)
}

pub fn infra_router(state: AppState) -> Router {
    infra_routes().with_state(state)
}

fn infra_routes() -> Router<AppState> {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics))
}

async fn healthz() -> &'static str {
    "ok\n"
}

async fn readyz(State(state): State<AppState>) -> impl IntoResponse {
    match roost_db::ping(&state.db).await {
        Ok(()) => (StatusCode::OK, "ok\n").into_response(),
        Err(error) => (
            StatusCode::SERVICE_UNAVAILABLE,
            format!("database unavailable: {error}\n"),
        )
            .into_response(),
    }
}

async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    let federation_enabled = u8::from(state.config.federation_enabled);
    let body = format!(
        concat!(
            "# HELP roost_process_up Process liveness marker.\n",
            "# TYPE roost_process_up gauge\n",
            "roost_process_up 1\n",
            "# HELP roost_federation_enabled Federation enabled configuration flag.\n",
            "# TYPE roost_federation_enabled gauge\n",
            "roost_federation_enabled {}\n",
        ),
        federation_enabled
    );

    ([(header::CONTENT_TYPE, "text/plain; version=0.0.4")], body)
}
