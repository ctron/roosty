use axum::{
    Json, Router,
    extract::{Query, State},
    response::{IntoResponse, Response},
    routing::get,
};
use roosty_core::{AccountId, FederationDiscoveryError, Result, RoostyError};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    accounts::{RemoteAccountResponse, remote_account_response},
    auth::{AccountResponse, AuthenticatedAccount, OptionalAuthenticatedAccount, account_response},
    http::AppState,
    statuses::TagResponse,
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
    search_type: Option<SearchType>,
    limit: Option<u64>,
    offset: Option<u64>,
    resolve: Option<bool>,
    following: Option<bool>,
}

/// Mastodon search result categories, retaining unknown extensions as unsupported.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum SearchType {
    Accounts,
    Hashtags,
    Statuses,
    #[serde(other)]
    Other,
}

#[derive(Serialize)]
struct SearchResponse {
    accounts: Vec<SearchAccountResponse>,
    statuses: Vec<Value>,
    hashtags: Vec<TagResponse>,
}

/// Untagged Mastodon account shape shared by local and cached remote search results.
#[derive(Serialize)]
#[serde(untagged)]
enum SearchAccountResponse {
    Local(Box<AccountResponse>),
    Remote(Box<RemoteAccountResponse>),
}

#[derive(Serialize)]
struct ErrorResponse {
    error: &'static str,
    error_description: String,
}

