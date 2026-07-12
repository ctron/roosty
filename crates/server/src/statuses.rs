use axum::{
    Json, Router,
    body::to_bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use roost_core::{AccountId, RoostError, StatusId};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    auth::{AuthenticatedAccount, account_response},
    http::AppState,
};

const DEFAULT_LIMIT: u64 = 20;
const MAX_LIMIT: u64 = 40;
const MAX_STATUS_CHARS: usize = 500;

/// Build routes for local status creation, lookup, deletion, and timelines.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/statuses", post(create_status))
        .route(
            "/api/v1/statuses/{status_id}",
            get(show_status).delete(delete_status),
        )
        .route("/api/v1/timelines/home", get(home_timeline))
        .route("/api/v1/timelines/public", get(public_timeline))
}

#[derive(Debug, thiserror::Error)]
enum StatusInputError {
    #[error("invalid JSON: {0}")]
    Json(serde_json::Error),
    #[error("invalid form body: {0}")]
    Form(serde_urlencoded::de::Error),
    #[error("status must not be empty")]
    Empty,
    #[error("status is too long")]
    TooLong,
    #[error("visibility is invalid")]
    Visibility,
    #[error("status id is invalid")]
    StatusId,
}

#[derive(Deserialize)]
struct StatusPath {
    status_id: Uuid,
}

#[derive(Clone, Copy, Debug)]
struct TimelineQuery {
    limit: u64,
    cursor: roost_db::TimelineCursor,
}

#[derive(Deserialize)]
struct TimelineParams {
    limit: Option<u64>,
    max_id: Option<String>,
    since_id: Option<String>,
    min_id: Option<String>,
}

#[derive(Deserialize)]
struct StatusInput {
    status: String,
    visibility: Option<String>,
    sensitive: Option<bool>,
    spoiler_text: Option<String>,
    language: Option<String>,
    in_reply_to_id: Option<String>,
}

#[derive(Serialize)]
struct StatusResponse {
    id: String,
    created_at: String,
    in_reply_to_id: Option<String>,
    in_reply_to_account_id: Option<String>,
    sensitive: bool,
    spoiler_text: String,
    visibility: String,
    language: Option<String>,
    uri: String,
    url: String,
    content: String,
    account: crate::auth::AccountResponse,
    media_attachments: Vec<Value>,
    mentions: Vec<Value>,
    tags: Vec<Value>,
    emojis: Vec<Value>,
    reblogs_count: u64,
    favourites_count: u64,
    replies_count: u64,
    favourited: bool,
    reblogged: bool,
    muted: bool,
    bookmarked: bool,
    pinned: bool,
    reblog: Option<Value>,
    application: Option<Value>,
}

async fn create_status(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    request: axum::extract::Request,
) -> Response {
    let input = match parse_status_input(request).await {
        Ok(input) => input,
        Err(error) => return bad_request(&error.to_string()),
    };

    let visibility = input
        .visibility
        .unwrap_or_else(|| account.default_visibility.clone());
    if let Err(error) = validate_visibility(&visibility) {
        return bad_request(&error.to_string());
    }
    let in_reply_to_id = match parse_optional_status_id(input.in_reply_to_id.as_deref()) {
        Ok(status_id) => status_id,
        Err(error) => return bad_request(&error.to_string()),
    };
    if let Some(parent_id) = in_reply_to_id {
        match roost_db::find_local_status_by_id(&state.db, parent_id).await {
            Ok(Some(parent)) if can_view_status(&parent, Some(account.id)) => {}
            Ok(Some(_)) | Ok(None) => return bad_request("reply target status does not exist"),
            Err(error) => return server_error(error),
        }
    }

    let new_status = roost_db::NewLocalStatus {
        account_id: account.id,
        content: input.status.trim().to_owned(),
        visibility,
        sensitive: input.sensitive.unwrap_or(account.default_sensitive),
        spoiler_text: input.spoiler_text.unwrap_or_default(),
        language: input.language.or(account.default_language.clone()),
        in_reply_to_id,
    };

    match roost_db::create_local_status(&state.db, new_status).await {
        Ok(status) => match status_response(&state, status, account).await {
            Ok(response) => {
                state.streaming_events.publish_update(&response);
                (StatusCode::OK, Json(response)).into_response()
            }
            Err(error) => server_error(error),
        },
        Err(error) => server_error(error),
    }
}

