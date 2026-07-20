//! Server-rendered and hydrated browser UI for Roosty.

mod app;
mod bootstrap;

pub use app::{App, shell};
pub use bootstrap::{UiAccount, UiBackend, UiBootstrap, UiServerContext};

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
