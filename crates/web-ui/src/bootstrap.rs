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
    pub server_version: String,
    pub account: Option<UiAccount>,
}

/// Non-sensitive account data exposed to the hydrated first-party UI.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiAccount {
    pub id: Uuid,
    pub username: String,
    pub display_name: String,
}

/// Native backend boundary used by SSR and UI server functions.
pub trait UiBackend: Send + Sync {
    fn bootstrap(
        &self,
        cookie_header: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<UiBootstrap, String>> + Send + 'static>>;
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