async fn show_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(path): Path<StatusPath>,
) -> Response {
    let viewer = match optional_account_from_headers(&state, &headers).await {
        Ok(viewer) => viewer,
        Err(response) => return response,
    };
    match roost_db::find_local_status_by_id(&state.db, StatusId(path.status_id)).await {
        Ok(Some(status)) if can_view_status(&status, viewer.as_ref().map(|account| account.id)) => {
            status_with_author_response(&state, status).await
        }
        Ok(None) => not_found(),
        Ok(Some(_)) => not_found(),
        Err(error) => server_error(error),
    }
}

async fn delete_status(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<StatusPath>,
) -> Response {
    match roost_db::delete_owned_local_status(&state.db, StatusId(path.status_id), account.id).await
    {
        Ok(Some(status)) => match status_response(&state, status, account).await {
            Ok(status) => Json(status).into_response(),
            Err(error) => server_error(error),
        },
        Ok(None) => not_found(),
        Err(RoostError::InvalidInput(error)) => forbidden(&error),
        Err(error) => server_error(error),
    }
}

async fn home_timeline(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Query(params): Query<TimelineParams>,
) -> Response {
    let query = match timeline_query(params) {
        Ok(query) => query,
        Err(error) => return bad_request(&error.to_string()),
    };
    match roost_db::home_timeline_for_account(&state.db, account.id, query.limit, query.cursor)
        .await
    {
        Ok(statuses) => {
            timeline_response(&state, statuses, query.limit, "/api/v1/timelines/home").await
        }
        Err(error) => server_error(error),
    }
}

async fn public_timeline(
    State(state): State<AppState>,
    Query(params): Query<TimelineParams>,
) -> Response {
    let query = match timeline_query(params) {
        Ok(query) => query,
        Err(error) => return bad_request(&error.to_string()),
    };
    match roost_db::public_local_timeline(&state.db, query.limit, query.cursor).await {
        Ok(statuses) => {
            timeline_response(&state, statuses, query.limit, "/api/v1/timelines/public").await
        }
        Err(error) => server_error(error),
    }
}

/// Parse either JSON or form-encoded Mastodon status creation input.
async fn parse_status_input(
    request: axum::extract::Request,
) -> Result<StatusInput, StatusInputError> {
    let content_type = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let body = to_bytes(request.into_body(), 1024 * 1024)
        .await
        .map_err(|_| StatusInputError::Empty)?;

    let input: StatusInput = if content_type.contains("application/json") {
        serde_json::from_slice(&body).map_err(StatusInputError::Json)?
    } else {
        serde_urlencoded::from_bytes(&body).map_err(StatusInputError::Form)?
    };

    validate_status_text(&input.status)?;
    Ok(input)
}

/// Validate status text against the current local posting policy.
fn validate_status_text(status: &str) -> Result<(), StatusInputError> {
    let trimmed = status.trim();
    if trimmed.is_empty() {
        return Err(StatusInputError::Empty);
    }
    if trimmed.chars().count() > MAX_STATUS_CHARS {
        return Err(StatusInputError::TooLong);
    }
    Ok(())
}

/// Validate Mastodon visibility values accepted for local statuses.
fn validate_visibility(value: &str) -> Result<(), StatusInputError> {
    match value {
        "public" | "unlisted" | "private" | "direct" => Ok(()),
        _ => Err(StatusInputError::Visibility),
    }
}

/// Parse an optional UUID status id from Mastodon form or JSON input.
fn parse_optional_status_id(value: Option<&str>) -> Result<Option<StatusId>, StatusInputError> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .parse()
                .map(StatusId)
                .map_err(|_| StatusInputError::StatusId)
        })
        .transpose()
}

async fn statuses_response(state: &AppState, statuses: Vec<roost_db::LocalStatus>) -> Response {
    let mut response = Vec::with_capacity(statuses.len());
    for status in statuses {
        match status_with_author(state, status).await {
            Ok(status) => response.push(status),
            Err(error) => return server_error(error),
        }
    }

    Json(response).into_response()
}

async fn timeline_response(
    state: &AppState,
    statuses: Vec<roost_db::LocalStatus>,
    limit: u64,
    path: &str,
) -> Response {
    let link_header = timeline_link_header(&statuses, limit, path);
    let mut response = statuses_response(state, statuses).await;
    if let Some(link_header) = link_header {
        response.headers_mut().insert(header::LINK, link_header);
    }
    response
}

