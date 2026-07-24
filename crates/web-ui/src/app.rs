use leptos::prelude::*;
use leptos_meta::{Link, Meta, MetaTags, Stylesheet, Title, provide_meta_context};
use leptos_router::{
    components::{A, Route, Router, Routes},
    hooks::use_query_map,
    path,
};

use crate::bootstrap::{UiAdminDashboard, UiBootstrap, load_admin_dashboard, load_bootstrap};
use crate::forms::{LoginError, PasswordChangeResult};

type BootstrapResource = Resource<Result<UiBootstrap, ServerFnError>>;
const DEFAULT_INSTANCE_DESCRIPTION: &str = "A place to connect on the social web.";

/// Render the complete HTML document used for SSR and hydration.
pub fn shell(options: LeptosOptions) -> impl IntoView {
    view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8"/>
                <meta name="viewport" content="width=device-width, initial-scale=1"/>
                <AutoReload options=options.clone()/>
                <HydrationScripts options/>
                <MetaTags/>
            </head>
            <body>
                <App/>
            </body>
        </html>
    }
}

/// Root component shared by the native renderer and browser hydration target.
#[component]
pub fn App() -> impl IntoView {
    provide_meta_context();
    let bootstrap = Resource::new_blocking(|| (), |_| load_bootstrap());
    provide_context(bootstrap);

    view! {
        <Stylesheet id="leptos" href="/pkg/roosty-web.css"/>
        <Router>
            <Routes fallback=|| view! { <NotFoundPage/> }>
                <Route path=path!("") view=WelcomePage/>
                <Route path=path!("about") view=AboutPage/>
                <Route path=path!("login") view=LoginPage/>
                <Route path=path!("auth/edit") view=ChangePasswordPage/>
                <Route path=path!("admin") view=AdminPage/>
                <Route path=path!("admin/accounts") view=AdminPage/>
                <Route path=path!("admin/audit-log") view=AdminPage/>
            </Routes>
        </Router>
    }
}

#[component]
fn AdminPage() -> impl IntoView {
    let bootstrap = expect_context::<BootstrapResource>();
    let query = use_query_map().get().get("q").unwrap_or_default();
    let search_value = query.clone();
    let dashboard = Resource::new_blocking(move || query.clone(), load_admin_dashboard);
    #[cfg(feature = "hydrate")]
    {
        let dashboard = dashboard.clone();
        set_interval(
            move || {
                if !document().hidden() {
                    dashboard.refetch();
                }
            },
            std::time::Duration::from_secs(15),
        );
    }
    view! {
        <PageMetadata bootstrap page_title="Administration" path="/admin"/>
        <PageFrame bootstrap login_next="/admin">
            <Suspense fallback=|| view! { <p>"Loading administrator dashboard…"</p> }>
                {Suspend::new(async move {
                    match dashboard.await {
                        Ok(dashboard) => admin_dashboard_content(dashboard, search_value),
                        Err(_) => view! {
                            <section class="form-card">
                                <h1>"Administrator access required"</h1>
                                <p>"This page is available only to instance administrators."</p>
                            </section>
                        }.into_any(),
                    }
                })}
            </Suspense>
        </PageFrame>
    }
}

