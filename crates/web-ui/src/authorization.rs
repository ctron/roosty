use leptos::prelude::*;
use serde::Deserialize;
use strum::IntoStaticStr;

/// User choice submitted from the OAuth consent form.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, IntoStaticStr, PartialEq)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum AuthorizationDecision {
    #[default]
    Approve,
    Deny,
}

/// Instance and account information shown around an authorization page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizationPageContext {
    pub instance_name: String,
    pub server_version: String,
    pub account_username: String,
}

/// A requested OAuth scope prepared for human-readable display.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizationPermission {
    pub scope: String,
    pub kind: AuthorizationPermissionKind,
}

impl AuthorizationPermission {
    /// Classify a Mastodon scope without narrowing the extensible wire value.
    pub fn new(scope: impl Into<String>) -> Self {
        let scope = scope.into();
        let root = scope
            .split_once(':')
            .map_or(scope.as_str(), |(root, _)| root);
        let kind = match root {
            "read" => AuthorizationPermissionKind::Read,
            "write" => AuthorizationPermissionKind::Write,
            "follow" => AuthorizationPermissionKind::Follow,
            "push" => AuthorizationPermissionKind::Push,
            _ => AuthorizationPermissionKind::Other,
        };
        Self { scope, kind }
    }
}

/// Friendly category for a requested OAuth scope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthorizationPermissionKind {
    Read,
    Write,
    Follow,
    Push,
    Other,
}

/// Validated values rendered into the native OAuth consent form.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizationConsent {
    pub context: AuthorizationPageContext,
    pub application_name: String,
    pub response_type: String,
    pub client_id: String,
    pub redirect_uri: String,
    pub scope: String,
    pub state: String,
    pub code_challenge: String,
    pub code_challenge_method: String,
    pub permissions: Vec<AuthorizationPermission>,
}

/// Outcome displayed when an OAuth client uses the out-of-band callback.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthorizationResult {
    Approved { code: String },
    Denied,
}

/// Data for the out-of-band authorization result page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutOfBandAuthorization {
    pub context: AuthorizationPageContext,
    pub application_name: String,
    pub result: AuthorizationResult,
}

/// Render the validated OAuth consent form as an SSR-only document.
pub fn render_authorization_consent(consent: AuthorizationConsent) -> String {
    let title = format!(
        "Authorize {} · {}",
        consent.application_name, consent.context.instance_name
    );
    let approve: &'static str = AuthorizationDecision::Approve.into();
    let deny: &'static str = AuthorizationDecision::Deny.into();
    let context = consent.context.clone();

    let application_name = consent.application_name;
    let permissions = consent.permissions;
    let content = view! {
                <section class="form-card authorization-card">
                    <p class="eyebrow">"Application access"</p>
                    <h1>"Authorize " {application_name}</h1>
                    <p>"This application is requesting permission to use your account."</p>
                    <h2>"Requested permissions"</h2>
                    <ul class="permission-list">
                        {permissions.into_iter().map(permission_view).collect_view()}
                    </ul>
                    <form method="post" action="/oauth/authorize">
                        <input type="hidden" name="response_type" value=consent.response_type/>
                        <input type="hidden" name="client_id" value=consent.client_id/>
                        <input type="hidden" name="redirect_uri" value=consent.redirect_uri/>
                        <input type="hidden" name="scope" value=consent.scope/>
                        <input type="hidden" name="state" value=consent.state/>
                        <input type="hidden" name="code_challenge" value=consent.code_challenge/>
                        <input
                            type="hidden"
                            name="code_challenge_method"
                            value=consent.code_challenge_method
                        />
                        <div class="form-actions">
                            <button type="submit" name="decision" value=approve>
                                "Authorize"
                            </button>
                            <button
                                type="submit"
                                name="decision"
                                value=deny
                                class="button--secondary"
                            >
                                "Deny"
                            </button>
                        </div>
                    </form>
                </section>
    }
    .into_any();
    render_document(title, context, content)
}

