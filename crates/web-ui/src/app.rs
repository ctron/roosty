use leptos::prelude::*;
use leptos_meta::{Link, Meta, MetaTags, Stylesheet, Title, provide_meta_context};
use leptos_router::{
    components::{A, Route, Router, Routes},
    path,
};

use crate::bootstrap::{UiBootstrap, load_bootstrap};

type BootstrapResource = Resource<Result<UiBootstrap, ServerFnError>>;

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
            <section class="hero">
                <p class="eyebrow">"A small place for a wider social web"</p>
                <h1>"Welcome to Roosty"</h1>
                <p class="hero__lede">
                    "Roosty is an ActivityPub server with Mastodon-compatible APIs, built in Rust."
                </p>
                <p><A attr:class="button" href="/about">"Learn more"</A></p>
            </section>
        </PageFrame>
    }
}

#[component]
fn AboutPage() -> impl IntoView {
    let bootstrap = expect_context::<BootstrapResource>();
    view! {
        <PageMetadata bootstrap page_title="About" path="/about"/>
        <PageFrame bootstrap login_next="/about">
            <article class="prose">
                <p class="eyebrow">"About"</p>
                <h1>"Social networking, with an open protocol"</h1>
                <p>
                    "Roosty speaks ActivityPub and exposes Mastodon-compatible APIs, so people can use the clients they already know while the server remains focused and lightweight."
                </p>
                <p><A href="/">"Return to the welcome page"</A></p>
            </article>
        </PageFrame>
    }
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
                <A attr:class="brand" href="/">"Roosty"</A>
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
                <p>"Built for the open social web."</p>
            </footer>
        </div>
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
                <a href="/auth/edit">"Account"</a>
            }
            .into_any(),
            None => {
                let href = format!("/login?next={login_next}");
                view! { <a href=href>"Sign in"</a> }.into_any()
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
                    .and_then(|value| value.instance_description.clone())
                    .unwrap_or_else(|| {
                        "An ActivityPub server with Mastodon-compatible APIs.".to_owned()
                    });
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
