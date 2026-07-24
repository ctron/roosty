use std::{future::Future, pin::Pin, sync::Arc};

use leptos::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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

/// Sanitized administrator dashboard data loaded from shared database state.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiAdminDashboard {
    pub csrf_token: String,
    pub summary: UiAdminJobSummary,
    pub jobs: Vec<UiAdminJob>,
    pub accounts: Vec<UiAdminAccount>,
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

    fn admin_dashboard(
        &self,
        _cookie_header: Option<String>,
        _query: String,
    ) -> Pin<Box<dyn Future<Output = Result<UiAdminDashboard, String>> + Send + 'static>> {
        Box::pin(async { Err("administrator dashboard is unavailable".to_owned()) })
    }
}

/// Load administrator-only operational and account data.
#[server(prefix = "/api/web")]
pub async fn load_admin_dashboard(query: String) -> Result<UiAdminDashboard, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        use axum::http::{HeaderMap, header};

        let headers: HeaderMap = leptos_axum::extract().await?;
        let cookie_header = headers
            .get(header::COOKIE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let backend = expect_context::<UiServerContext>();
        backend
            .0
            .admin_dashboard(cookie_header, query)
            .await
            .map_err(ServerFnError::new)
    }

    #[cfg(not(feature = "ssr"))]
    unreachable!("the browser build uses the generated server-function client")
}

/// Request-independent native services supplied by the Axum integration.
#[derive(Clone)]
pub struct UiServerContext(pub Arc<dyn UiBackend>);

/// Load public configuration and the optional current account on the server.
#[server(prefix = "/api/web")]
pub async fn load_bootstrap() -> Result<UiBootstrap, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        use axum::http::{HeaderMap, header};

        let headers: HeaderMap = leptos_axum::extract().await?;
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
