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

Leptos renders the `/login`, `/auth/edit`, and OAuth authorization views, while the existing server
handlers remain authoritative for credential verification, session cookies, password updates, and
OAuth grants. Native HTML form submissions use POST/Redirect/GET; fixed Strum-backed values select
user-facing results and OAuth consent decisions without putting credentials or arbitrary errors in
URLs. Login return paths remain sanitized and same-origin. The password page and OAuth consent flow
redirect anonymous visitors through login.

Links to server-owned account routes use `rel="external"` so Leptos performs a full document
navigation instead of resolving them as client-side UI routes. Future state-changing UI server
functions must validate both the session and a CSRF token.

## Administration

Administrators have an SSR and hydrated operations interface at `/admin`, with account and audit
deep links at `/admin/accounts` and `/admin/audit-log`. The navigation link is projected only for
accounts whose persisted `is_admin` flag is set, and the server independently protects every admin
route. Anonymous requests return through login; authenticated non-administrators receive a
forbidden response.

The dashboard reads durable queue health from PostgreSQL so multiple server and worker processes
share the same view. It polls every 15 seconds while the document is visible and exposes only job
kind, lifecycle state, attempts, scheduling timestamps, and bounded error text. Raw job payloads
are never sent to the UI. Process-local Prometheus metrics remain infrastructure endpoints.

Account creation, password reset, and local or cached-remote account limits use native forms with
a session-bound HMAC CSRF token. Mutations and their audit records commit in one transaction.
Generated temporary passwords are returned only by the successful response and are not stored in
audit metadata.

## OAuth views

OAuth consent and out-of-band code results use SSR-only Leptos documents with the shared instance
chrome and stylesheet. They intentionally use native forms without hydration: consent needs no
browser-side state, and an out-of-band one-time code must stay in the POST response rather than a
redirect URL. Consent lists the requested scopes and supports typed approve or deny decisions;
regular denials return `access_denied` to the exact registered callback, while out-of-band denials
render a local result. Elk and Phanpy remain independent full Mastodon clients.

## Packaging and failures

Install Cargo Leptos 0.3.7 and the wasm-bindgen CLI matching the workspace's wasm-bindgen
dependency (currently 0.2.126). Set `LEPTOS_WASM_OPT_VERSION=version_131` when invoking Cargo
Leptos; CI, release, and container builds set this automatically. Because Cargo Leptos prefers an
existing executable, any `wasm-opt` already on `PATH` must also be version 131. `cargo leptos build`
then produces the native binary and `target/site`. Container and archive releases ship both, and
`ROOSTY_WEB_ROOT` locates the site directory at runtime. The backend serves `/pkg` from that
directory.

The hydrated entry point installs `browser-panic-hook` before starting Leptos. Browser panics keep
their diagnostics in the console while replacing the page body with a small, non-diagnostic
recovery view that works without further WebAssembly execution.
