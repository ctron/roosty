# First-party frontend

Roosty's first-party frontend uses Leptos 0.8. The existing Axum process renders complete HTML on
the server, serves the generated CSS, JavaScript, and WebAssembly assets, and exposes narrowly
scoped server functions for hydrated interactions. Caddy remains a reverse proxy and does not own
frontend routing.

## Route ownership

UI routes are registered explicitly from the Leptos route list. The initial routes are `/` and
`/about`; direct requests and browser refreshes therefore use the same server renderer as client
navigation. Generated assets live below `/pkg`, and internal UI server functions live below
`/api/web`. Roosty deliberately has no global single-page-app fallback so missing Mastodon API and
ActivityPub routes keep their existing response semantics.

Future public profile and status pages should use human-facing `/@username` routes. Existing
ActivityPub actor and Note URLs remain protocol endpoints and should not depend on browser content
negotiation.

## Rendering and SEO

SEO-relevant content must be present in the initial response. Each public route owns its title,
description, canonical URL, and Open Graph metadata through Leptos Meta. Route data should use a
blocking SSR resource when it affects visible content or metadata; hydration reuses the serialized
resource result instead of replacing server-rendered content after startup.

The welcome and about pages present the operator-configured instance name and description rather
than Roosty project marketing. A missing or blank description uses neutral social-web copy. Roosty
appears as software attribution with its release version in the shared footer.

The shared `roosty-web-ui` crate is compiled once with `ssr` for the native server and once with
`hydrate` for `wasm32-unknown-unknown`. Server-only database and authentication services cross that
boundary through `UiBackend`; they are never compiled into or exposed to the browser bundle.

## Authentication

The first-party UI uses Roosty's signed, secure, HTTP-only session cookie. It is not registered as
an OAuth client and does not store bearer tokens in browser storage. A read-only bootstrap server
function projects only public instance metadata and a small optional account summary. Invalid,
expired, and deleted-account sessions render as anonymous.

The existing `/login` handler remains authoritative during the first UI increment. UI links pass a
sanitized same-origin `next` path, and the server redirects back after login. Links to server-owned
login and account routes use `rel="external"` so Leptos performs a full document navigation instead
of resolving them as client-side UI routes. Future state-changing UI server functions must validate
both the session and a CSRF token.

## Static-page migration

Replace the existing Askama views without changing their underlying protocol or mutation handlers:

1. Login form and errors.
2. Password-change form and validation results.
3. OAuth authorization consent.
4. Out-of-band authorization code display.

Askama can be removed only after the final view is migrated. Elk and Phanpy remain independent full
Mastodon clients throughout this migration.

## Packaging and failures

Install Cargo Leptos 0.3.7 and the wasm-bindgen CLI matching the workspace's wasm-bindgen
dependency (currently 0.2.126). `cargo leptos build` then produces the native binary and
`target/site`. Container and archive releases ship both, and `ROOSTY_WEB_ROOT` locates the site
directory at runtime. The backend serves `/pkg` from that directory.

The hydrated entry point installs `browser-panic-hook` before starting Leptos. Browser panics keep
their diagnostics in the console while replacing the page body with a small, non-diagnostic
recovery view that works without further WebAssembly execution.
