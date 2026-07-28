//! Mastodon-compatible public Explore and trend discovery.

use axum::{
    Json, Router,
    extract::{Query, State, rejection::QueryRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use roosty_core::RoostyError;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::{http::AppState, statuses::TagResponse};

const DEFAULT_TAG_LIMIT: u64 = 10;
const MAX_TAG_LIMIT: u64 = 20;

/// Build routes for Mastodon's public trend discovery API.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/trends", get(trending_tags))
        .route("/api/v1/trends/tags", get(trending_tags))
}

#[derive(Debug, Default, Deserialize)]
struct TrendParams {
    limit: Option<u64>,
    offset: Option<u64>,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: &'static str,
}

async fn trending_tags(
    State(state): State<AppState>,
    query: Result<Query<TrendParams>, QueryRejection>,
) -> Response {
    let Query(params) = match query {
        Ok(params) => params,
        Err(_) => return bad_request(),
    };
    let limit = params
        .limit
        .unwrap_or(DEFAULT_TAG_LIMIT)
        .clamp(1, MAX_TAG_LIMIT);
    let offset = params.offset.unwrap_or_default();
    if i64::try_from(offset).is_err() {
        return bad_request();
    }

    match roosty_db::trending_tags(&state.db, limit, offset).await {
        Ok(trends) => Json(
            trends
                .into_iter()
                .map(|trend| TagResponse::new(&state, trend.tag, trend.history, None))
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(error) => server_error(error),
    }
}

fn bad_request() -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ErrorResponse {
            error: "Invalid request parameters",
        }),
    )
        .into_response()
}

fn server_error(error: RoostyError) -> Response {
    warn!(%error, "trending tag query failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse {
            error: "Internal server error",
        }),
    )
        .into_response()
}