fn admin_dashboard_content(dashboard: UiAdminDashboard, search_value: String) -> AnyView {
    let csrf_create = dashboard.csrf_token.clone();
    let csrf_actions = dashboard.csrf_token;
    let summary = dashboard.summary;
    let jobs = dashboard.jobs;
    let accounts = dashboard.accounts;
    let audit_entries = dashboard.audit_entries;
    view! {
        <section class="admin-page">
            <div class="admin-heading">
                <div>
                    <p class="eyebrow">"Instance operations"</p>
                    <h1>"Administration"</h1>
                </div>
                <a class="button button--secondary" href="/admin">"Refresh"</a>
            </div>
            <p class="admin-refresh-note">"Operational data refreshes every 15 seconds while this page is visible."</p>
            <div class="admin-summary" aria-label="Durable queue summary">
                <article><strong>{summary.due}</strong><span>"Due"</span></article>
                <article><strong>{summary.in_progress}</strong><span>"In progress"</span></article>
                <article><strong>{summary.scheduled_retries}</strong><span>"Scheduled retries"</span></article>
                <article><strong>{summary.permanently_failed}</strong><span>"Permanent failures"</span></article>
            </div>
            {summary.oldest_due_at.map(|timestamp| view! {
                <p class="form-message form-message--error">
                    "Oldest due job: " {timestamp}
                </p>
            })}
            <section class="admin-panel">
                <h2>"Durable work"</h2>
                <div class="table-scroll">
                    <table>
                        <thead><tr><th>"Kind"</th><th>"State"</th><th>"Attempts"</th><th>"Run after"</th><th>"Last error"</th></tr></thead>
                        <tbody>
                            {jobs.into_iter().map(|job| view! {
                                <tr>
                                    <td><code>{job.kind}</code></td>
                                    <td>{job.state}</td>
                                    <td>{job.attempts}</td>
                                    <td>{job.run_after}</td>
                                    <td>{job.last_error.unwrap_or_default()}</td>
                                </tr>
                            }).collect_view()}
                        </tbody>
                    </table>
                </div>
            </section>
            <section class="admin-panel">
                <h2>"Create local account"</h2>
                <form method="post" action="/admin/accounts">
                    <input type="hidden" name="csrf_token" value=csrf_create/>
                    <label class="form-field"><span>"Username"</span><input name="username" required minlength="2" maxlength="30"/></label>
                    <label class="form-field"><span>"Email"</span><input name="email" type="email" required/></label>
                    <label class="checkbox-field"><input name="admin" type="checkbox" value="true"/><span>"Grant full administrator privileges"</span></label>
                    <label class="checkbox-field"><input type="checkbox" required/><span>"I confirm this account creation and understand that administrator access is unrestricted."</span></label>
                    <button type="submit">"Create account"</button>
                </form>
            </section>
            <section class="admin-panel">
                <h2>"Accounts"</h2>
                <form class="admin-search" method="get" action="/admin">
                    <label class="form-field"><span>"Search accounts"</span><input name="q" value=search_value placeholder="Username, display name, email, or domain"/></label>
                    <button type="submit">"Search"</button>
                    <a href="/admin">"Clear"</a>
                </form>
                <div class="table-scroll">
                    <table>
                        <thead><tr><th>"Account"</th><th>"Origin"</th><th>"Role"</th><th>"State"</th><th>"Actions"</th></tr></thead>
                        <tbody>
                            {accounts.into_iter().map(|account| {
                                let account_id = account.id.to_string();
                                let reset_id = account_id.clone();
                                let csrf_limit = csrf_actions.clone();
                                let csrf_reset = csrf_actions.clone();
                                let action = if account.limited { "unlimit" } else { "limit" };
                                let handle = account.domain.as_ref().map_or_else(
                                    || account.username.clone(),
                                    |domain| format!("{}@{domain}", account.username),
                                );
                                view! {
                                    <tr>
                                        <td><strong>{handle}</strong><br/><small>{account.display_name}</small></td>
                                        <td>{if account.domain.is_some() { "Remote" } else { "Local" }}</td>
                                        <td>{if account.is_admin { "Admin" } else { "User" }}</td>
                                        <td>{if account.limited { "Limited" } else { "Active" }}</td>
                                        <td class="admin-actions">
                                            <form method="post" action=format!("/admin/accounts/{account_id}/limit")>
                                                <input type="hidden" name="csrf_token" value=csrf_limit/>
                                                <input type="hidden" name="limited" value=(!account.limited).to_string()/>
                                                <label class="checkbox-field"><input type="checkbox" required/><span>"Confirm"</span></label>
                                                <button class="button--secondary" type="submit">{action}</button>
                                            </form>
                                            {account.domain.is_none().then(|| view! {
                                                <form method="post" action=format!("/admin/accounts/{reset_id}/reset-password")>
                                                    <input type="hidden" name="csrf_token" value=csrf_reset/>
                                                    <label class="checkbox-field"><input type="checkbox" required/><span>"Confirm"</span></label>
                                                    <button class="button--secondary" type="submit">"Reset password"</button>
                                                </form>
                                            })}
                                        </td>
                                    </tr>
                                }
                            }).collect_view()}
                        </tbody>
                    </table>
                </div>
            </section>
            <section class="admin-panel">
                <h2>"Recent administrator activity"</h2>
                <ul class="audit-list">
                    {audit_entries.into_iter().map(|entry| view! {
                        <li><time>{entry.created_at}</time> <strong>{entry.action}</strong> <code>{entry.target_id}</code> <span>{entry.source}</span></li>
                    }).collect_view()}
                </ul>
            </section>
        </section>
    }.into_any()
}

