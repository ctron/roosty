//! Mastodon-compatible featured hashtag management and account projection.

use axum::{
    Json, Router,
    body::to_bytes,
    extract::{Path, Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get},
};
use roosty_core::{AccountId, RoostyError};
use roosty_db::{FeatureTagResult, FeaturedTag};
use sea_orm::TransactionTrait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{auth::AuthenticatedAccount, http::AppState, statuses::TagResponse};

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

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

async fn index(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
) -> Response {
    match roosty_db::local_featured_tags(&state.db, account.id).await {
        Ok(tags) => Json(local_responses(&state, &account.username, tags)).into_response(),
        Err(error) => server_error(error),
    }
}

async fn create(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    request: Request,
) -> Response {
    let input = match parse_input(request).await {
        Ok(input) => input,
        Err(error) => return unprocessable(&error),
    };
    let Some(name) = roosty_db::normalize_featured_tag_name(&input.name) else {
        return unprocessable("Featured tag name is invalid");
    };
    let txn = match state.db.begin().await {
        Ok(txn) => txn,
        Err(error) => return server_error(error.into()),
    };
    let result =
        match roosty_db::feature_local_tag(&txn, account.id, &name, MAX_FEATURED_TAGS).await {
            Ok(result) => result,
            Err(error) => return server_error(error),
        };
    let (tag, created) = match result {
        FeatureTagResult::Featured { tag, created } => (tag, created),
        FeatureTagResult::LimitReached => {
            return unprocessable("You have already featured the maximum number of hashtags");
        }
    };
    if created
        && let Err(error) =
            crate::federation::enqueue_featured_tag_activity(&state, &txn, &account, &tag, true)
                .await
    {
        return server_error(error);
    }
    if let Err(error) = txn.commit().await {
        return server_error(error.into());
    }
    Json(local_response(&state, &account.username, tag)).into_response()
}

async fn destroy(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(featured_tag_id): Path<Uuid>,
) -> Response {
    let txn = match state.db.begin().await {
        Ok(txn) => txn,
        Err(error) => return server_error(error.into()),
    };
    let removed = match roosty_db::unfeature_local_tag(&txn, account.id, featured_tag_id).await {
        Ok(Some(tag)) => tag,
        Ok(None) => return not_found(),
        Err(error) => return server_error(error),
    };
    if let Err(error) =
        crate::federation::enqueue_featured_tag_activity(&state, &txn, &account, &removed, false)
            .await
    {
        return server_error(error);
    }
    if let Err(error) = txn.commit().await {
        return server_error(error.into());
    }
    Json(serde_json::json!({})).into_response()
}

async fn suggestions(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
) -> Response {
    let tags =
        match roosty_db::suggested_featured_tags(&state.db, account.id, MAX_SUGGESTIONS).await {
            Ok(tags) => tags,
            Err(error) => return server_error(error),
        };
    let responses = tags
        .into_iter()
        .map(|tag| TagResponse::new(&state, tag, Vec::new(), None))
        .collect::<Vec<_>>();
    Json(responses).into_response()
}

async fn account_featured_tags(
    State(state): State<AppState>,
    Path(account_id): Path<Uuid>,
) -> Response {
    let account_id = AccountId(account_id);
    match roosty_db::find_local_account_by_id(&state.db, account_id).await {
        Ok(Some(account)) => match roosty_db::local_featured_tags(&state.db, account_id).await {
            Ok(tags) => Json(local_responses(&state, &account.username, tags)).into_response(),
            Err(error) => server_error(error),
        },
        Ok(None) => match roosty_db::find_remote_actor_by_id(&state.db, account_id).await {
            Ok(Some(_actor)) => {
                match roosty_db::remote_featured_tags(&state.db, account_id).await {
                    Ok(tags) => Json(tags.into_iter().map(remote_response).collect::<Vec<_>>())
                        .into_response(),
                    Err(error) => server_error(error),
                }
            }
            Ok(None) => not_found(),
            Err(error) => server_error(error),
        },
        Err(error) => server_error(error),
    }
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

fn unprocessable(description: &str) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(ErrorResponse {
            error: description.to_owned(),
        }),
    )
        .into_response()
}

fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorResponse {
            error: "Record not found".to_owned(),
        }),
    )
        .into_response()
}

fn server_error(error: RoostyError) -> Response {
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
