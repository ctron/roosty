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
use serde_json::Value;
use tracing::warn;

use crate::{
    auth::OptionalAuthenticatedAccount,
    http::AppState,
    statuses::{TagResponse, trending_status_models},
};

const DEFAULT_TAG_LIMIT: u64 = 10;
const MAX_TAG_LIMIT: u64 = 20;
const DEFAULT_STATUS_LIMIT: u64 = 20;
const MAX_STATUS_LIMIT: u64 = 40;

/// Build routes for Mastodon's public trend discovery API.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/trends", get(trending_tags))
        .route("/api/v1/trends/tags", get(trending_tags))
        .route("/api/v1/trends/statuses", get(trending_statuses))
        .route("/api/v1/trends/links", get(trending_links))
}

async fn trending_statuses(
    State(state): State<AppState>,
    OptionalAuthenticatedAccount(viewer): OptionalAuthenticatedAccount,
    query: Result<Query<TrendParams>, QueryRejection>,
) -> Response {
    let Query(params) = match query {
        Ok(params) => params,
        Err(_) => return bad_request(),
    };
    let limit = params
        .limit
        .unwrap_or(DEFAULT_STATUS_LIMIT)
        .clamp(1, MAX_STATUS_LIMIT);
    let offset = params.offset.unwrap_or_default();
    if i64::try_from(offset).is_err() {
        return bad_request();
    }

    match roosty_db::trending_statuses(&state.db, limit, offset).await {
        Ok(trends) => {
            match trending_status_models(&state, trends, viewer.as_ref().map(|account| account.id))
                .await
            {
                Ok(statuses) => Json(statuses).into_response(),
                Err(error) => server_error(error),
            }
        }
        Err(error) => server_error(error),
    }
}

async fn trending_links(query: Result<Query<TrendParams>, QueryRejection>) -> Response {
    let Query(params) = match query {
        Ok(params) => params,
        Err(_) => return bad_request(),
    };
    let limit = params
        .limit
        .unwrap_or(DEFAULT_TAG_LIMIT)
        .clamp(1, MAX_TAG_LIMIT);
    if i64::try_from(params.offset.unwrap_or_default()).is_err() {
        return bad_request();
    }
    Json(Vec::<Value>::with_capacity(limit as usize)).into_response()
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
    warn!(%error, "trend query failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse {
            error: "Internal server error",
        }),
    )
        .into_response()
}
