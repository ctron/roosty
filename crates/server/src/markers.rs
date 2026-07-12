use std::collections::BTreeMap;

use axum::{
    Form, Json, Router,
    extract::{RawQuery, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use roost_db::{LocalTimeline, LocalTimelineMarker};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{auth::AuthenticatedAccount, http::AppState};

/// Build routes for Mastodon-compatible home and notification timeline markers.
pub fn router() -> Router<AppState> {
    Router::new().route("/api/v1/markers", get(markers).post(save_markers))
}

/// Query parameters accepted when fetching saved marker positions.
#[derive(Default, Deserialize)]
struct MarkerQuery {
    #[serde(default)]
    timeline: Vec<String>,
}

/// Form parameters accepted when saving timeline marker positions.
#[derive(Default, Deserialize)]
struct MarkerUpdateParams {
    #[serde(rename = "home[last_read_id]")]
    home_last_read_id: Option<String>,
    #[serde(rename = "notifications[last_read_id]")]
    notifications_last_read_id: Option<String>,
}

/// Mastodon marker representation returned by the marker API.
#[derive(Serialize)]
struct MarkerResponse {
    last_read_id: String,
    version: i64,
    updated_at: String,
}

/// Error response returned for malformed marker requests.
#[derive(Serialize)]
struct ErrorResponse {
    error: &'static str,
}

/// Return the authenticated account's saved positions for requested timelines.
async fn markers(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    RawQuery(query): RawQuery,
) -> Response {
    let query = match marker_query(query.as_deref()) {
        Ok(query) => query,
        Err(()) => return bad_request(),
    };
    let timelines = query
        .timeline
        .iter()
        .filter_map(|timeline| parse_timeline(timeline))
        .collect::<Vec<_>>();

    match roost_db::local_timeline_markers_for_account(&state.db, account.id, &timelines).await {
        Ok(markers) => Json(marker_response_map(markers)).into_response(),
        Err(error) => server_error(error),
    }
}

/// Save one or both timeline positions supplied by the authenticated account.
async fn save_markers(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Form(params): Form<MarkerUpdateParams>,
) -> Response {
    let updates = match marker_updates(params) {
        Ok(updates) => updates,
        Err(()) => return bad_request(),
    };
    let mut markers = Vec::with_capacity(updates.len());
    for (timeline, last_read_id) in updates {
        match roost_db::save_local_timeline_marker(&state.db, account.id, timeline, last_read_id)
            .await
        {
            Ok(marker) => markers.push(marker),
            Err(error) => return server_error(error),
        }
    }

    Json(marker_response_map(markers)).into_response()
}

/// Decode a Mastodon bracket-array query string used by marker requests.
fn marker_query(query: Option<&str>) -> Result<MarkerQuery, ()> {
    let Some(query) = query else {
        return Ok(MarkerQuery::default());
    };

    serde_qs::Config::new()
        .array_format(serde_qs::ArrayFormat::EmptyIndexed)
        .use_form_encoding(true)
        .deserialize_str(query)
        .map_err(|_| ())
}

/// Convert submitted marker form values into typed local timeline updates.
fn marker_updates(params: MarkerUpdateParams) -> Result<Vec<(LocalTimeline, Uuid)>, ()> {
    [
        (LocalTimeline::Home, params.home_last_read_id),
        (
            LocalTimeline::Notifications,
            params.notifications_last_read_id,
        ),
    ]
    .into_iter()
    .filter_map(|(timeline, value)| value.map(|value| (timeline, value)))
    .map(|(timeline, value)| parse_marker_id(&value).map(|id| (timeline, id)))
    .collect()
}

/// Parse a supported timeline name while ignoring names unavailable locally.
fn parse_timeline(value: &str) -> Option<LocalTimeline> {
    match value {
        "home" => Some(LocalTimeline::Home),
        "notifications" => Some(LocalTimeline::Notifications),
        _ => None,
    }
}

/// Parse the UUIDv7 identifiers used by local timeline entries.
fn parse_marker_id(value: &str) -> Result<Uuid, ()> {
    Uuid::parse_str(value.trim()).map_err(|_| ())
}

/// Build a Mastodon marker hash keyed by the timeline wire value.
fn marker_response_map(markers: Vec<LocalTimelineMarker>) -> BTreeMap<String, MarkerResponse> {
    markers
        .into_iter()
        .map(|marker| {
            (
                marker.timeline.as_str().to_owned(),
                MarkerResponse {
                    last_read_id: marker.last_read_id.to_string(),
                    version: marker.version,
                    updated_at: crate::statuses::format_timestamp(marker.updated_at),
                },
            )
        })
        .collect()
}

/// Return a Mastodon-compatible malformed-marker response.
fn bad_request() -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ErrorResponse {
            error: "marker request is invalid",
        }),
    )
        .into_response()
}

/// Return an internal error without exposing database details to the client.
fn server_error(error: roost_core::RoostError) -> Response {
    tracing::warn!(%error, "marker request failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse {
            error: "internal server error",
        }),
    )
        .into_response()
}
