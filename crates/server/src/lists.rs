//! Mastodon-compatible private list management and list timelines.

use axum::{
    Extension, Json, Router,
    body::to_bytes,
    extract::{Path, Query, Request, State},
    http::header,
    response::{IntoResponse, Response},
    routing::get,
};
use roosty_core::{AccountId, StatusId};
use roosty_db::{AddListAccountsResult, ListRepliesPolicy, LocalList};
use serde::{Deserialize, Serialize};
use url::form_urlencoded;
use uuid::Uuid;

use crate::{
    accounts::{RemoteAccountResponse, remote_account_response_on},
    auth::{AccountResponse, AuthenticatedAccount, account_response_on},
    http::{ApiError, ApiResult, AppState, DatabaseContext},
    statuses::{CollectionLink, StatusRenderContext, home_timeline_response, timeline_limit},
};

const DEFAULT_ACCOUNT_LIMIT: u64 = 40;
const MAX_ACCOUNT_LIMIT: u64 = 80;
const MAX_LIST_TITLE_LENGTH: usize = 100;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/lists", get(index).post(create))
        .route(
            "/api/v1/lists/{list_id}",
            get(show).put(update).delete(destroy),
        )
        .route(
            "/api/v1/lists/{list_id}/accounts",
            get(accounts).post(add_accounts).delete(remove_accounts),
        )
        .route("/api/v1/accounts/{account_id}/lists", get(account_lists))
        .route("/api/v1/timelines/list/{list_id}", get(timeline))
}

#[derive(Deserialize)]
struct ListPath {
    list_id: Uuid,
}

#[derive(Deserialize)]
struct AccountPath {
    account_id: Uuid,
}

#[derive(Default, Deserialize)]
struct ListInput {
    title: Option<String>,
    replies_policy: Option<ListRepliesPolicy>,
    exclusive: Option<bool>,
}

#[derive(Default, Deserialize)]
struct AccountIdsInput {
    #[serde(rename = "account_ids[]", alias = "account_ids", default)]
    account_ids: Vec<Uuid>,
}

#[derive(Default, Deserialize)]
struct CursorParams {
    limit: Option<u64>,
    max_id: Option<String>,
    since_id: Option<String>,
    min_id: Option<String>,
}

#[derive(Serialize)]
struct ListResponse {
    id: String,
    title: String,
    replies_policy: ListRepliesPolicy,
    exclusive: bool,
}

impl From<LocalList> for ListResponse {
    fn from(list: LocalList) -> Self {
        Self {
            id: list.id.to_string(),
            title: list.title,
            replies_policy: list.replies_policy,
            exclusive: list.exclusive,
        }
    }
}

#[derive(Serialize)]
#[serde(untagged)]
enum ListAccountResponse {
    Local(Box<AccountResponse>),
    Remote(Box<RemoteAccountResponse>),
}

async fn index(
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
) -> ApiResult<Json<Vec<ListResponse>>> {
    let txn = database.begin_read().await?;
    let lists = roosty_db::local_lists_for_account(&txn, account.id).await?;
    txn.commit().await?;
    Ok(Json(lists.into_iter().map(ListResponse::from).collect()))
}

