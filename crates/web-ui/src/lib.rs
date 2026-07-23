//! Server-rendered and hydrated browser UI for Roosty.

#![recursion_limit = "256"]

mod app;
#[cfg(feature = "ssr")]
mod authorization;
mod bootstrap;
mod forms;

pub use app::{App, shell};
#[cfg(feature = "ssr")]
pub use authorization::{
    AuthorizationConsent, AuthorizationDecision, AuthorizationPageContext, AuthorizationPermission,
    AuthorizationPermissionKind, AuthorizationResult, OutOfBandAuthorization,
    render_authorization_consent, render_out_of_band_authorization,
};
pub use bootstrap::{
    UiAccount, UiAdminAccount, UiAdminAuditEntry, UiAdminDashboard, UiAdminJob, UiAdminJobSummary,
    UiBackend, UiBootstrap, UiServerContext,
};
pub use forms::{LoginError, PasswordChangeResult};

#[cfg(feature = "hydrate")]
fn panic_body(_: browser_panic_hook::PanicDetails<'_>) -> String {
    r#"<main class="panic-page">
<h1>Roosty needs a fresh start</h1>
<p>The browser application stopped unexpectedly. Details were written to the browser console.</p>
<p class="panic-page__actions"><a href="">Reload this page</a><a href="/">Return home</a></p>
</main>"#
        .to_owned()
}

#[cfg(feature = "hydrate")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn hydrate() {
    browser_panic_hook::set_once(|| browser_panic_hook::CustomBody::from(panic_body));
    leptos::mount::hydrate_body(App);
}
