use std::{future::Future, pin::Pin, sync::Arc};

use leptos::{
    prelude::*,
    server_fn::{
        Http,
        codec::{GetUrl, Json},
    },
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::public_pages::{
    ProfileHeaderFuture, ProfileTimelineFuture, StatusPageFuture, ThreadFuture, UiProfileTab,
    UiPublicPageError,
};

/// Public instance and session data needed to render the application shell.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiBootstrap {
    pub instance_name: String,
    pub instance_description: Option<String>,
    pub public_base_url: String,
    pub build_identifier: String,
    pub account: Option<UiAccount>,
    pub csrf_token: Option<String>,
}

/// Non-sensitive account data exposed to the hydrated first-party UI.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiAccount {
    pub id: Uuid,
    pub username: String,
    pub display_name: String,
    pub avatar_url: Option<String>,
    pub is_admin: bool,
}

/// Queue health and sanitized durable work shown on the administrator work-queue page.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiAdminWorkQueue {
    pub summary: UiAdminJobSummary,
    pub jobs: Vec<UiAdminJob>,
}

/// Account management data and mutation protection for the administrator UI.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiAdminAccounts {
    pub csrf_token: String,
    pub accounts: Vec<UiAdminAccount>,
    pub previous_cursor: Option<String>,
    pub next_cursor: Option<String>,
}

/// Persisted federation moderation rules and mutation protection for the administrator UI.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiAdminDomainBlocks {
    pub csrf_token: String,
    pub domain_blocks: Vec<UiAdminDomainBlock>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiAdminDomainBlock {
    pub id: Uuid,
    pub domain: String,
    pub severity: String,
    pub reject_media: bool,
    pub reject_reports: bool,
    pub private_comment: String,
    pub public_comment: String,
    pub obfuscate: bool,
}

/// Reports and public rules displayed by the first-party moderation console.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiAdminModeration {
    pub csrf_token: String,
    pub rules: Vec<UiInstanceRule>,
    pub reports: Vec<UiModerationReport>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiInstanceRule {
    pub id: Uuid,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiModerationReport {
    pub id: Uuid,
    pub category: String,
    pub comment: String,
    pub source: String,
    pub target: String,
    pub target_id: Uuid,
    pub resolved: bool,
    pub assigned: bool,
    pub status_ids: Vec<Uuid>,
}

/// Account origin selected by an administrator account page.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UiAdminAccountOrigin {
    Local,
    Remote,
}

impl UiAdminAccountOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Remote => "remote",
        }
    }
}

/// Sortable columns in the administrator account tables.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UiAdminAccountSort {
    Account,
    Email,
    Role,
    State,
    CreatedAt,
}

impl UiAdminAccountSort {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Account => "account",
            Self::Email => "email",
            Self::Role => "role",
            Self::State => "state",
            Self::CreatedAt => "created_at",
        }
    }
}

/// Direction applied to an administrator account sort column.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UiAdminAccountSortDirection {
    Ascending,
    Descending,
}

impl UiAdminAccountSortDirection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ascending => "ascending",
            Self::Descending => "descending",
        }
    }
}