/// Render an out-of-band OAuth result without placing the one-time code in a URL.
pub fn render_out_of_band_authorization(page: OutOfBandAuthorization) -> String {
    let title = format!("Authorization result · {}", page.context.instance_name);
    let context = page.context.clone();
    let application_name = page.application_name;
    let content = match page.result {
        AuthorizationResult::Approved { code } => view! {
            <section class="form-card authorization-card">
                <p class="eyebrow">"Authorization complete"</p>
                <h1>"Copy your code"</h1>
                <p>
                    "Enter this one-time code in " <strong>{application_name}</strong> "."
                </p>
                <code id="authorization-code" class="authorization-code">{code}</code>
                <p>"Keep this code private. It grants access to your account."</p>
            </section>
        }
        .into_any(),
        AuthorizationResult::Denied => view! {
            <section class="form-card authorization-card">
                <p class="eyebrow">"Authorization denied"</p>
                <h1>"Access was not granted"</h1>
                <p>{application_name} " was not given access to your account."</p>
                <p>"You can close this page and return to the application."</p>
            </section>
        }
        .into_any(),
    };
    render_document(title, context, content)
}

fn permission_view(permission: AuthorizationPermission) -> impl IntoView {
    let description = match permission.kind {
        AuthorizationPermissionKind::Read => "Read account data",
        AuthorizationPermissionKind::Write => "Publish and modify content",
        AuthorizationPermissionKind::Follow => "Manage follows",
        AuthorizationPermissionKind::Push => "Receive push notifications",
        AuthorizationPermissionKind::Other => "Use this application-specific permission",
    };
    view! {
        <li>
            <strong>{description}</strong>
            <code>{permission.scope}</code>
        </li>
    }
}

fn render_document(title: String, context: AuthorizationPageContext, content: AnyView) -> String {
    let instance_name = context.instance_name;
    let account_username = context.account_username;
    let server_version = context.server_version;
    view! {
            <!DOCTYPE html>
            <html lang="en">
                <head>
                    <meta charset="utf-8"/>
                    <meta name="viewport" content="width=device-width, initial-scale=1"/>
                    <title>{title}</title>
                    <link rel="stylesheet" href="/pkg/roosty-web.css"/>
                </head>
                <body>
                    <div class="site-shell">
                        <header class="site-header">
                            <a class="brand" href="/">{instance_name}</a>
                            <nav aria-label="Primary navigation">
                                <a href="/about">"About"</a>
                                <span class="session-account">"Welcome, " {account_username}</span>
                                <a href="/auth/edit">"Account"</a>
                            </nav>
                        </header>
                        <main>{content}</main>
                        <footer class="site-footer">
                            <p>"Powered by Roosty v" {server_version}</p>
                        </footer>
                    </div>
                </body>
            </html>
    }
    .to_html()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_granular_scopes_by_their_root() {
        assert_eq!(
            AuthorizationPermission::new("read:accounts").kind,
            AuthorizationPermissionKind::Read
        );
        assert_eq!(
            AuthorizationPermission::new("custom:action").kind,
            AuthorizationPermissionKind::Other
        );
    }

    #[test]
    fn consent_escapes_application_metadata_and_has_no_hydration_script() {
        let html = render_authorization_consent(AuthorizationConsent {
            context: AuthorizationPageContext {
                instance_name: "Test instance".to_owned(),
                server_version: "1.2.3".to_owned(),
                account_username: "alice".to_owned(),
            },
            application_name: "<script>alert('x')</script>".to_owned(),
            response_type: "code".to_owned(),
            client_id: "client".to_owned(),
            redirect_uri: "https://client.example/callback".to_owned(),
            scope: "read:accounts custom:action".to_owned(),
            state: String::new(),
            code_challenge: String::new(),
            code_challenge_method: String::new(),
            permissions: vec![
                AuthorizationPermission::new("read:accounts"),
                AuthorizationPermission::new("custom:action"),
            ],
        });

        assert!(html.contains("&lt;script&gt;alert('x')&lt;/script&gt;"));
        assert!(!html.contains("<script>alert('x')</script>"));
        assert!(html.contains("Read account data"));
        assert!(html.contains("Use this application-specific permission"));
        assert!(!html.contains("roosty-web.js"));
    }
}
