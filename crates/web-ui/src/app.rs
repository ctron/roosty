use leptos::prelude::*;
use leptos_meta::{Link, Meta, MetaTags, Stylesheet, Title, provide_meta_context};
use leptos_router::{
    components::{A, Route, Router, Routes},
    hooks::use_query_map,
    path,
};

use crate::bootstrap::{UiBootstrap, load_bootstrap};
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
            </Routes>
        </Router>
    }
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
        Ok(bootstrap) => {
            view! { <p>"Powered by Roosty v" {bootstrap.server_version}</p> }.into_any()
        }
        Err(_) => view! { <p>"Powered by Roosty"</p> }.into_any(),
    }
}

fn session_navigation(
    result: Result<UiBootstrap, ServerFnError>,
    login_next: &'static str,
) -> AnyView {
    match result {
        Ok(bootstrap) => match bootstrap.account {
            Some(account) => view! {
                <span class="session-account">"Welcome, " {account.username}</span>
                <a href="/auth/edit" rel="external">"Account"</a>
            }
            .into_any(),
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
