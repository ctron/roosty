use leptos::prelude::*;
use leptos_meta::{Link, Meta, MetaTags, Stylesheet, Title, provide_meta_context};
use leptos_router::{
    components::{A, Route, Router, Routes},
    path,
};

use crate::bootstrap::{UiBootstrap, load_bootstrap};

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
