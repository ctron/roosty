use std::collections::BTreeMap;

use axum::{Extension, Form, Json, Router, extract::RawQuery, routing::get};
use roosty_db::{LocalTimeline, LocalTimelineMarker};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    auth::AuthenticatedAccount,
    http::{ApiError, ApiResult, AppState, DatabaseContext},
};

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

/// Return the authenticated account's saved positions for requested timelines.
async fn markers(
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    RawQuery(query): RawQuery,
) -> ApiResult<Json<BTreeMap<String, MarkerResponse>>> {
    let query = marker_query(query.as_deref())
        .map_err(|()| ApiError::BadRequest("marker request is invalid".into()))?;
    let timelines = query
        .timeline
        .iter()
        .filter_map(|timeline| parse_timeline(timeline))
        .collect::<Vec<_>>();

    let txn = database.begin_read().await?;
    let markers =
        roosty_db::local_timeline_markers_for_account(&txn, account.id, &timelines).await?;
    txn.commit().await?;
    Ok(Json(marker_response_map(markers)))
}

/// Save one or both timeline positions supplied by the authenticated account.
async fn save_markers(
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Form(params): Form<MarkerUpdateParams>,
) -> ApiResult<Json<BTreeMap<String, MarkerResponse>>> {
    let updates = marker_updates(params)
        .map_err(|()| ApiError::BadRequest("marker request is invalid".into()))?;
    let txn = database.begin_write().await?;
    let mut markers = Vec::with_capacity(updates.len());
    for (timeline, last_read_id) in updates {
        markers.push(
            roosty_db::save_local_timeline_marker(&txn, account.id, timeline, last_read_id).await?,
        );
    }
    txn.commit().await?;

    Ok(Json(marker_response_map(markers)))
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