#[component]
fn WelcomePage() -> impl IntoView {
    let bootstrap = expect_context::<BootstrapResource>();
    view! {
        <PageMetadata bootstrap page_title="Welcome" path="/"/>
        <PageFrame bootstrap login_next="/">
            <Suspense fallback=|| welcome_content("Roosty".to_owned(), DEFAULT_INSTANCE_DESCRIPTION.to_owned())>
                {Suspend::new(async move {
                    let (name, description) = instance_identity(bootstrap.await.ok());
                    welcome_content(name, description)
                })}
            </Suspense>
        </PageFrame>
    }
}

#[component]
fn AboutPage() -> impl IntoView {
    let bootstrap = expect_context::<BootstrapResource>();
    view! {
        <PageMetadata bootstrap page_title="About" path="/about"/>
        <PageFrame bootstrap login_next="/about">
            <Suspense fallback=|| about_content("Roosty".to_owned(), DEFAULT_INSTANCE_DESCRIPTION.to_owned())>
                {Suspend::new(async move {
                    let (name, description) = instance_identity(bootstrap.await.ok());
                    about_content(name, description)
                })}
            </Suspense>
        </PageFrame>
    }
}

#[component]
fn LoginPage() -> impl IntoView {
    let bootstrap = expect_context::<BootstrapResource>();
    let query = use_query_map().get();
    let next = query.get("next").unwrap_or_else(|| "/".to_owned());
    let error = query
        .get_str("error")
        .and_then(|value| value.parse::<LoginError>().ok());

    view! {
        <PageMetadata bootstrap page_title="Sign in" path="/login"/>
        <PageFrame bootstrap login_next="/login">
            <section class="form-card">
                <p class="eyebrow">"Account access"</p>
                <h1>"Sign in"</h1>
                {error.map(|error| view! {
                    <p class="form-message form-message--error" role="alert">
                        {login_error_message(error)}
                    </p>
                })}
                <form method="post" action="/login">
                    <input type="hidden" name="next" value=next/>
                    <label class="form-field">
                        <span>"Username or email"</span>
                        <input name="login" autocomplete="username" required autofocus/>
                    </label>
                    <label class="form-field">
                        <span>"Password"</span>
                        <input
                            name="password"
                            type="password"
                            autocomplete="current-password"
                            required
                        />
                    </label>
                    <button type="submit">"Sign in"</button>
                </form>
            </section>
        </PageFrame>
    }
}

#[component]
fn ChangePasswordPage() -> impl IntoView {
    let bootstrap = expect_context::<BootstrapResource>();
    let result = use_query_map()
        .get()
        .get_str("result")
        .and_then(|value| value.parse::<PasswordChangeResult>().ok());

    view! {
        <PageMetadata bootstrap page_title="Change password" path="/auth/edit"/>
        <PageFrame bootstrap login_next="/auth/edit">
            <Suspense fallback=|| ()>
                {Suspend::new(async move {
                    match bootstrap.await {
                        Ok(bootstrap) if bootstrap.account.is_some() => {
                            change_password_content(result)
                        }
                        _ => {
                            view! {
                                <section class="form-card">
                                    <h1>"Sign in required"</h1>
                                    <p>"Sign in before changing your password."</p>
                                    <p><a href="/login?next=%2Fauth%2Fedit" rel="external">"Sign in"</a></p>
                                </section>
                            }
                            .into_any()
                        }
                    }
                })}
            </Suspense>
        </PageFrame>
    }
}