async fn status_with_author_response(state: &AppState, status: roost_db::LocalStatus) -> Response {
    match status_with_author(state, status).await {
        Ok(status) => Json(status).into_response(),
        Err(error) => server_error(error),
    }
}

async fn status_with_author(
    state: &AppState,
    status: roost_db::LocalStatus,
) -> Result<StatusResponse, RoostError> {
    let account = roost_db::find_local_account_by_id(&state.db, status.account_id)
        .await?
        .ok_or_else(|| RoostError::InvalidInput("status author does not exist".to_owned()))?;

    status_response(state, status, account).await
}

async fn status_response(
    state: &AppState,
    status: roost_db::LocalStatus,
    account: roost_db::LocalAccount,
) -> Result<StatusResponse, RoostError> {
    let status_path = format!("@{}/{}", account.username, status.id.0);
    let url = public_url(state, &status_path);
    let in_reply_to_account_id = match status.in_reply_to_id {
        Some(status_id) => roost_db::find_local_status_by_id(&state.db, status_id)
            .await?
            .map(|status| status.account_id.0.to_string()),
        None => None,
    };
    let replies_count = roost_db::count_local_replies(&state.db, status.id).await?;

    Ok(StatusResponse {
        id: status.id.0.to_string(),
        created_at: format_timestamp(status.created_at),
        in_reply_to_id: status.in_reply_to_id.map(|id| id.0.to_string()),
        in_reply_to_account_id,
        sensitive: status.sensitive,
        spoiler_text: status.spoiler_text,
        visibility: status.visibility,
        language: status.language,
        uri: url.clone(),
        url,
        content: status_content_html(&status.content),
        account: account_response(state, account).await?,
        media_attachments: Vec::new(),
        mentions: Vec::new(),
        tags: Vec::new(),
        emojis: Vec::new(),
        reblogs_count: 0,
        favourites_count: 0,
        replies_count,
        favourited: false,
        reblogged: false,
        muted: false,
        bookmarked: false,
        pinned: false,
        reblog: None,
        application: None,
    })
}

fn timeline_limit(limit: Option<u64>) -> u64 {
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

fn timeline_query(params: TimelineParams) -> Result<TimelineQuery, StatusInputError> {
    Ok(TimelineQuery {
        limit: timeline_limit(params.limit),
        cursor: roost_db::TimelineCursor {
            max_id: parse_optional_status_id(params.max_id.as_deref())?,
            since_id: parse_optional_status_id(params.since_id.as_deref())?,
            min_id: parse_optional_status_id(params.min_id.as_deref())?,
        },
    })
}

fn timeline_link_header(
    statuses: &[roost_db::LocalStatus],
    limit: u64,
    path: &str,
) -> Option<HeaderValue> {
    if statuses.len() < limit as usize {
        return None;
    }
    let first = statuses.first()?;
    let last = statuses.last()?;
    let value = format!(
        r#"<{path}?min_id={}>; rel="prev", <{path}?max_id={}>; rel="next""#,
        first.id.0, last.id.0,
    );
    HeaderValue::from_str(&value).ok()
}

fn can_view_status(status: &roost_db::LocalStatus, viewer: Option<AccountId>) -> bool {
    matches!(status.visibility.as_str(), "public" | "unlisted")
        || viewer.is_some_and(|account_id| account_id == status.account_id)
}

async fn optional_account_from_headers(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Option<roost_db::LocalAccount>, Response> {
    let Some(bearer) = bearer_token(headers) else {
        return Ok(None);
    };

    crate::auth::account_from_bearer_token(state, bearer)
        .await
        .map(Some)
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
}

fn status_content_html(content: &str) -> String {
    let escaped = escape_html(content).replace('\n', "<br />");
    format!("<p>{escaped}</p>")
}

fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

fn format_timestamp(timestamp: OffsetDateTime) -> String {
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        timestamp.year(),
        u8::from(timestamp.month()),
        timestamp.day(),
        timestamp.hour(),
        timestamp.minute(),
        timestamp.second(),
        timestamp.millisecond(),
    )
}

fn public_url(state: &AppState, path: &str) -> String {
    state
        .config
        .public_base_url
        .join(path.trim_start_matches('/'))
        .map(|url| url.to_string())
        .unwrap_or_else(|_| format!("{}/{}", state.config.public_base_url, path))
}

fn bad_request(description: &str) -> Response {
    error_response(StatusCode::BAD_REQUEST, "invalid_request", description)
}

fn forbidden(description: &str) -> Response {
    error_response(StatusCode::FORBIDDEN, "forbidden", description)
}

fn not_found() -> Response {
    error_response(StatusCode::NOT_FOUND, "not_found", "status not found")
}

fn server_error(error: RoostError) -> Response {
    error_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        "server_error",
        &error.to_string(),
    )
}

