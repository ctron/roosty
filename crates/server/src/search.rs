use axum::{
    Json, Router,
    extract::{Query, State},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    auth::{AuthenticatedAccount, account_response},
    http::AppState,
};

const DEFAULT_SEARCH_LIMIT: u64 = 20;
const MAX_SEARCH_LIMIT: u64 = 40;
const MAX_ACCOUNT_SEARCH_LIMIT: u64 = 80;

/// Build routes for Mastodon-compatible search and account autocomplete.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v2/search", get(search))
        .route("/api/v1/accounts/search", get(account_search))
}

#[derive(Deserialize)]
struct SearchParams {
    q: Option<String>,
    #[serde(rename = "type")]
    search_type: Option<String>,
    limit: Option<u64>,
    offset: Option<u64>,
    resolve: Option<bool>,
    following: Option<bool>,
}

#[derive(Serialize)]
struct SearchResponse {
    accounts: Vec<crate::auth::AccountResponse>,
    statuses: Vec<Value>,
    hashtags: Vec<crate::statuses::TagResponse>,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: &'static str,
    error_description: String,
}

async fn search(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Query(params): Query<SearchParams>,
) -> Response {
    let accounts = if matches!(params.search_type.as_deref(), None | Some("accounts")) {
        search_accounts(&state, account.id, &params, MAX_SEARCH_LIMIT).await
    } else {
        Ok(Vec::new())
    };
    let hashtags = if matches!(params.search_type.as_deref(), None | Some("hashtags")) {
        search_hashtags(&state, &params).await
    } else {
        Ok(Vec::new())
    };

    match (accounts, hashtags) {
        (Ok(accounts), Ok(hashtags)) => Json(SearchResponse {
            accounts,
            statuses: Vec::new(),
            hashtags,
        })
        .into_response(),
        (Err(error), _) | (_, Err(error)) => server_error(error),
    }
}

async fn account_search(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    Query(params): Query<SearchParams>,
) -> Response {
    match search_accounts(&state, account.id, &params, MAX_ACCOUNT_SEARCH_LIMIT).await {
        Ok(accounts) => Json(accounts).into_response(),
        Err(error) => server_error(error),
    }
}

/// Search local accounts and convert them to Mastodon account responses.
async fn search_accounts(
    state: &AppState,
    account_id: roost_core::AccountId,
    params: &SearchParams,
    max_limit: u64,
) -> roost_core::Result<Vec<crate::auth::AccountResponse>> {
    let _resolve = params.resolve.unwrap_or(false);
    let _following = params.following.unwrap_or(false);
    let Some(query) = normalized_local_query(state, params.q.as_deref()) else {
        return Ok(Vec::new());
    };
    let limit = params
        .limit
        .unwrap_or(DEFAULT_SEARCH_LIMIT)
        .clamp(1, max_limit);
    let offset = params.offset.unwrap_or(0);
    let accounts =
        roost_db::search_local_accounts(&state.db, account_id, &query, limit, offset).await?;
    let mut responses = Vec::with_capacity(accounts.len());

    for account in accounts {
        responses.push(account_response(state, account).await?);
    }

    Ok(responses)
}

/// Search local hashtags and include recent usage history in Mastodon tag responses.
async fn search_hashtags(
    state: &AppState,
    params: &SearchParams,
) -> roost_core::Result<Vec<crate::statuses::TagResponse>> {
    let Some(query) = normalized_tag_query(params.q.as_deref()) else {
        return Ok(Vec::new());
    };
    let limit = params
        .limit
        .unwrap_or(DEFAULT_SEARCH_LIMIT)
        .clamp(1, MAX_SEARCH_LIMIT);
    let offset = params.offset.unwrap_or(0);
    let tags = roost_db::search_local_tags(&state.db, &query, limit, offset).await?;
    let mut responses = Vec::with_capacity(tags.len());
    for tag in tags {
        let history = roost_db::local_tag_history(&state.db, tag.id).await?;
        responses.push(crate::statuses::TagResponse::new(state, tag, history, None));
    }

    Ok(responses)
}

/// Normalize local mention-style search terms and reject remote account queries.
fn normalized_local_query(state: &AppState, query: Option<&str>) -> Option<String> {
    let trimmed = query?.trim().trim_start_matches('@');
    if trimmed.is_empty() {
        return None;
    }

    if let Some((username, domain)) = trimmed.split_once('@') {
        let local_host = state.config.public_base_url.host_str()?;
        if domain != local_host {
            return None;
        }
        return non_empty(username);
    }

    non_empty(trimmed)
}

/// Normalize hashtag search terms accepted by Mastodon search.
fn normalized_tag_query(query: Option<&str>) -> Option<String> {
    let trimmed = query?.trim().trim_start_matches('#').to_lowercase();
    non_empty(&trimmed)
}

fn non_empty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

