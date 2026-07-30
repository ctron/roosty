//! Server-rendered and hydrated browser UI for Roosty.

#![recursion_limit = "256"]

mod app;
#[cfg(feature = "ssr")]
mod authorization;
mod bootstrap;
mod forms;
mod public_pages;
mod ui;

#[cfg(feature = "ssr")]
pub use app::stylesheet_href;
pub use app::{App, shell};
#[cfg(feature = "ssr")]
pub use authorization::{
    AuthorizationConsent, AuthorizationDecision, AuthorizationPageContext, AuthorizationPermission,
    AuthorizationPermissionKind, AuthorizationResult, OutOfBandAuthorization,
    render_authorization_consent, render_out_of_band_authorization,
};
pub use bootstrap::{
    UiAccount, UiAdminAccount, UiAdminAccountOrigin, UiAdminAccounts, UiAdminAuditEntry,
    UiAdminAuditLog, UiAdminDomainBlock, UiAdminDomainBlocks, UiAdminJob, UiAdminJobSummary,
    UiAdminModeration, UiAdminWorkQueue, UiBackend, UiBootstrap, UiInstanceRule,
    UiModerationReport, UiServerContext,
};
pub use forms::{LoginError, PasswordChangeResult};
pub use public_pages::{
    AtUsernameSegment, UiFeaturedTag, UiMedia, UiMediaKind, UiPoll, UiPollOption, UiPreviewCard,
    UiProfileField, UiProfileHeader, UiProfileTab, UiProfileTimeline, UiPublicAccount,
    UiPublicPageError, UiStatus, UiStatusAuthor, UiStatusPage, UiStatusThread, UiStatusVisibility,
};

#[cfg(feature = "hydrate")]
fn panic_body(_: browser_panic_hook::PanicDetails<'_>) -> String {
    r#"<main class="card border-base-300 bg-base-100 mx-auto my-12 w-full max-w-2xl border shadow-xl">
<div class="card-body">
<h1 class="card-title text-3xl">Roosty needs a fresh start</h1>
<p>The browser application stopped unexpectedly. Details were written to the browser console.</p>
<div class="card-actions"><a class="btn btn-primary" href="">Reload this page</a><a class="btn btn-ghost" href="/">Return home</a></div>
</div>
</main>"#
        .to_owned()
}

#[cfg(feature = "hydrate")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn hydrate() {
    browser_panic_hook::set_once(|| browser_panic_hook::CustomBody::from(panic_body));
    leptos::mount::hydrate_body(App);
}