fn change_password_content(result: Option<PasswordChangeResult>) -> AnyView {
    let notice = result.map(password_result_message);
    view! {
        <section class="form-card">
            <p class="eyebrow">"Account security"</p>
            <h1>"Change password"</h1>
            {notice.map(|(message, success)| {
                let class = if success {
                    "form-message form-message--success"
                } else {
                    "form-message form-message--error"
                };
                let role = if success { "status" } else { "alert" };
                view! { <p class=class role=role>{message}</p> }
            })}
            <form method="post" action="/auth">
                <label class="form-field">
                    <span>"Current password"</span>
                    <input
                        name="user[current_password]"
                        type="password"
                        autocomplete="current-password"
                        required
                        autofocus
                    />
                </label>
                <label class="form-field">
                    <span>"New password"</span>
                    <input
                        name="user[password]"
                        type="password"
                        autocomplete="new-password"
                        minlength="8"
                        required
                    />
                </label>
                <label class="form-field">
                    <span>"Confirm new password"</span>
                    <input
                        name="user[password_confirmation]"
                        type="password"
                        autocomplete="new-password"
                        minlength="8"
                        required
                    />
                </label>
                <button type="submit">"Change password"</button>
            </form>
        </section>
    }
    .into_any()
}

fn login_error_message(error: LoginError) -> &'static str {
    match error {
        LoginError::InvalidCredentials => "Invalid username or password.",
    }
}

fn password_result_message(result: PasswordChangeResult) -> (&'static str, bool) {
    match result {
        PasswordChangeResult::PasswordChanged => ("Password changed.", true),
        PasswordChangeResult::ConfirmationMismatch => {
            ("New password confirmation does not match.", false)
        }
        PasswordChangeResult::TooShort => ("New password must be at least 8 characters.", false),
        PasswordChangeResult::CurrentPasswordIncorrect => ("Current password is incorrect.", false),
        PasswordChangeResult::ChangeFailed => {
            ("Unable to change password. Please try again.", false)
        }
        PasswordChangeResult::VerificationFailed => {
            ("Unable to verify the current password.", false)
        }
    }
}

fn welcome_content(name: String, description: String) -> AnyView {
    view! {
        <section class="hero">
            <p class="eyebrow">"Welcome to"</p>
            <h1>{name}</h1>
            <p class="hero__lede">{description}</p>
            <p><A attr:class="button" href="/about">"About this instance"</A></p>
        </section>
    }
    .into_any()
}

fn about_content(name: String, description: String) -> AnyView {
    view! {
        <article class="prose">
            <p class="eyebrow">"About this instance"</p>
            <h1>{name}</h1>
            <p>{description}</p>
            <p>
                "This instance is part of the decentralized social web. People can connect across compatible servers without needing an account on the same site."
            </p>
            <p><A href="/">"Return to the welcome page"</A></p>
        </article>
    }
    .into_any()
}

#[component]
fn PageFrame(
    bootstrap: BootstrapResource,
    login_next: &'static str,
    children: Children,
) -> impl IntoView {
    view! {
        <div class="site-shell">
            <header class="site-header">
                <Suspense fallback=|| view! { <A attr:class="brand" href="/">"Roosty"</A> }>
                    {move || bootstrap.get().map(instance_brand)}
                </Suspense>
                <nav aria-label="Primary navigation">
                    <A href="/about">"About"</A>
                    <Suspense fallback=move || view! { <span class="session-placeholder">"Checking session…"</span> }>
                        {move || {
                            bootstrap
                                .get()
                                .map(|result| session_navigation(result, login_next))
                        }}
                    </Suspense>
                </nav>
            </header>
            <main>{children()}</main>
            <footer class="site-footer">
                <Suspense fallback=|| view! { <p>"Powered by Roosty"</p> }>
                    {move || bootstrap.get().map(version_attribution)}
                </Suspense>
            </footer>
        </div>
    }
}

fn instance_identity(bootstrap: Option<UiBootstrap>) -> (String, String) {
    match bootstrap {
        Some(bootstrap) => {
            let description = bootstrap
                .instance_description
                .filter(|description| !description.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_INSTANCE_DESCRIPTION.to_owned());
            (bootstrap.instance_name, description)
        }
        None => ("Roosty".to_owned(), DEFAULT_INSTANCE_DESCRIPTION.to_owned()),
    }
}