fn server_error(error: roost_core::RoostError) -> Response {
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse {
            error: "server_error",
            error_description: error.to_string(),
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use std::{
        net::{IpAddr, Ipv4Addr, SocketAddr},
        time::{SystemTime, UNIX_EPOCH},
    };

    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{Request, StatusCode, header},
    };
    use postgresql_embedded::PostgreSQL;
    use roost_core::AccountId;
    use roost_migration::Migrator;
    use sea_orm_migration::MigratorTrait;
    use serde_json::Value;
    use tempfile::TempDir;
    use test_context::{AsyncTestContext, test_context};
    use tower::ServiceExt;

    use crate::{config::Config, http::AppState, password};

    #[test_context(SearchContext)]
    #[tokio::test]
    /// Verifies that v2 search returns local account autocomplete results.
    async fn search_returns_local_accounts(context: &mut SearchContext) {
        let token = context.access_token().await;
        context
            .create_account("alice", "alice@example.com", "Alice Example")
            .await;
        context
            .create_account("bob", "bob@example.com", "Bob Example")
            .await;

        let response = context
            .authenticated_get("/api/v2/search?type=accounts&q=@alice", &token)
            .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        assert_eq!(body["accounts"].as_array().unwrap().len(), 1);
        assert_eq!(body["accounts"][0]["username"], "alice");
        assert_eq!(body["statuses"], serde_json::json!([]));
        assert_eq!(body["hashtags"], serde_json::json!([]));
    }

    #[test_context(SearchContext)]
    #[tokio::test]
    /// Given stored local hashtags, when v2 search requests hashtags, then Mastodon tag results include usage history.
    async fn search_returns_local_hashtags(context: &mut SearchContext) {
        let token = context.access_token().await;
        let status = roost_db::create_local_status(
            &context.db,
            roost_db::NewLocalStatus {
                account_id: context.account_id,
                content: "searchable #RoostTag".to_owned(),
                visibility: "public".to_owned(),
                sensitive: false,
                spoiler_text: String::new(),
                language: None,
                in_reply_to_id: None,
            },
        )
        .await
        .unwrap();
        roost_db::replace_local_status_tags(&context.db, status.id, &["roosttag".to_owned()])
            .await
            .unwrap();

        let response = context
            .authenticated_get("/api/v2/search?type=hashtags&q=%23roost", &token)
            .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        assert_eq!(body["accounts"], serde_json::json!([]));
        assert_eq!(body["statuses"], serde_json::json!([]));
        assert_eq!(
            body["hashtags"],
            serde_json::json!([{
                "id": body["hashtags"][0]["id"],
                "name": "roosttag",
                "url": "https://roost.localhost:4000/tags/roosttag",
                "history": [{
                    "day": body["hashtags"][0]["history"][0]["day"],
                    "uses": "1",
                    "accounts": "1"
                }]
            }])
        );
    }

    #[test_context(SearchContext)]
    #[tokio::test]
    /// Verifies that account search supports display names and requires auth.
    async fn account_search_matches_display_name(context: &mut SearchContext) {
        let token = context.access_token().await;
        context
            .create_account("alice", "alice@example.com", "Alice Example")
            .await;

        let response = context
            .authenticated_get("/api/v1/accounts/search?q=Example", &token)
            .await;
        let unauthorized = context.get("/api/v1/accounts/search?q=Example").await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(json_body(response).await[0]["username"], "alice");
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    }

    struct SearchContext {
        postgresql: PostgreSQL,
        db: roost_db::DbConnection,
        database_name: String,
        config: Config,
        account_id: AccountId,
        application_id: uuid::Uuid,
        _temp_dir: TempDir,
    }

    impl AsyncTestContext for SearchContext {
        async fn setup() -> Self {
            let temp_dir = tempfile::Builder::new()
                .prefix("roost-search-")
                .tempdir()
                .unwrap();
            let database_name = unique_name();
            let data_dir = temp_dir.path().join("data").join(&database_name);
            let password_file = temp_dir
                .path()
                .join("passwords")
                .join(format!("{database_name}.pgpass"));

            if let Some(parent) = password_file.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }

            let settings = crate::test_postgres::settings(&data_dir, password_file);
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
                public_base_url: "https://roost.localhost:4000".parse().unwrap(),
                listen_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4000),
                infra_listen_addr: None,
                session_secret: "test-session-secret-change-me-000".to_owned(),
                token_pepper: "test-token-pepper-change-me-0000".to_owned(),
                object_storage_backend: "local".to_owned(),
                media_root: "./media".to_owned(),
                registration_mode: "closed".to_owned(),
                federation_enabled: false,
                federation_key_encryption_secret: None,
                federation_allowed_domains: Vec::new(),
                federation_blocked_domains: Vec::new(),
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

    impl SearchContext {
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

        async fn create_account(&self, username: &str, email: &str, display_name: &str) {
            let password_hash = password::hash_password("password").unwrap();
            let account_id = AccountId(
                roost_db::create_local_account(&self.db, username, email, &password_hash)
                    .await
                    .unwrap(),
            );
            roost_db::update_local_account_settings(
                &self.db,
                account_id,
                roost_db::LocalAccountSettingsUpdate {
                    display_name: Some(display_name.to_owned()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
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

        format!("roost_search_{}_{}", std::process::id(), timestamp)
    }
}