fn error_response(status: StatusCode, error: &str, description: &str) -> Response {
    (
        status,
        Json(json!({
            "error": error,
            "error_description": description,
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use std::{
        net::{IpAddr, Ipv4Addr, SocketAddr},
        time::{Duration as StdDuration, SystemTime, UNIX_EPOCH},
    };

    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{Request, StatusCode, header},
    };
    use postgresql_embedded::{PostgreSQL, SettingsBuilder, V18};
    use roost_core::AccountId;
    use roost_migration::Migrator;
    use sea_orm_migration::MigratorTrait;
    use serde_json::Value;
    use tempfile::TempDir;
    use test_context::{AsyncTestContext, test_context};
    use tower::ServiceExt;

    use super::{escape_html, status_content_html, timeline_limit};
    use crate::{config::Config, http::AppState, password};

    #[test_context(StatusContext)]
    #[tokio::test]
    async fn creating_a_status_populates_status_lookup_and_timelines(context: &mut StatusContext) {
        // This exercises the first real Mastodon client flow after login:
        // post text, fetch the status, and see it in both relevant timelines.
        let token = context.access_token().await;
        let create = context
            .authenticated_json(
                "POST",
                "/api/v1/statuses",
                &token,
                serde_json::json!({"status": "hello <roost>"}),
            )
            .await;

        assert_eq!(create.status(), StatusCode::OK);
        let created = json_body(create).await;
        assert_eq!(created["content"], "<p>hello &lt;roost&gt;</p>");

        let status_id = created["id"].as_str().unwrap();
        let lookup = context.get(&format!("/api/v1/statuses/{status_id}")).await;
        assert_eq!(lookup.status(), StatusCode::OK);
        assert_eq!(json_body(lookup).await["id"], status_id);

        let home = context
            .authenticated_get("/api/v1/timelines/home?limit=30", &token)
            .await;
        assert_eq!(home.status(), StatusCode::OK);
        assert_eq!(json_body(home).await.as_array().unwrap().len(), 1);

        let public = context.get("/api/v1/timelines/public?limit=30").await;
        assert_eq!(public.status(), StatusCode::OK);
        assert_eq!(json_body(public).await.as_array().unwrap().len(), 1);
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    async fn deleting_a_status_removes_it_from_timelines(context: &mut StatusContext) {
        // Deletion is soft in storage but API reads should no longer expose the
        // status through direct lookup or timeline queries.
        let token = context.access_token().await;
        let create = context
            .authenticated_json(
                "POST",
                "/api/v1/statuses",
                &token,
                serde_json::json!({"status": "temporary"}),
            )
            .await;
        let status_id = json_body(create).await["id"].as_str().unwrap().to_owned();

        let delete = context
            .authenticated_empty("DELETE", &format!("/api/v1/statuses/{status_id}"), &token)
            .await;
        assert_eq!(delete.status(), StatusCode::OK);

        assert_eq!(
            context
                .get(&format!("/api/v1/statuses/{status_id}"))
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        let public = context.get("/api/v1/timelines/public").await;
        assert_eq!(json_body(public).await, serde_json::json!([]));
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    async fn status_creation_validates_auth_and_content(context: &mut StatusContext) {
        // Clients should receive normal Mastodon-style validation failures
        // instead of accidentally creating blank rows.
        let token = context.access_token().await;
        let unauthenticated = context
            .json(
                "POST",
                "/api/v1/statuses",
                serde_json::json!({"status": "hello"}),
            )
            .await;
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

        let blank = context
            .authenticated_json(
                "POST",
                "/api/v1/statuses",
                &token,
                serde_json::json!({"status": "   "}),
            )
            .await;
        assert_eq!(blank.status(), StatusCode::BAD_REQUEST);
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    async fn replies_validate_parent_statuses_and_return_reply_metadata(
        context: &mut StatusContext,
    ) {
        // Reply fields are part of the public Mastodon status shape, so parent
        // validation and reply counts must agree with stored relationships.
        let token = context.access_token().await;
        let parent = context.create_status(&token, "parent", None, None).await;
        let parent_id = parent["id"].as_str().unwrap();

        let reply = context
            .create_status(&token, "reply", None, Some(parent_id))
            .await;
        assert_eq!(reply["in_reply_to_id"], parent_id);
        assert_eq!(
            reply["in_reply_to_account_id"],
            context.account_id.0.to_string()
        );

        let parent = context.get(&format!("/api/v1/statuses/{parent_id}")).await;
        assert_eq!(json_body(parent).await["replies_count"], 1);

        let missing_reply = context
            .authenticated_json(
                "POST",
                "/api/v1/statuses",
                &token,
                serde_json::json!({
                    "status": "missing parent",
                    "in_reply_to_id": uuid::Uuid::now_v7().to_string(),
                }),
            )
            .await;
        assert_eq!(missing_reply.status(), StatusCode::BAD_REQUEST);
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    async fn visibility_controls_public_timeline_and_direct_status_reads(
        context: &mut StatusContext,
    ) {
        // Until follow graph support exists, private and direct statuses are
        // owner-only while public and unlisted statuses remain URL-readable.
        let token = context.access_token().await;
        context
            .create_status(&token, "public", Some("public"), None)
            .await;
        let unlisted = context
            .create_status(&token, "unlisted", Some("unlisted"), None)
            .await;
        let private = context
            .create_status(&token, "private", Some("private"), None)
            .await;
        let direct = context
            .create_status(&token, "direct", Some("direct"), None)
            .await;

        let public = json_body(context.get("/api/v1/timelines/public").await).await;
        assert_eq!(public.as_array().unwrap().len(), 1);
        assert_eq!(public[0]["visibility"], "public");

        let home = json_body(
            context
                .authenticated_get("/api/v1/timelines/home", &token)
                .await,
        )
        .await;
        assert_eq!(home.as_array().unwrap().len(), 4);

        let unlisted_id = unlisted["id"].as_str().unwrap();
        assert_eq!(
            context
                .get(&format!("/api/v1/statuses/{unlisted_id}"))
                .await
                .status(),
            StatusCode::OK
        );

        for status in [private, direct] {
            let status_id = status["id"].as_str().unwrap();
            assert_eq!(
                context
                    .get(&format!("/api/v1/statuses/{status_id}"))
                    .await
                    .status(),
                StatusCode::NOT_FOUND
            );
            assert_eq!(
                context
                    .authenticated_get(&format!("/api/v1/statuses/{status_id}"), &token)
                    .await
                    .status(),
                StatusCode::OK
            );
        }
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    async fn timeline_cursors_page_through_local_statuses(context: &mut StatusContext) {
        // Cursor support is what lets Mastodon clients incrementally load
        // timeline pages without relying on offset pagination.
        let token = context.access_token().await;
        let first = context.create_status(&token, "first", None, None).await;
        let second = context.create_status(&token, "second", None, None).await;
        let third = context.create_status(&token, "third", None, None).await;

        let page = context.get("/api/v1/timelines/public?limit=2").await;
        assert!(page.headers().get(header::LINK).is_some());
        let body = json_body(page).await;
        assert_eq!(body.as_array().unwrap().len(), 2);
        assert_eq!(body[0]["id"], third["id"]);
        assert_eq!(body[1]["id"], second["id"]);

        let second_id = second["id"].as_str().unwrap();
        let older = json_body(
            context
                .get(&format!("/api/v1/timelines/public?max_id={second_id}"))
                .await,
        )
        .await;
        assert_eq!(older.as_array().unwrap().len(), 1);
        assert_eq!(older[0]["id"], first["id"]);

        let newer = json_body(
            context
                .get(&format!("/api/v1/timelines/public?since_id={second_id}"))
                .await,
        )
        .await;
        assert_eq!(newer.as_array().unwrap().len(), 1);
        assert_eq!(newer[0]["id"], third["id"]);
    }

    #[test_context(StatusContext)]
    #[tokio::test]
    async fn account_responses_include_local_status_metadata(context: &mut StatusContext) {
        // Status counts are client-visible account metadata and should ignore
        // soft-deleted statuses.
        let token = context.access_token().await;
        context.create_status(&token, "kept", None, None).await;
        let deleted = context.create_status(&token, "deleted", None, None).await;
        let deleted_id = deleted["id"].as_str().unwrap();
        assert_eq!(
            context
                .authenticated_empty("DELETE", &format!("/api/v1/statuses/{deleted_id}"), &token)
                .await
                .status(),
            StatusCode::OK
        );

        let credentials = context
            .authenticated_get("/api/v1/accounts/verify_credentials", &token)
            .await;
        let body = json_body(credentials).await;
        assert_eq!(body["statuses_count"], 1);
        assert!(body["last_status_at"].as_str().is_some());
    }

    #[test]
    fn status_helpers_match_mastodon_compatibility_shapes() {
        // These helpers are intentionally tiny, but they define externally
        // visible timeline sizing and HTML escaping behavior.
        assert_eq!(timeline_limit(None), 20);
        assert_eq!(timeline_limit(Some(0)), 1);
        assert_eq!(timeline_limit(Some(100)), 40);
        assert_eq!(escape_html("<&>'\""), "&lt;&amp;&gt;&#39;&quot;");
        assert_eq!(status_content_html("a\nb"), "<p>a<br />b</p>");
    }

    struct StatusContext {
        postgresql: PostgreSQL,
        db: roost_db::DbConnection,
        database_name: String,
        config: Config,
        account_id: AccountId,
        application_id: uuid::Uuid,
        _temp_dir: TempDir,
    }

    impl AsyncTestContext for StatusContext {
        async fn setup() -> Self {
            let temp_dir = tempfile::Builder::new()
                .prefix("roost-status-")
                .tempdir()
                .unwrap();
            let install_cache_root = std::env::var_os("CARGO_TARGET_TMPDIR")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::env::temp_dir().join("roost-target-tmp"));
            let install_cache = install_cache_root.join("embedded-postgres").join("install");
            let database_name = unique_name();
            let data_dir = temp_dir.path().join("data").join(&database_name);
            let password_file = temp_dir
                .path()
                .join("passwords")
                .join(format!("{database_name}.pgpass"));

            if let Some(parent) = password_file.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }

            let settings = SettingsBuilder::new()
                .version((*V18).clone())
                .installation_dir(install_cache)
                .data_dir(&data_dir)
                .password_file(password_file)
                .timeout(Some(StdDuration::from_secs(30)))
                .build();
            let mut postgresql = PostgreSQL::new(settings);

            postgresql.setup().await.unwrap();
            postgresql.start().await.unwrap();
            postgresql.create_database(&database_name).await.unwrap();

            let database_url = postgresql.settings().url(&database_name);
            let db = roost_db::connect(&database_url).await.unwrap();
            Migrator::up(&db, None).await.unwrap();

            let password_hash = password::hash_password("password").unwrap();
            let account_id = AccountId(
                roost_db::create_bootstrap_admin(&db, "admin", "admin@example.com", &password_hash)
                    .await
                    .unwrap(),
            );
            let (application, _secret) = roost_db::create_oauth_application(
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
                instance_name: "Roost Test".to_owned(),
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

    impl StatusContext {
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
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
        }

        async fn json(
            &self,
            method: &str,
            uri: &str,
            body: serde_json::Value,
        ) -> axum::http::Response<Body> {
            self.request(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
        }

        async fn authenticated_json(
            &self,
            method: &str,
            uri: &str,
            token: &str,
            body: serde_json::Value,
        ) -> axum::http::Response<Body> {
            self.request(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
        }

        async fn authenticated_empty(
            &self,
            method: &str,
            uri: &str,
            token: &str,
        ) -> axum::http::Response<Body> {
            self.request(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
        }

        async fn access_token(&self) -> String {
            roost_db::create_access_token(
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

        async fn create_status(
            &self,
            token: &str,
            status: &str,
            visibility: Option<&str>,
            in_reply_to_id: Option<&str>,
        ) -> Value {
            let mut body = serde_json::json!({ "status": status });
            if let Some(visibility) = visibility {
                body["visibility"] = serde_json::json!(visibility);
            }
            if let Some(in_reply_to_id) = in_reply_to_id {
                body["in_reply_to_id"] = serde_json::json!(in_reply_to_id);
            }

            let response = self
                .authenticated_json("POST", "/api/v1/statuses", token, body)
                .await;
            assert_eq!(response.status(), StatusCode::OK);
            json_body(response).await
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

        format!("roost_status_{}_{}", std::process::id(), timestamp)
    }
}
