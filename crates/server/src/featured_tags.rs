//! Mastodon-compatible featured hashtag management and account projection.

use axum::{
    Extension, Json, Router,
    body::to_bytes,
    extract::{Path, Request, State},
    http::header,
    response::{IntoResponse, Response},
    routing::{delete, get},
};
use roosty_core::AccountId;
use roosty_db::{FeatureTagResult, FeaturedTag};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    auth::AuthenticatedAccount,
    http::{ApiError, ApiResult, AppState, DatabaseContext},
    statuses::TagResponse,
};

pub(crate) const MAX_FEATURED_TAGS: u64 = 10;
const MAX_SUGGESTIONS: u64 = 10;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/featured_tags", get(index).post(create))
        .route("/api/v1/featured_tags/{featured_tag_id}", delete(destroy))
        .route("/api/v1/featured_tags/suggestions", get(suggestions))
        .route(
            "/api/v1/accounts/{account_id}/featured_tags",
            get(account_featured_tags),
        )
}

#[derive(Deserialize)]
struct FeaturedTagInput {
    name: String,
}

#[derive(Serialize)]
struct FeaturedTagResponse {
    id: String,
    name: String,
    url: String,
    statuses_count: String,
    last_status_at: Option<String>,
}

async fn index(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
) -> ApiResult<Json<Vec<FeaturedTagResponse>>> {
    let txn = database.begin_read().await?;
    let tags = roosty_db::local_featured_tags(&txn, account.id).await?;
    txn.commit().await?;
    Ok(Json(local_responses(&state, &account.username, tags)))
}

async fn create(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    request: Request,
) -> ApiResult<Json<FeaturedTagResponse>> {
    let input = parse_input(request)
        .await
        .map_err(|error| ApiError::Unprocessable(error.into()))?;
    let Some(name) = roosty_db::normalize_featured_tag_name(&input.name) else {
        return Err(ApiError::Unprocessable(
            "Featured tag name is invalid".into(),
        ));
    };
    let txn = database.begin_write().await?;
    let result = roosty_db::feature_local_tag(&txn, account.id, &name, MAX_FEATURED_TAGS).await?;
    let (tag, created) = match result {
        FeatureTagResult::Featured { tag, created } => (tag, created),
        FeatureTagResult::LimitReached => {
            return Err(ApiError::Unprocessable(
                "You have already featured the maximum number of hashtags".into(),
            ));
        }
    };
    if created {
        crate::federation::enqueue_featured_tag_activity(&state, &txn, &account, &tag, true)
            .await?;
    }
    txn.commit().await?;
    Ok(Json(local_response(&state, &account.username, tag)))
}

async fn destroy(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(featured_tag_id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let txn = database.begin_write().await?;
    let removed = roosty_db::unfeature_local_tag(&txn, account.id, featured_tag_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Record not found".into()))?;
    crate::federation::enqueue_featured_tag_activity(&state, &txn, &account, &removed, false)
        .await?;
    txn.commit().await?;
    Ok(Json(serde_json::json!({})))
}

async fn suggestions(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
) -> ApiResult<Json<Vec<TagResponse>>> {
    let txn = database.begin_read().await?;
    let tags = roosty_db::suggested_featured_tags(&txn, account.id, MAX_SUGGESTIONS).await?;
    txn.commit().await?;
    let responses = tags
        .into_iter()
        .map(|tag| TagResponse::new(&state, tag, Vec::new(), None))
        .collect::<Vec<_>>();
    Ok(Json(responses))
}

async fn account_featured_tags(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    Path(account_id): Path<Uuid>,
) -> ApiResult<Response> {
    let account_id = AccountId(account_id);
    let txn = database.begin_snapshot().await?;
    let response = match roosty_db::find_local_account_by_id(&txn, account_id).await? {
        Some(account) => Json(local_responses(
            &state,
            &account.username,
            roosty_db::local_featured_tags(&txn, account_id).await?,
        ))
        .into_response(),
        None => {
            roosty_db::find_remote_actor_by_id(&txn, account_id)
                .await?
                .ok_or_else(|| ApiError::NotFound("Record not found".into()))?;
            Json(
                roosty_db::remote_featured_tags(&txn, account_id)
                    .await?
                    .into_iter()
                    .map(remote_response)
                    .collect::<Vec<_>>(),
            )
            .into_response()
        }
    };
    txn.commit().await?;
    Ok(response)
}

fn local_responses(
    state: &AppState,
    username: &str,
    tags: Vec<FeaturedTag>,
) -> Vec<FeaturedTagResponse> {
    tags.into_iter()
        .map(|tag| local_response(state, username, tag))
        .collect()
}

fn local_response(state: &AppState, username: &str, tag: FeaturedTag) -> FeaturedTagResponse {
    let url = public_url(state, &format!("@{username}/tagged/{}", tag.name));
    response(tag, url)
}

fn remote_response(tag: FeaturedTag) -> FeaturedTagResponse {
    let url = tag.href.clone().unwrap_or_default();
    response(tag, url)
}

fn response(tag: FeaturedTag, url: String) -> FeaturedTagResponse {
    FeaturedTagResponse {
        id: tag.id.to_string(),
        name: tag.name,
        url,
        statuses_count: tag.statuses_count.to_string(),
        last_status_at: tag
            .last_status_at
            .map(|timestamp| timestamp.date().to_string()),
    }
}

async fn parse_input(request: Request) -> Result<FeaturedTagInput, String> {
    let content_type = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let body = to_bytes(request.into_body(), 1024 * 1024)
        .await
        .map_err(|error| format!("invalid request body: {error}"))?;
    if content_type.contains("application/json") {
        serde_json::from_slice(&body).map_err(|error| format!("invalid request body: {error}"))
    } else {
        serde_urlencoded::from_bytes(&body)
            .map_err(|error| format!("invalid request body: {error}"))
    }
}

fn public_url(state: &AppState, path: &str) -> String {
    state
        .config
        .public_base_url
        .join(path.trim_start_matches('/'))
        .map(|url| url.to_string())
        .unwrap_or_else(|_| format!("{}/{}", state.config.public_base_url, path))
}

#[cfg(test)]
mod tests {
    use time::{OffsetDateTime, format_description::well_known::Rfc3339};

    use super::{FeaturedTag, remote_response};

    /// FeaturedTag responses retain Mastodon's string count and date-only last-use shape.
    #[test]
    fn serializes_remote_featured_tag_shape() {
        let response = remote_response(FeaturedTag {
            id: uuid::Uuid::nil(),
            name: "rust".to_owned(),
            href: Some("https://remote.test/@alice/tagged/rust".to_owned()),
            statuses_count: 3,
            last_status_at: Some(OffsetDateTime::parse("2026-07-18T12:00:00Z", &Rfc3339).unwrap()),
            created_at: OffsetDateTime::UNIX_EPOCH,
        });
        let value = serde_json::to_value(response).unwrap();

        assert_eq!(value["id"], uuid::Uuid::nil().to_string());
        assert_eq!(value["name"], "rust");
        assert_eq!(value["statuses_count"], "3");
        assert_eq!(value["last_status_at"], "2026-07-18");
        assert_eq!(value["url"], "https://remote.test/@alice/tagged/rust");
    }
}