async fn search(
    State(state): State<AppState>,
    OptionalAuthenticatedAccount(account): OptionalAuthenticatedAccount,
    Query(params): Query<SearchParams>,
) -> Response {
    let privileged = params.resolve.unwrap_or(false)
        || params.following.unwrap_or(false)
        || params.offset.unwrap_or(0) != 0;
    if privileged && account.is_none() {
        return unauthorized();
    }
    let accounts = if matches!(params.search_type, None | Some(SearchType::Accounts)) {
        search_accounts(
            &state,
            account.as_ref().map(|account| account.id),
            &params,
            MAX_SEARCH_LIMIT,
        )
        .await
    } else {
        Ok(Vec::new())
    };
    let hashtags = if matches!(params.search_type, None | Some(SearchType::Hashtags)) {
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
    match search_accounts(&state, Some(account.id), &params, MAX_ACCOUNT_SEARCH_LIMIT).await {
        Ok(accounts) => Json(accounts).into_response(),
        Err(error) => server_error(error),
    }
}

/// Search local accounts and convert them to Mastodon account responses.
async fn search_accounts(
    state: &AppState,
    account_id: Option<AccountId>,
    params: &SearchParams,
    max_limit: u64,
) -> Result<Vec<SearchAccountResponse>> {
    let Some(query) = normalized_account_query(params.q.as_deref()) else {
        return Ok(Vec::new());
    };
    if params.resolve.unwrap_or(false)
        && state.config.federation_enabled
        && crate::federation::discovery::exact_remote_handle(&query).is_some()
    {
        match crate::federation::discovery::resolve_remote_actor_for_search(state, &query).await {
            Ok(_)
            | Err(RoostyError::FederationDiscovery(FederationDiscoveryError::PolicyRejected(_))) => {
            }
            Err(error) => return Err(error),
        }
    }
    let limit = params
        .limit
        .unwrap_or(DEFAULT_SEARCH_LIMIT)
        .clamp(1, max_limit);
    let offset = params.offset.unwrap_or(0);
    let viewer = account_id.unwrap_or(AccountId(uuid::Uuid::nil()));
    let local_domain = state.config.public_base_url.host_str().unwrap_or_default();
    let accounts = roosty_db::search_accounts(
        &state.db,
        roosty_db::AccountSearchOptions {
            viewer_account_id: viewer,
            query: &query,
            local_domain,
            following_only: params.following.unwrap_or(false),
            include_remote: state.config.federation_enabled,
            allow_all_remote_domains: state
                .config
                .federation_allowed_domains
                .iter()
                .any(|domain| domain == "*"),
            allowed_remote_domains: &state.config.federation_allowed_domains,
            blocked_remote_domains: &state.config.federation_blocked_domains,
            limit,
            offset,
        },
    )
    .await?;
    let mut responses = Vec::with_capacity(accounts.len());

    for account in accounts {
        responses.push(match account {
            roosty_db::AccountSearchResult::Local(account) => {
                SearchAccountResponse::Local(Box::new(account_response(state, account).await?))
            }
            roosty_db::AccountSearchResult::Remote(actor) => SearchAccountResponse::Remote(
                Box::new(remote_account_response(state, actor).await?),
            ),
        });
    }

    Ok(responses)
}

/// Search local hashtags and include recent usage history in Mastodon tag responses.
async fn search_hashtags(state: &AppState, params: &SearchParams) -> Result<Vec<TagResponse>> {
    let Some(query) = normalized_tag_query(params.q.as_deref()) else {
        return Ok(Vec::new());
    };
    let limit = params
        .limit
        .unwrap_or(DEFAULT_SEARCH_LIMIT)
        .clamp(1, MAX_SEARCH_LIMIT);
    let offset = params.offset.unwrap_or(0);
    let tags = roosty_db::search_local_tags(&state.db, &query, limit, offset).await?;
    let mut responses = Vec::with_capacity(tags.len());
    for tag in tags {
        let history = roosty_db::tag_history(&state.db, tag.id).await?;
        responses.push(TagResponse::new(state, tag, history, None));
    }

    Ok(responses)
}

/// Normalize local mention-style search terms and reject remote account queries.
fn normalized_account_query(query: Option<&str>) -> Option<String> {
    let trimmed = query?.trim().trim_start_matches('@');
    if trimmed.is_empty() {
        return None;
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

fn server_error(error: RoostyError) -> Response {
    let status = if matches!(error, RoostyError::InvalidInput(_)) {
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    } else {
        axum::http::StatusCode::INTERNAL_SERVER_ERROR
    };
    (
        status,
        Json(ErrorResponse {
            error: "server_error",
            error_description: error.to_string(),
        }),
    )
        .into_response()
}

fn unauthorized() -> Response {
    (
        axum::http::StatusCode::UNAUTHORIZED,
        Json(ErrorResponse {
            error: "unauthorized",
            error_description: "This method requires an authenticated user".to_owned(),
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
    use roosty_core::AccountId;
    use roosty_db::StatusVisibility;
    use roosty_migration::Migrator;
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
    /// Given no token, basic v2 search remains public while privileged parameters require a user.
    async fn v2_search_authenticates_only_privileged_parameters(context: &mut SearchContext) {
        context
            .create_account("alice", "alice@example.com", "Alice Example")
            .await;

        let public = context.get("/api/v2/search?type=accounts&q=alice").await;
        let offset = context
            .get("/api/v2/search?type=accounts&q=alice&offset=1")
            .await;
        let following = context
            .get("/api/v2/search?type=accounts&q=alice&following=true")
            .await;
        let resolve = context
            .get("/api/v2/search?type=accounts&q=alice@example.test&resolve=true")
            .await;

        assert_eq!(public.status(), StatusCode::OK);
        assert_eq!(json_body(public).await["accounts"][0]["username"], "alice");
        assert_eq!(offset.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(following.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(resolve.status(), StatusCode::UNAUTHORIZED);
    }

    #[test_context(SearchContext)]
    #[tokio::test]
    /// Given a fresh cached actor, exact account search returns its navigable remote projection.
    async fn search_returns_cached_remote_accounts(context: &mut SearchContext) {
        context.config.federation_enabled = true;
        context.config.federation_allowed_domains = vec!["*".to_owned()];
        let token = context.access_token().await;
        let actor = context.cache_remote_actor("alice", "remote.test").await;
        let now = time::OffsetDateTime::now_utc();
        for number in 1..=2 {
            roosty_db::upsert_remote_status(
                &context.db,
                roosty_db::NewRemoteStatus {
                    activitypub_id: format!("https://remote.test/statuses/{number}"),
                    remote_actor_id: actor.id,
                    content: format!("remote status {number}"),
                    visibility: StatusVisibility::Public,
                    published_at: now + time::Duration::seconds(number),
                    updated_at: now + time::Duration::seconds(number),
                    in_reply_to: None,
                    in_reply_to_local_status_id: None,
                    in_reply_to_remote_status_id: None,
                    object: serde_json::json!({
                        "tag": [{"type": "Hashtag", "name": "#remote"}]
                    }),
                    tag_names: vec!["remote".to_owned()],
                    quote_automatic_policy: Vec::new(),
                    quote_manual_policy: Vec::new(),
                },
            )
            .await
            .unwrap();
        }

        let response = context
            .authenticated_get(
                "/api/v1/accounts/search?q=alice%40remote.test&limit=1",
                &token,
            )
            .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        assert_eq!(body[0]["id"], actor.id.0.to_string());
        assert_eq!(body[0]["acct"], "alice@remote.test");

        let lookup = context
            .get("/api/v1/accounts/lookup?acct=alice%40remote.test")
            .await;
        assert_eq!(json_body(lookup).await["statuses_count"], 2);
        let show = context
            .get(&format!("/api/v1/accounts/{}", actor.id.0))
            .await;
        assert_eq!(json_body(show).await["acct"], "alice@remote.test");
        let statuses = context
            .get(&format!(
                "/api/v1/accounts/{}/statuses?limit=1&tagged=remote",
                actor.id.0
            ))
            .await;
        assert_eq!(statuses.status(), StatusCode::OK);
        assert!(statuses.headers().contains_key(header::LINK));
        assert_eq!(json_body(statuses).await.as_array().unwrap().len(), 1);

        context.config.federation_blocked_domains = vec!["remote.test".to_owned()];
        let blocked = context
            .authenticated_get("/api/v1/accounts/search?q=alice%40remote.test", &token)
            .await;
        assert_eq!(json_body(blocked).await, serde_json::json!([]));

        context.config.federation_enabled = false;
        context.config.federation_blocked_domains.clear();
        let disabled = context
            .authenticated_get("/api/v1/accounts/search?q=alice%40remote.test", &token)
            .await;
        assert_eq!(json_body(disabled).await, serde_json::json!([]));
    }

    #[test_context(SearchContext)]
    #[tokio::test]
    /// Given stored local hashtags, when v2 search requests hashtags, then Mastodon tag results include usage history.
    async fn search_returns_local_hashtags(context: &mut SearchContext) {
        let token = context.access_token().await;
        let status = roosty_db::create_local_status(
            &context.db,
            roosty_db::NewLocalStatus {
                account_id: context.account_id,
                content: "searchable #RoostTag".to_owned(),
                visibility: StatusVisibility::Public,
                sensitive: false,
                spoiler_text: String::new(),
                language: None,
                in_reply_to_id: None,
                in_reply_to_remote_status_id: None,
                quote_approval_policy: roosty_db::QuoteApprovalPolicy::Nobody,
            },
        )
        .await
        .unwrap();
        roosty_db::replace_local_status_tags(&context.db, status.id, &["roosttag".to_owned()])
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
                "url": "https://roosty.localhost:4000/tags/roosttag",
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
        db: roosty_db::DbConnection,
        config: Config,
        account_id: AccountId,
        application_id: uuid::Uuid,
        _temp_dir: TempDir,
    }

    impl AsyncTestContext for SearchContext {
        async fn setup() -> Self {
            let temp_dir = tempfile::Builder::new()
                .prefix("roosty-search-")
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
            let db = roosty_db::connect(&database_url).await.unwrap();
            Migrator::up(&db, None).await.unwrap();

            let password_hash = password::hash_password("password").unwrap();
            let account_id = AccountId(
                roosty_db::create_bootstrap_admin(
                    &db,
                    "admin",
                    "admin@example.com",
                    &password_hash,
                )
                .await
                .unwrap(),
            );
            let (application, _secret) = roosty_db::create_oauth_application(
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
                public_base_url: "https://roosty.localhost:4000".parse().unwrap(),
                listen_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4000),
                infra_listen_addr: None,
                session_secret: "test-session-secret-change-me-000".to_owned(),
                token_pepper: "test-token-pepper-change-me-0000".to_owned(),
                vapid_private_key: None,
                object_storage_backend: crate::config::ObjectStorageBackend::Local,
                media_root: "./media".to_owned(),
                registration_mode: crate::config::RegistrationMode::Closed,
                federation_enabled: false,
                federation_key_encryption_secret: None,
                federation_allowed_domains: Vec::new(),
                federation_blocked_domains: Vec::new(),
                federation_delivery_max_age: time::Duration::days(7),
                remote_media_cache_ttl: time::Duration::days(30),
                remote_media_max_bytes: 40 * 1024 * 1024,
                remote_media_fetch_concurrency: 5,
                worker_concurrency: 4,
                streaming: crate::config::StreamingConfig::default(),
                instance_name: "Roosty Test".to_owned(),
                instance_description: Some("Endpoint test instance".to_owned()),
            };

            Self {
                postgresql,
                db,
                config,
                account_id,
                application_id: application.id,
                _temp_dir: temp_dir,
            }
        }

        async fn teardown(self) {
            self.db.close().await.unwrap();
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
            roosty_db::create_access_token(
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
                roosty_db::create_local_account(&self.db, username, email, &password_hash)
                    .await
                    .unwrap(),
            );
            roosty_db::update_local_account_settings(
                &self.db,
                account_id,
                roosty_db::LocalAccountSettingsUpdate {
                    display_name: Some(display_name.to_owned()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        }

        async fn cache_remote_actor(&self, username: &str, domain: &str) -> roosty_db::RemoteActor {
            let actor = roosty_db::RemoteActor {
                id: AccountId(uuid::Uuid::now_v7()),
                activitypub_id: format!("https://{domain}/users/{username}"),
                username: username.to_owned(),
                domain: domain.to_owned(),
                display_name: "Remote Alice".to_owned(),
                summary: String::new(),
                emojis: serde_json::json!([]),
                inbox_url: format!("https://{domain}/users/{username}/inbox"),
                shared_inbox_url: None,
                followers_url: None,
                featured_url: None,
                featured_tags_url: None,
                public_key_id: format!("https://{domain}/users/{username}#main-key"),
                public_key_pem: "test-public-key".to_owned(),
                expires_at: time::OffsetDateTime::now_utc() + time::Duration::hours(1),
                profile_created_at: None,
                first_seen_at: time::OffsetDateTime::now_utc(),
                deleted_at: None,
                moved_to_remote_actor_id: None,
            };
            roosty_db::upsert_remote_actor(&self.db, &actor)
                .await
                .unwrap()
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

        format!("roosty_search_{}_{}", std::process::id(), timestamp)
    }
}