async fn show(
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<ListPath>,
) -> ApiResult<Json<ListResponse>> {
    let txn = database.begin_read().await?;
    let list = roosty_db::find_owned_local_list(&txn, account.id, path.list_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Record not found".into()))?;
    txn.commit().await?;
    Ok(Json(ListResponse::from(list)))
}

async fn create(
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    request: Request,
) -> ApiResult<Json<ListResponse>> {
    let input: ListInput = parse_input(request)
        .await
        .map_err(|error| ApiError::Unprocessable(error.into()))?;
    let title = validate_title(input.title.as_deref())
        .map_err(|error| ApiError::Unprocessable(error.into()))?;
    let txn = database.begin_write().await?;
    let list = roosty_db::create_local_list(
        &txn,
        account.id,
        title,
        input.replies_policy.unwrap_or(ListRepliesPolicy::List),
        input.exclusive.unwrap_or(false),
    )
    .await?;
    txn.commit().await?;
    Ok(Json(ListResponse::from(list)))
}

async fn update(
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<ListPath>,
    request: Request,
) -> ApiResult<Json<ListResponse>> {
    let input: ListInput = parse_input(request)
        .await
        .map_err(|error| ApiError::Unprocessable(error.into()))?;
    let txn = database.begin_write().await?;
    let current = roosty_db::find_owned_local_list(&txn, account.id, path.list_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Record not found".into()))?;
    let title = match input.title.as_deref() {
        Some(title) => validate_title(Some(title))
            .map_err(|error| ApiError::Unprocessable(error.into()))?
            .to_owned(),
        None => current.title,
    };
    let list = roosty_db::update_local_list(
        &txn,
        account.id,
        path.list_id,
        &title,
        input.replies_policy.unwrap_or(current.replies_policy),
        input.exclusive.unwrap_or(current.exclusive),
    )
    .await?
    .ok_or_else(|| ApiError::NotFound("Record not found".into()))?;
    txn.commit().await?;
    Ok(Json(ListResponse::from(list)))
}

async fn destroy(
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<ListPath>,
) -> ApiResult<Json<serde_json::Value>> {
    let txn = database.begin_write().await?;
    if !roosty_db::delete_local_list(&txn, account.id, path.list_id).await? {
        return Err(ApiError::NotFound("Record not found".into()));
    }
    txn.commit().await?;
    Ok(Json(serde_json::json!({})))
}

async fn account_lists(
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<AccountPath>,
) -> ApiResult<Json<Vec<ListResponse>>> {
    let txn = database.begin_read().await?;
    let lists =
        roosty_db::local_lists_containing_account(&txn, account.id, AccountId(path.account_id))
            .await?;
    txn.commit().await?;
    Ok(Json(lists.into_iter().map(ListResponse::from).collect()))
}

async fn accounts(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<ListPath>,
    Query(params): Query<CursorParams>,
) -> ApiResult<Response> {
    let cursor = collection_cursor(&params)
        .map_err(|()| ApiError::BadRequest("collection cursor is invalid".into()))?;
    let limit = match params.limit {
        Some(0) => None,
        limit => Some(
            limit
                .unwrap_or(DEFAULT_ACCOUNT_LIMIT)
                .clamp(1, MAX_ACCOUNT_LIMIT),
        ),
    };
    let txn = database.begin_snapshot().await?;
    let page = roosty_db::local_list_accounts(&txn, account.id, path.list_id, limit, cursor)
        .await?
        .ok_or_else(|| ApiError::NotFound("Record not found".into()))?;
    let link = limit.and_then(|limit| {
        CollectionLink::new(
            limit,
            page.first_cursor,
            page.last_cursor,
            page.has_more,
            &format!("/api/v1/lists/{}/accounts", path.list_id),
        )
        .header_value()
    });
    let mut responses = Vec::with_capacity(page.items.len());
    for member in page.items {
        responses.push(match member {
            roosty_db::ListAccount::Local(member) => account_response_on(&state, &txn, member)
                .await
                .map(|response| ListAccountResponse::Local(Box::new(response)))?,
            roosty_db::ListAccount::Remote(member) => {
                remote_account_response_on(&state, &txn, member)
                    .await
                    .map(|response| ListAccountResponse::Remote(Box::new(response)))?
            }
        });
    }
    txn.commit().await?;
    let mut response = Json(responses).into_response();
    if let Some(link) = link {
        response.headers_mut().insert(header::LINK, link);
    }
    Ok(response)
}

async fn add_accounts(
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<ListPath>,
    request: Request,
) -> ApiResult<Json<serde_json::Value>> {
    let input = parse_account_ids(request)
        .await
        .map_err(|error| ApiError::Unprocessable(error.into()))?;
    if input.account_ids.is_empty() {
        return Err(ApiError::Unprocessable(
            "account_ids[] must contain at least one account".into(),
        ));
    }
    let txn = database.begin_write().await?;
    let ids = input
        .account_ids
        .into_iter()
        .map(AccountId)
        .collect::<Vec<_>>();
    match roosty_db::add_local_list_accounts(&txn, account.id, path.list_id, &ids).await? {
        AddListAccountsResult::Added => {
            txn.commit().await?;
            Ok(Json(serde_json::json!({})))
        }
        AddListAccountsResult::ListNotFound | AddListAccountsResult::AccountNotFollowed => {
            Err(ApiError::NotFound("Record not found".into()))
        }
        AddListAccountsResult::AlreadyPresent => Err(ApiError::Unprocessable(
            "Validation failed: Account has already been taken".into(),
        )),
    }
}

async fn remove_accounts(
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<ListPath>,
    request: Request,
) -> ApiResult<Json<serde_json::Value>> {
    let input = parse_account_ids(request)
        .await
        .map_err(|error| ApiError::Unprocessable(error.into()))?;
    let txn = database.begin_write().await?;
    let ids = input
        .account_ids
        .into_iter()
        .map(AccountId)
        .collect::<Vec<_>>();
    if !roosty_db::remove_local_list_accounts(&txn, account.id, path.list_id, &ids).await? {
        return Err(ApiError::NotFound("Record not found".into()));
    }
    txn.commit().await?;
    Ok(Json(serde_json::json!({})))
}

async fn timeline(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Path(path): Path<ListPath>,
    Query(params): Query<CursorParams>,
) -> ApiResult<Response> {
    let cursor = timeline_cursor(&params)
        .map_err(|()| ApiError::BadRequest("timeline cursor is invalid".into()))?;
    let limit = timeline_limit(params.limit);
    let txn = database.begin_snapshot().await?;
    let page = roosty_db::local_list_timeline(&txn, account.id, path.list_id, limit, cursor)
        .await?
        .ok_or_else(|| ApiError::NotFound("Record not found".into()))?;
    let context = StatusRenderContext::new(&state, &txn);
    let response = home_timeline_response(
        &context,
        page,
        limit,
        &format!("/api/v1/timelines/list/{}", path.list_id),
        account.id,
    )
    .await;
    txn.commit().await?;
    Ok(response)
}

fn validate_title(title: Option<&str>) -> Result<&str, &'static str> {
    let Some(title) = title.map(str::trim).filter(|title| !title.is_empty()) else {
        return Err("Validation failed: Title can't be blank");
    };
    if title.chars().count() > MAX_LIST_TITLE_LENGTH {
        return Err("Validation failed: Title is too long");
    }
    Ok(title)
}

async fn parse_input<T: for<'de> Deserialize<'de>>(request: Request) -> Result<T, String> {
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

async fn parse_account_ids(request: Request) -> Result<AccountIdsInput, String> {
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
        return serde_json::from_slice(&body)
            .map_err(|error| format!("invalid request body: {error}"));
    }
    let account_ids = form_urlencoded::parse(&body)
        .filter(|(key, _)| key == "account_ids[]" || key == "account_ids")
        .map(|(_, value)| {
            Uuid::parse_str(&value).map_err(|_| "account_ids[] contains an invalid id".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(AccountIdsInput { account_ids })
}

fn collection_cursor(params: &CursorParams) -> Result<roosty_db::CollectionCursor, ()> {
    Ok(roosty_db::CollectionCursor {
        max_id: parse_uuid(params.max_id.as_deref())?,
        since_id: parse_uuid(params.since_id.as_deref())?,
        min_id: parse_uuid(params.min_id.as_deref())?,
    })
}

fn timeline_cursor(params: &CursorParams) -> Result<roosty_db::TimelineCursor, ()> {
    Ok(roosty_db::TimelineCursor {
        max_id: parse_uuid(params.max_id.as_deref())?.map(StatusId),
        since_id: parse_uuid(params.since_id.as_deref())?.map(StatusId),
        min_id: parse_uuid(params.min_id.as_deref())?.map(StatusId),
    })
}

fn parse_uuid(value: Option<&str>) -> Result<Option<Uuid>, ()> {
    value
        .map(|value| Uuid::parse_str(value).map_err(|_| ()))
        .transpose()
}

#[cfg(test)]
mod tests {
    use axum::{body::Body, http::Request};

    use super::{ListResponse, parse_account_ids, validate_title};
    use roosty_core::AccountId;
    use roosty_db::{ListRepliesPolicy, LocalList};
    use time::OffsetDateTime;
    use uuid::Uuid;

    /// List projections preserve Mastodon's string id and closed policy values.
    #[test]
    fn serializes_list_shape() {
        let response = ListResponse::from(LocalList {
            id: Uuid::nil(),
            account_id: AccountId(Uuid::nil()),
            title: "Friends".to_owned(),
            replies_policy: ListRepliesPolicy::Followed,
            exclusive: true,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        });
        let value = serde_json::to_value(response).unwrap();
        assert_eq!(value["id"], Uuid::nil().to_string());
        assert_eq!(value["title"], "Friends");
        assert_eq!(value["replies_policy"], "followed");
        assert_eq!(value["exclusive"], true);
    }

    #[test]
    fn rejects_blank_and_overlong_titles() {
        assert!(validate_title(Some("  ")).is_err());
        assert!(validate_title(Some(&"x".repeat(101))).is_err());
    }

    /// Repeated bracketed form fields match Mastodon's account membership input.
    #[tokio::test]
    async fn parses_repeated_form_account_ids() {
        let first = Uuid::now_v7();
        let second = Uuid::now_v7();
        let request = Request::builder()
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(format!(
                "account_ids%5B%5D={first}&account_ids%5B%5D={second}"
            )))
            .unwrap();
        let input = parse_account_ids(request).await.unwrap();
        assert_eq!(input.account_ids, vec![first, second]);
    }
}
