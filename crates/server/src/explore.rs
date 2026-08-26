//! Mastodon-compatible public Explore and trend discovery.

use axum::{
    Extension, Json, Router,
    extract::{Query, State, rejection::QueryRejection},
    routing::get,
};
use roosty_db::{LocalTagHistory, PreviewCard};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    auth::OptionalAuthenticatedAccount,
    http::{ApiError, ApiResult, AppState, DatabaseContext},
    media::media_url,
    statuses::{StatusRenderContext, StatusResponse, TagResponse, trending_status_models},
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
    Extension(database): Extension<DatabaseContext>,
    OptionalAuthenticatedAccount(viewer): OptionalAuthenticatedAccount,
    query: Result<Query<TrendParams>, QueryRejection>,
) -> ApiResult<Json<Vec<StatusResponse>>> {
    let Query(params) =
        query.map_err(|_| ApiError::BadRequest("Invalid request parameters".into()))?;
    let limit = params
        .limit
        .unwrap_or(DEFAULT_STATUS_LIMIT)
        .clamp(1, MAX_STATUS_LIMIT);
    let offset = params.offset.unwrap_or_default();
    if i64::try_from(offset).is_err() {
        return Err(ApiError::BadRequest("Invalid request parameters".into()));
    }

    let txn = database.begin_snapshot().await?;
    let trends = roosty_db::trending_statuses(&txn, limit, offset).await?;
    let context = StatusRenderContext::new(&state, &txn);
    let statuses =
        trending_status_models(&context, trends, viewer.as_ref().map(|account| account.id)).await?;
    txn.commit().await?;
    Ok(Json(statuses))
}

async fn trending_links(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    query: Result<Query<TrendParams>, QueryRejection>,
) -> ApiResult<Json<Vec<PreviewCardResponse>>> {
    let Query(params) =
        query.map_err(|_| ApiError::BadRequest("Invalid request parameters".into()))?;
    let limit = params
        .limit
        .unwrap_or(DEFAULT_TAG_LIMIT)
        .clamp(1, MAX_TAG_LIMIT);
    let offset = params.offset.unwrap_or_default();
    if i64::try_from(offset).is_err() {
        return Err(ApiError::BadRequest("Invalid request parameters".into()));
    }
    let txn = database.begin_snapshot().await?;
    let links = roosty_db::trending_links(&txn, limit, offset).await?;
    txn.commit().await?;
    Ok(Json(
        links
            .into_iter()
            .map(|link| PreviewCardResponse::new(&state, link.card, Some(link.history)))
            .collect(),
    ))
}

/// Mastodon preview-card projection, with optional trend history.
#[derive(Clone, Serialize)]
pub(crate) struct PreviewCardResponse {
    url: String,
    title: String,
    description: String,
    #[serde(rename = "type")]
    card_type: &'static str,
    authors: Vec<PreviewCardAuthorResponse>,
    author_name: String,
    author_url: String,
    provider_name: String,
    provider_url: String,
    html: &'static str,
    width: u32,
    height: u32,
    image: Option<String>,
    embed_url: &'static str,
    blurhash: Option<String>,
    missing_attribution: Option<bool>,
    published_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    history: Option<Vec<HistoryResponse>>,
}

#[derive(Clone, Serialize)]
struct PreviewCardAuthorResponse {
    name: String,
    url: String,
    account: Option<Value>,
}

#[derive(Clone, Serialize)]
struct HistoryResponse {
    day: String,
    uses: String,
    accounts: String,
}

impl PreviewCardResponse {
    pub(crate) fn new(
        state: &AppState,
        card: PreviewCard,
        history: Option<Vec<LocalTagHistory>>,
    ) -> Self {
        let authors = (!card.author_name.is_empty())
            .then(|| PreviewCardAuthorResponse {
                name: card.author_name.clone(),
                url: card.author_url.clone(),
                account: None,
            })
            .into_iter()
            .collect();
        Self {
            url: card.url,
            title: card.title,
            description: card.description,
            card_type: "link",
            authors,
            author_name: card.author_name,
            author_url: card.author_url,
            provider_name: card.provider_name,
            provider_url: card.provider_url,
            html: "",
            width: card.image_width,
            height: card.image_height,
            image: card
                .image_file_path
                .as_deref()
                .map(|path| media_url(state, path)),
            embed_url: "",
            blurhash: card.blurhash,
            missing_attribution: None,
            published_at: card
                .published_at
                .map(|value| value.unix_timestamp().to_string()),
            history: history.map(|history| {
                history
                    .into_iter()
                    .map(|bucket| HistoryResponse {
                        day: bucket.day.to_string(),
                        uses: bucket.uses.to_string(),
                        accounts: bucket.accounts.to_string(),
                    })
                    .collect()
            }),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct TrendParams {
    limit: Option<u64>,
    offset: Option<u64>,
}

async fn trending_tags(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    OptionalAuthenticatedAccount(account): OptionalAuthenticatedAccount,
    query: Result<Query<TrendParams>, QueryRejection>,
) -> ApiResult<Json<Vec<TagResponse>>> {
    let Query(params) =
        query.map_err(|_| ApiError::BadRequest("Invalid request parameters".into()))?;
    let limit = params
        .limit
        .unwrap_or(DEFAULT_TAG_LIMIT)
        .clamp(1, MAX_TAG_LIMIT);
    let offset = params.offset.unwrap_or_default();
    if i64::try_from(offset).is_err() {
        return Err(ApiError::BadRequest("Invalid request parameters".into()));
    }

    let txn = database.begin_snapshot().await?;
    let trends = roosty_db::trending_tags(&txn, limit, offset).await?;
    let relationships = match account {
        Some(account) => Some(
            roosty_db::local_tag_relationships(
                &txn,
                account.id,
                &trends.iter().map(|trend| trend.tag.id).collect::<Vec<_>>(),
            )
            .await?,
        ),
        None => None,
    };
    let response = trends
        .into_iter()
        .map(|trend| {
            let relationship = relationships
                .as_ref()
                .and_then(|relationships| relationships.get(&trend.tag.id).copied());
            TagResponse::new(&state, trend.tag, trend.history, relationship)
        })
        .collect();
    txn.commit().await?;
    Ok(Json(response))
}