fn instance_brand(result: Result<UiBootstrap, ServerFnError>) -> AnyView {
    let name = result
        .map(|bootstrap| bootstrap.instance_name)
        .unwrap_or_else(|_| "Roosty".to_owned());
    view! { <A attr:class="brand" href="/">{name}</A> }.into_any()
}

fn version_attribution(result: Result<UiBootstrap, ServerFnError>) -> AnyView {
    match result {
        Ok(bootstrap) => view! {
            <p>
                "Powered by "
                <a href="https://github.com/ctron/roosty">"Roosty"</a>
                " " {bootstrap.build_identifier}
            </p>
        }
        .into_any(),
        Err(_) => view! {
            <p>"Powered by " <a href="https://github.com/ctron/roosty">"Roosty"</a></p>
        }
        .into_any(),
    }
}

fn session_navigation(
    result: Result<UiBootstrap, ServerFnError>,
    login_next: &'static str,
) -> AnyView {
    match result {
        Ok(bootstrap) => match bootstrap.account {
            Some(account) => {
                let initial = account
                    .display_name
                    .chars()
                    .find(|character| !character.is_whitespace())
                    .or_else(|| account.username.chars().next())
                    .map(|character| character.to_uppercase().to_string())
                    .unwrap_or_else(|| "?".to_owned());
                view! {
                <span class="session-account" title=account.display_name>
                    {account.avatar_url.map_or_else(
                        || view! { <span class="profile-icon" aria-hidden="true">{initial}</span> }.into_any(),
                        |avatar_url| view! {
                            <img class="profile-icon" src=avatar_url alt=""/>
                        }.into_any(),
                    )}
                    <span>{account.username}</span>
                </span>
                {account.is_admin.then(|| view! { <A href="/admin">"Admin"</A> })}
                <a href="/auth/edit" rel="external">"Account"</a>
                <form class="logout-form" method="post" action="/logout">
                    <button type="submit">"Log out"</button>
                </form>
                }
                .into_any()
            }
            None => {
                let href = format!("/login?next={login_next}");
                view! { <a href=href rel="external">"Sign in"</a> }.into_any()
            }
        },
        Err(_) => view! { <span class="session-error">"Session unavailable"</span> }.into_any(),
    }
}

#[component]
fn PageMetadata(
    bootstrap: BootstrapResource,
    page_title: &'static str,
    path: &'static str,
) -> impl IntoView {
    view! {
        <Suspense fallback=|| ()>
            {Suspend::new(async move {
                let bootstrap = bootstrap.await.ok();
                let title = bootstrap
                    .as_ref()
                    .map(|value| format!("{page_title} · {}", value.instance_name))
                    .unwrap_or_else(|| format!("{page_title} · Roosty"));
                let description = bootstrap
                    .as_ref()
                    .and_then(|value| {
                        value
                            .instance_description
                            .clone()
                            .filter(|description| !description.trim().is_empty())
                    })
                    .unwrap_or_else(|| DEFAULT_INSTANCE_DESCRIPTION.to_owned());
                let canonical = bootstrap
                    .as_ref()
                    .map(|value| {
                        format!("{}{path}", value.public_base_url.trim_end_matches('/'))
                    })
                    .unwrap_or_default();

                view! {
                    <Title text=title.clone()/>
                    <Meta name="description" content=description.clone()/>
                    <Meta property="og:title" content=title/>
                    <Meta property="og:description" content=description/>
                    <Meta property="og:type" content="website"/>
                    <Meta property="og:url" content=canonical.clone()/>
                    <Link rel="canonical" href=canonical/>
                }
            })}
        </Suspense>
    }
}

#[component]
fn NotFoundPage() -> impl IntoView {
    view! {
        <main class="not-found">
            <Title text="Page not found · Roosty"/>
            <h1>"Page not found"</h1>
            <p><A href="/">"Return home"</A></p>
        </main>
    }
}