/// Recent administrator actions shown on the audit-log page.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiAdminAuditLog {
    pub audit_entries: Vec<UiAdminAuditEntry>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiAdminJobSummary {
    pub due: u64,
    pub in_progress: u64,
    pub scheduled_retries: u64,
    pub permanently_failed: u64,
    pub oldest_due_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiAdminJob {
    pub id: Uuid,
    pub kind: String,
    pub state: String,
    pub attempts: u32,
    pub run_after: String,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiAdminAccount {
    pub id: Uuid,
    pub username: String,
    pub domain: Option<String>,
    pub email: Option<String>,
    pub display_name: String,
    pub is_admin: bool,
    pub limited: bool,
    pub suspended: bool,
    pub created_at: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiAdminAuditEntry {
    pub id: Uuid,
    pub action: String,
    pub source: String,
    pub target_kind: String,
    pub target_id: String,
    pub created_at: String,
}

/// Native backend boundary used by SSR and UI server functions.
pub trait UiBackend: Send + Sync {
    fn bootstrap(
        &self,
        cookie_header: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<UiBootstrap, String>> + Send + 'static>>;

    fn profile_header(
        &self,
        _cookie_header: Option<String>,
        _username: String,
    ) -> ProfileHeaderFuture {
        Box::pin(async { Err(UiPublicPageError::NotFound) })
    }

    fn profile_timeline(
        &self,
        _cookie_header: Option<String>,
        _username: String,
        _tab: UiProfileTab,
        _hashtag: Option<String>,
        _max_id: Option<String>,
    ) -> ProfileTimelineFuture {
        Box::pin(async { Err(UiPublicPageError::NotFound) })
    }

    fn profile_statuses(
        &self,
        _cookie_header: Option<String>,
        _username: String,
        _tab: UiProfileTab,
        _hashtag: Option<String>,
        _max_id: String,
    ) -> StatusPageFuture {
        Box::pin(async { Err(UiPublicPageError::NotFound) })
    }

    fn status_thread(
        &self,
        _cookie_header: Option<String>,
        _username: String,
        _status_id: String,
    ) -> ThreadFuture {
        Box::pin(async { Err(UiPublicPageError::NotFound) })
    }

    fn admin_work_queue(
        &self,
        _cookie_header: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<UiAdminWorkQueue, String>> + Send + 'static>> {
        Box::pin(async { Err("administrator work queue is unavailable".to_owned()) })
    }

    fn admin_accounts(
        &self,
        _cookie_header: Option<String>,
        _query: String,
        _origin: UiAdminAccountOrigin,
        _sort: UiAdminAccountSort,
        _direction: UiAdminAccountSortDirection,
        _cursor: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<UiAdminAccounts, String>> + Send + 'static>> {
        Box::pin(async { Err("administrator accounts are unavailable".to_owned()) })
    }

    fn admin_audit_log(
        &self,
        _cookie_header: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<UiAdminAuditLog, String>> + Send + 'static>> {
        Box::pin(async { Err("administrator audit log is unavailable".to_owned()) })
    }

    fn admin_domain_blocks(
        &self,
        _cookie_header: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<UiAdminDomainBlocks, String>> + Send + 'static>> {
        Box::pin(async { Err("administrator federation settings are unavailable".to_owned()) })
    }

    fn admin_moderation(
        &self,
        _cookie_header: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<UiAdminModeration, String>> + Send + 'static>> {
        Box::pin(async { Err("administrator moderation is unavailable".to_owned()) })
    }
}

/// Load administrator-only queue health and durable work as a JSON response.
#[server(prefix = "/api/web", protocol = Http<GetUrl, Json>)]
pub async fn load_admin_work_queue() -> Result<UiAdminWorkQueue, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        expect_context::<UiServerContext>()
            .0
            .admin_work_queue(request_cookie().await?)
            .await
            .map_err(ServerFnError::new)
    }

    #[cfg(not(feature = "ssr"))]
    unreachable!("the browser build uses the generated server-function client")
}

/// Load administrator-only account data as a JSON response.
#[server(prefix = "/api/web", protocol = Http<GetUrl, Json>)]
pub async fn load_admin_accounts(
    query: String,
    origin: UiAdminAccountOrigin,
    sort: UiAdminAccountSort,
    direction: UiAdminAccountSortDirection,
    cursor: Option<String>,
) -> Result<UiAdminAccounts, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        expect_context::<UiServerContext>()
            .0
            .admin_accounts(
                request_cookie().await?,
                query,
                origin,
                sort,
                direction,
                cursor,
            )
            .await
            .map_err(ServerFnError::new)
    }

    #[cfg(not(feature = "ssr"))]
    unreachable!("the browser build uses the generated server-function client")
}

/// Load recent administrator actions as a JSON response.
#[server(prefix = "/api/web", protocol = Http<GetUrl, Json>)]
pub async fn load_admin_audit_log() -> Result<UiAdminAuditLog, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        expect_context::<UiServerContext>()
            .0
            .admin_audit_log(request_cookie().await?)
            .await
            .map_err(ServerFnError::new)
    }

    #[cfg(not(feature = "ssr"))]
    unreachable!("the browser build uses the generated server-function client")
}

/// Load administrator-managed federation domain rules as a JSON response.
#[server(prefix = "/api/web", protocol = Http<GetUrl, Json>)]
pub async fn load_admin_domain_blocks() -> Result<UiAdminDomainBlocks, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        expect_context::<UiServerContext>()
            .0
            .admin_domain_blocks(request_cookie().await?)
            .await
            .map_err(ServerFnError::new)
    }

    #[cfg(not(feature = "ssr"))]
    unreachable!("the browser build uses the generated server-function client")
}

/// Load unresolved reports and instance rules for the moderation console.
#[server(prefix = "/api/web", protocol = Http<GetUrl, Json>)]
pub async fn load_admin_moderation() -> Result<UiAdminModeration, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        expect_context::<UiServerContext>()
            .0
            .admin_moderation(request_cookie().await?)
            .await
            .map_err(ServerFnError::new)
    }

    #[cfg(not(feature = "ssr"))]
    unreachable!("the browser build uses the generated server-function client")
}

#[cfg(feature = "ssr")]
pub(crate) async fn request_cookie() -> Result<Option<String>, ServerFnError> {
    use axum::http::{HeaderMap, HeaderValue, header};

    let headers: HeaderMap = leptos_axum::extract().await?;
    expect_context::<leptos_axum::ResponseOptions>().insert_header(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    expect_context::<leptos_axum::ResponseOptions>()
        .insert_header(header::VARY, HeaderValue::from_static("Cookie"));
    Ok(headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned))
}

/// Request-independent native services supplied by the Axum integration.
#[derive(Clone)]
pub struct UiServerContext(pub Arc<dyn UiBackend>);

/// Load public configuration and the optional current account on the server.
#[server(prefix = "/api/web")]
pub async fn load_bootstrap() -> Result<UiBootstrap, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        use axum::http::{HeaderMap, HeaderValue, header};

        let headers: HeaderMap = leptos_axum::extract().await?;
        let response = expect_context::<leptos_axum::ResponseOptions>();
        response.insert_header(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, no-store"),
        );
        response.insert_header(header::VARY, HeaderValue::from_static("Cookie"));
        let cookie_header = headers
            .get(header::COOKIE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let backend = expect_context::<UiServerContext>();
        backend
            .0
            .bootstrap(cookie_header)
            .await
            .map_err(ServerFnError::new)
    }

    #[cfg(not(feature = "ssr"))]
    unreachable!("the browser build uses the generated server-function client")
}
