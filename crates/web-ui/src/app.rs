use std::collections::HashSet;
#[cfg(feature = "ssr")]
use std::{env, fs, path::Path};

use leptos::prelude::*;
use leptos_meta::{HashedStylesheet, Link, Meta, MetaTags, Script, Title, provide_meta_context};
use leptos_router::{
    ParamSegment, StaticSegment,
    components::{A, Outlet, ParentRoute, Route, Router, Routes},
    hooks::{use_location, use_params_map, use_query_map},
    path,
};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde::{Serialize, de::DeserializeOwned};
use uuid::Uuid;

use crate::{
    bootstrap::{
        UiAdminAccountOrigin, UiAdminAccountSort, UiAdminAccountSortDirection, UiAdminAccounts,
        UiAdminAuditLog, UiAdminDomainBlocks, UiAdminModeration, UiAdminWorkQueue, UiBootstrap,
        load_admin_accounts, load_admin_audit_log, load_admin_domain_blocks, load_admin_moderation,
        load_admin_work_queue, load_bootstrap,
    },
    forms::{LoginError, PasswordChangeResult},
    public_pages::{
        AtUsernameSegment, UiMedia, UiMediaKind, UiProfileHeader, UiProfileTab, UiProfileTimeline,
        UiStatus, UiStatusThread, UiStatusVisibility, load_profile_header, load_profile_statuses,
        load_profile_timeline, load_status_thread,
    },
    structured_data::{posting_structured_data, profile_structured_data},
    ui::{
        AccountMenu, AdminActionModal, AdminLayout, AdminPanel, AdminSection, FormField, Hero,
        Notice, NoticeKind, Page, PageCard, PageCardKind, PageCardTitle, PageTitle, SiteFooter,
        SiteHeader,
    },
};

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
                <HydrationScripts options=options.clone()/>
                <HashedStylesheet id="leptos" options/>
                <MetaTags/>
            </head>
            <body>
                <App/>
            </body>
        </html>
    }
}

/// Resolve the CSS asset name produced by Cargo Leptos for standalone SSR documents.
#[cfg(feature = "ssr")]
pub fn stylesheet_href(options: &LeptosOptions) -> String {
    let mut filename = options.output_name.to_string();
    if options.hash_files {
        let hash_path = env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(Path::to_path_buf))
            .unwrap_or_default()
            .join(options.hash_file.as_ref());
        if let Ok(hashes) = fs::read_to_string(hash_path)
            && let Some(hash) = hashes.lines().find_map(|line| {
                let (file, hash) = line.trim().split_once(':')?;
                (file == "css").then_some(hash.trim())
            })
        {
            filename.push('.');
            filename.push_str(hash);
        }
    }
    format!("/pkg/{filename}.css")
}

/// Root component shared by the native renderer and browser hydration target.
#[component]
pub fn App() -> impl IntoView {
    provide_meta_context();
    let bootstrap = Resource::new_blocking(|| (), |_| load_bootstrap());
    provide_context(bootstrap);

    view! {
        <Router>
            <Routes fallback=|| view! { <NotFoundPage/> }>
                <Route path=path!("") view=WelcomePage/>
                <Route path=path!("about") view=AboutPage/>
                <Route path=path!("login") view=LoginPage/>
                <Route path=path!("auth/edit") view=ChangePasswordPage/>
                <Route path=path!("admin") view=AdminWorkQueuePage/>
                <Route path=path!("admin/jobs") view=AdminWorkQueuePage/>
                <Route path=path!("admin/accounts") view=AdminLocalAccountsPage/>
                <Route path=path!("admin/remote-accounts") view=AdminRemoteAccountsPage/>
                <Route path=path!("admin/federation") view=AdminFederationPage/>
                <Route path=path!("admin/moderation") view=AdminModerationPage/>
                <Route path=path!("admin/audit-log") view=AdminAuditLogPage/>
                <ParentRoute path=AtUsernameSegment("username") view=PublicProfileLayout>
                    <Route path=path!("") view=PublicProfilePostsPage/>
                    <Route path=path!("with_replies") view=PublicProfileRepliesPage/>
                    <Route path=path!("media") view=PublicProfileMediaPage/>
                    <Route
                        path=(StaticSegment("tagged"), ParamSegment("hashtag"))
                        view=PublicProfileTaggedPage
                    />
                </ParentRoute>
                <Route
                    path=(AtUsernameSegment("username"), ParamSegment("status_id"))
                    view=PublicStatusPage
                />
            </Routes>
        </Router>
    }
}

#[component]
fn PublicProfilePostsPage() -> impl IntoView {
    view! { <PublicProfileTimelinePage tab=UiProfileTab::Posts/> }
}

#[component]
fn PublicProfileRepliesPage() -> impl IntoView {
    view! { <PublicProfileTimelinePage tab=UiProfileTab::WithReplies/> }
}

#[component]
fn PublicProfileMediaPage() -> impl IntoView {
    view! { <PublicProfileTimelinePage tab=UiProfileTab::Media/> }
}

#[component]
fn PublicProfileTaggedPage() -> impl IntoView {
    view! { <PublicProfileTimelinePage tab=UiProfileTab::Tagged/> }
}

#[component]
fn PublicProfileLayout() -> impl IntoView {
    let bootstrap = expect_context::<BootstrapResource>();
    let params = use_params_map().get();
    let username = params.get("username").unwrap_or_default();
    let profile = Resource::new_blocking(move || username.clone(), load_profile_header);

    view! {
        <PageFrame bootstrap login_next="/">
            <Transition fallback=|| public_page_loading("Loading profile…")>
                {Suspend::new(async move {
                    match profile.await {
                        Ok(header) => view! { <PublicProfileHeaderView header/> }.into_any(),
                        Err(_) => public_page_not_found("Profile not found"),
                    }
                })}
            </Transition>
        </PageFrame>
    }
}

#[component]
fn PublicProfileTimelinePage(tab: UiProfileTab) -> impl IntoView {
    let header = expect_context::<UiProfileHeader>();
    let params = use_params_map().get();
    let username = params.get("username").unwrap_or_default();
    let hashtag = matches!(tab, UiProfileTab::Tagged)
        .then(|| params.get("hashtag"))
        .flatten();
    let max_id = use_query_map().get().get("max_id");
    let metadata_tab = tab.clone();
    let metadata_hashtag = hashtag.clone();
    let metadata_max_id = max_id.clone();
    let profile = Resource::new_blocking(
        move || {
            (
                username.clone(),
                hashtag.clone(),
                max_id.clone(),
                tab.clone(),
            )
        },
        |(username, hashtag, max_id, tab)| load_profile_timeline(username, tab, hashtag, max_id),
    );

    view! {
        <PublicProfileMetadata
            header
            tab=metadata_tab
            hashtag=metadata_hashtag
            max_id=metadata_max_id
        />
        <Transition fallback=|| public_timeline_loading()>
            {Suspend::new(async move {
                match profile.await {
                    Ok(page) => public_profile_timeline_content(page),
                    Err(_) => public_page_not_found("Profile not found"),
                }
            })}
        </Transition>
    }
}

#[component]
fn PublicStatusPage() -> impl IntoView {
    let bootstrap = expect_context::<BootstrapResource>();
    let params = use_params_map().get();
    let username = params.get("username").unwrap_or_default();
    let status_id = params.get("status_id").unwrap_or_default();
    let thread = Resource::new_blocking(
        move || (username.clone(), status_id.clone()),
        |(username, status_id)| load_status_thread(username, status_id),
    );

    view! {
        <PageFrame bootstrap login_next="/">
            <Transition fallback=|| public_page_loading("Loading conversation…")>
                {Suspend::new(async move {
                    match thread.await {
                        Ok(thread) => public_thread_content(thread),
                        Err(_) => public_page_not_found("Status not found"),
                    }
                })}
            </Transition>
        </PageFrame>
    }
}

fn public_page_loading(message: &'static str) -> AnyView {
    view! {
        <div class="mx-auto grid max-w-3xl gap-4 py-12" aria-live="polite">
            <span class="loading loading-spinner" aria-hidden="true"></span>
            <span>{message}</span>
        </div>
    }
    .into_any()
}

fn public_page_not_found(title: &'static str) -> AnyView {
    view! {
        <section class="card border-base-300 bg-base-100 mx-auto my-12 max-w-3xl border shadow">
            <div class="card-body">
                <Title text=format!("{title} · Roosty")/>
                <h1 class="card-title text-2xl">{title}</h1>
                <p>"This resource does not exist or is not visible to you."</p>
            </div>
        </section>
    }
    .into_any()
}

fn public_timeline_loading() -> AnyView {
    view! {
        <div class="grid justify-items-center gap-3 py-8" aria-live="polite">
            <span class="loading loading-spinner" aria-hidden="true"></span>
            <span>"Loading posts…"</span>
        </div>
    }
    .into_any()
}

#[component]
fn PublicProfileHeaderView(header: UiProfileHeader) -> impl IntoView {
    provide_context(header.clone());
    let account = header.account;
    let profile_path = format!("/@{}", account.username);
    let replies_path = format!("{profile_path}/with_replies");
    let media_path = format!("{profile_path}/media");
    let pathname = use_location().pathname;
    let posts_class_path = profile_path.clone();
    let replies_class_path = replies_path.clone();
    let media_class_path = media_path.clone();

    view! {
        <main class="mx-auto grid w-full max-w-3xl gap-6 py-8">
            <section class="card border-base-300 bg-base-100 overflow-hidden border shadow h-card">
                {account.header_url.clone().map(|url| view! {
                    <img class="h-48 w-full object-cover" src=url alt=""/>
                })}
                <div class="card-body">
                    <div class="flex items-end gap-4">
                        {account.avatar_url.clone().map(|url| view! {
                            <img
                                class="h-24 w-24 rounded-full border-4 border-base-100 object-cover u-photo"
                                src=url
                                alt=format!("{}’s avatar", account.display_name)
                            />
                        })}
                        <div>
                            <h1 class="card-title text-3xl p-name">{account.display_name.clone()}</h1>
                            <a class="link link-hover u-url" href=profile_path.clone()>
                                "@" {account.username.clone()}
                            </a>
                        </div>
                    </div>
                    <p class="whitespace-pre-wrap p-note">{account.bio.clone()}</p>
                    {(!account.fields.is_empty()).then(|| view! {
                        <dl class="grid gap-2 sm:grid-cols-2">
                            {account.fields.clone().into_iter().map(|field| view! {
                                <div class="rounded-box bg-base-200 p-3">
                                    <dt class="font-semibold">{field.name}</dt>
                                    <dd class="break-words">{field.value}</dd>
                                </div>
                            }).collect_view()}
                        </dl>
                    })}
                    <dl class="stats stats-vertical bg-base-200 sm:stats-horizontal">
                        <div class="stat"><dt class="stat-title">"Posts"</dt><dd class="stat-value text-2xl">{account.statuses_count}</dd></div>
                        <div class="stat"><dt class="stat-title">"Following"</dt><dd class="stat-value text-2xl">{account.following_count}</dd></div>
                        <div class="stat"><dt class="stat-title">"Followers"</dt><dd class="stat-value text-2xl">{account.followers_count}</dd></div>
                    </dl>
                    <p class="text-base-content/70">
                        "Joined " <time datetime=account.created_at.clone()>{account.created_at.clone()}</time>
                    </p>
                    {(!header.featured_tags.is_empty()).then(|| view! {
                        <nav aria-label="Featured hashtags" class="flex flex-wrap gap-2">
                            {header.featured_tags.into_iter().map(|tag| view! {
                                <a
                                    class="badge badge-outline"
                                    href=format!("/@{}/tagged/{}", account.username, tag.name)
                                >
                                    "#" {tag.name.clone()} " · " {tag.statuses_count}
                                </a>
                            }).collect_view()}
                        </nav>
                    })}
                </div>
            </section>

            <nav class="tabs tabs-box" aria-label="Profile timelines">
                <A
                    attr:class=move || if pathname.get() == posts_class_path { "tab tab-active" } else { "tab" }
                    href=profile_path
                    exact=true
                >
                    "Posts"
                </A>
                <A
                    attr:class=move || if pathname.get() == replies_class_path { "tab tab-active" } else { "tab" }
                    href=replies_path
                    exact=true
                >
                    "Posts and replies"
                </A>
                <A
                    attr:class=move || if pathname.get() == media_class_path { "tab tab-active" } else { "tab" }
                    href=media_path
                    exact=true
                >
                    "Media"
                </A>
            </nav>

            <Outlet/>
        </main>
    }
}

fn public_profile_timeline_content(page: UiProfileTimeline) -> AnyView {
    let header = expect_context::<UiProfileHeader>();
    let username = header.account.username.clone();
    let hashtag = page.hashtag.clone();
    let tab = page.tab.clone();
    let initial_next = page.timeline.next_cursor;
    let statuses = RwSignal::new(page.timeline.statuses.clone());
    let next_cursor = RwSignal::new(initial_next);
    let load_more = Action::new(move |cursor: &Uuid| {
        let cursor = *cursor;
        let username = username.clone();
        let hashtag = hashtag.clone();
        let tab = tab.clone();
        async move {
            (
                cursor,
                load_profile_statuses(username, tab, hashtag, cursor.to_string()).await,
            )
        }
    });
    let load_error = RwSignal::new(false);
    Effect::new(move |_| {
        load_more.value().with(|value| {
            let Some((cursor, result)) = value else {
                return;
            };
            match result {
                Ok(page) => {
                    statuses.update(|current| append_unique_statuses(current, &page.statuses));
                    next_cursor.set(page.next_cursor);
                    load_error.set(false);
                    replace_cursor_query(*cursor);
                }
                Err(_) => load_error.set(true),
            }
        });
    });

    view! {
        {(!page.pinned_statuses.is_empty()).then(|| view! {
                <section class="grid gap-4" aria-labelledby="pinned-heading">
                    <h2 id="pinned-heading" class="text-xl font-semibold">"Pinned posts"</h2>
                    {page.pinned_statuses.clone().into_iter().map(public_status_card).collect_view()}
                </section>
        })}

        <section class="grid gap-4" aria-label="Profile posts">
            <Show when=move || statuses.with(Vec::is_empty)>
                <div class="card border-base-300 bg-base-100 border"><div class="card-body">"No posts to show."</div></div>
            </Show>
            <For
                each=move || statuses.get()
                key=|status| status.id
                children=public_status_card
            />
        </section>
        <Show when=move || load_error.get()>
            <div class="alert alert-error" role="alert">
                "Could not load more posts. You can retry."
            </div>
        </Show>
        <Show when=move || next_cursor.get().is_some()>
            <button
                class="btn btn-outline"
                type="button"
                disabled=move || load_more.pending().get()
                on:click=move |_| {
                    if let Some(cursor) = next_cursor.get_untracked() {
                        load_error.set(false);
                        load_more.dispatch(cursor);
                    }
                }
            >
                {move || if load_more.pending().get() { "Loading…" } else { "Load more" }}
            </button>
        </Show>
    }
    .into_any()
}

fn append_unique_statuses(current: &mut Vec<UiStatus>, incoming: &[UiStatus]) {
    let existing = current
        .iter()
        .map(|status| status.id)
        .collect::<HashSet<_>>();
    current.extend(
        incoming
            .iter()
            .filter(|status| !existing.contains(&status.id))
            .cloned(),
    );
}

#[cfg(feature = "hydrate")]
fn replace_cursor_query(cursor: Uuid) {
    let location = window().location();
    let path = location.pathname().unwrap_or_default();
    let url = format!("{path}?max_id={cursor}");
    let _ = window().history().and_then(|history| {
        history.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&url))
    });
}

#[cfg(not(feature = "hydrate"))]
fn replace_cursor_query(_cursor: Uuid) {}

fn public_thread_content(thread: UiStatusThread) -> AnyView {
    view! {
        <PublicStatusMetadata thread=thread.clone()/>
        <main class="mx-auto grid w-full max-w-3xl gap-4 py-8">
            <nav aria-label="Profile">
                <a class="btn btn-ghost" href=format!("/@{}", thread.account.username)>
                    "← " {thread.account.display_name.clone()}
                </a>
            </nav>
            {thread.ancestors.into_iter().map(public_status_card).collect_view()}
            <div class="ring-primary rounded-box ring-2">
                {public_status_card(thread.status)}
            </div>
            {thread.descendants.into_iter().map(public_status_card).collect_view()}
        </main>
    }
    .into_any()
}

fn public_status_card(status: UiStatus) -> AnyView {
    let content = status.content_html.clone();
    let media = status.media.clone();
    let sensitive = status.sensitive;
    let spoiler = status.spoiler_text.clone();
    let body = view! {
        <div class="grid gap-4">
            <div class="prose max-w-none e-content" inner_html=content></div>
            {(!media.is_empty()).then(|| public_status_media(media))}
            {status.poll.clone().map(|poll| view! {
                <section class="grid gap-2" aria-label="Poll">
                    {poll.options.into_iter().map(|option| view! {
                        <div class="rounded-box bg-base-200 flex justify-between p-3">
                            <span>{option.title}</span>
                            {option.votes_count.map(|count| view! { <span>{count} " votes"</span> })}
                        </div>
                    }).collect_view()}
                    <small>{if poll.expired { "Poll closed" } else { "Poll open (read-only)" }}</small>
                </section>
            })}
            {status.card.clone().map(|card| view! {
                <a class="card card-side border-base-300 overflow-hidden border" href=card.url rel="nofollow noopener noreferrer">
                    {card.image_url.map(|url| view! { <img class="w-32 object-cover" src=url alt=""/> })}
                    <div class="card-body p-4">
                        <strong>{card.title}</strong>
                        <span>{card.description}</span>
                        <small>{card.provider_name}</small>
                    </div>
                </a>
            })}
            {status.quote.clone().map(|quote| view! {
                <blockquote class="border-base-300 border-l-4 pl-4">
                    {public_status_card(*quote)}
                </blockquote>
            })}
        </div>
    };
    let status_body = if sensitive || !spoiler.is_empty() {
        view! {
            <details class="collapse collapse-arrow bg-base-200">
                <summary class="collapse-title font-semibold">
                    {if spoiler.is_empty() { "Sensitive content".to_owned() } else { spoiler }}
                </summary>
                <div class="collapse-content">{body}</div>
            </details>
        }
        .into_any()
    } else {
        body.into_any()
    };
    let visibility = match status.visibility {
        UiStatusVisibility::Public => "Public",
        UiStatusVisibility::Unlisted => "Unlisted",
        UiStatusVisibility::Private => "Followers only",
        UiStatusVisibility::Direct => "Direct",
    };
    view! {
        <article class="card border-base-300 bg-base-100 border shadow-sm h-entry">
            <div class="card-body gap-4">
                <header class="flex items-center gap-3 h-card p-author">
                    {status.author.avatar_url.clone().map(|url| view! {
                        <img class="h-12 w-12 rounded-full object-cover u-photo" src=url alt=""/>
                    })}
                    <div class="min-w-0">
                        <a class="font-semibold link link-hover p-name u-url" href=status.author.url.clone()>
                            {status.author.display_name}
                        </a>
                        <div class="text-base-content/70 truncate">{status.author.handle}</div>
                    </div>
                    {status.pinned.then(|| view! { <span class="badge badge-primary ml-auto">"Pinned"</span> })}
                </header>
                {status_body}
                <footer class="text-base-content/70 flex flex-wrap gap-3 text-sm">
                    <a class="u-url" href=status.url>
                        <time class="dt-published" datetime=status.created_at.clone()>{status.created_at.clone()}</time>
                    </a>
                    <span>{visibility}</span>
                    <span>{status.replies_count} " replies"</span>
                    <span>{status.reblogs_count} " boosts"</span>
                    <span>{status.favourites_count} " favourites"</span>
                </footer>
            </div>
        </article>
    }
    .into_any()
}

fn public_status_media(media: Vec<UiMedia>) -> AnyView {
    view! {
        <div class="grid gap-2 sm:grid-cols-2">
            {media.into_iter().map(|item| {
                let description = item.description.unwrap_or_default();
                let url = item.url;
                let preview_url = item.preview_url.unwrap_or_else(|| url.clone());
                match item.kind {
                    UiMediaKind::Image | UiMediaKind::Unknown => view! {
                        <a href=url>
                            <img class="rounded-box max-h-96 w-full object-cover" src=preview_url alt=description/>
                        </a>
                    }.into_any(),
                    UiMediaKind::Video => view! {
                        <video class="rounded-box w-full" controls preload="metadata">
                            <source src=url/>
                            {description}
                        </video>
                    }.into_any(),
                    UiMediaKind::Audio => view! {
                        <div>
                            <audio class="w-full" controls preload="metadata" src=url></audio>
                            <p>{description}</p>
                        </div>
                    }.into_any(),
                }
            }).collect_view()}
        </div>
    }
    .into_any()
}

#[component]
fn PublicProfileMetadata(
    header: UiProfileHeader,
    tab: UiProfileTab,
    hashtag: Option<String>,
    max_id: Option<String>,
) -> impl IntoView {
    let account = &header.account;
    let title = format!("{} (@{})", account.display_name, account.username);
    let description = if account.bio.trim().is_empty() {
        format!("Posts by @{}", account.username)
    } else {
        account.bio.clone()
    };
    let image = account.avatar_url.clone();
    let canonical_url = match tab {
        UiProfileTab::Tagged => format!(
            "{}/tagged/{}",
            header.profile_url,
            hashtag.unwrap_or_default().trim_start_matches('#')
        ),
        _ => format!("{}{}", header.profile_url, tab.path_suffix()),
    };
    let eligible = header.search_indexing_enabled && !account.limited && account.discoverable;
    let noindex = max_id.is_some() || !eligible;
    let structured_data = (!noindex).then(|| profile_structured_data(&header));
    view! {
        <Title text=title.clone()/>
        <Meta name="description" content=description.clone()/>
        <Meta name="robots" content=if noindex { "noindex, nofollow" } else { "index, follow" }/>
        <Meta property="og:type" content="profile"/>
        <Meta property="og:title" content=title/>
        <Meta property="og:description" content=description/>
        <Meta property="og:url" content=canonical_url.clone()/>
        {image.map(|image| view! { <Meta property="og:image" content=image/> })}
        <Meta name="fediverse:creator" content=format!("@{}", account.username)/>
        <Link rel="canonical" href=canonical_url/>
        <Link rel="alternate" type_="application/activity+json" href=header.activitypub_url/>
        {structured_data.map(|data| view! { <Script type_="application/ld+json">{data}</Script> })}
    }
}

#[component]
fn PublicStatusMetadata(thread: UiStatusThread) -> impl IntoView {
    let sensitive = thread.status.sensitive;
    let description = if sensitive {
        thread.status.spoiler_text.clone()
    } else {
        thread.status.content_text.chars().take(300).collect()
    };
    let title = format!(
        "{} (@{}) on Roosty",
        thread.account.display_name, thread.account.username
    );
    let image = (!sensitive)
        .then(|| thread.status.media.first())
        .flatten()
        .map(|media| {
            media
                .preview_url
                .clone()
                .unwrap_or_else(|| media.url.clone())
        });
    let structured_data = (!thread.noindex && thread.search_indexing_enabled)
        .then(|| posting_structured_data(&thread))
        .flatten();
    view! {
        <Title text=title.clone()/>
        <Meta name="description" content=description.clone()/>
        <Meta name="robots" content=if thread.noindex { "noindex, nofollow" } else { "index, follow" }/>
        <Meta property="og:type" content="article"/>
        <Meta property="og:title" content=title/>
        <Meta property="og:description" content=description/>
        <Meta property="og:url" content=thread.canonical_url.clone()/>
        {image.map(|image| view! { <Meta property="og:image" content=image/> })}
        <Meta name="fediverse:creator" content=format!("@{}", thread.account.username)/>
        <Link rel="canonical" href=thread.canonical_url/>
        <Link rel="alternate" type_="application/activity+json" href=thread.activitypub_url/>
        {structured_data.map(|data| view! { <Script type_="application/ld+json">{data}</Script> })}
    }
}

#[cfg(test)]
mod public_page_tests {
    use uuid::Uuid;

    use super::{append_unique_statuses, profile_structured_data};
    use crate::public_pages::{
        UiProfileHeader, UiPublicAccount, UiStatus, UiStatusAuthor, UiStatusVisibility,
    };

    fn status(id: Uuid) -> UiStatus {
        UiStatus {
            id,
            author: UiStatusAuthor {
                display_name: "Alice".to_owned(),
                handle: "@alice".to_owned(),
                url: "/@alice".to_owned(),
                avatar_url: None,
                local: true,
            },
            url: format!("/@alice/{id}"),
            activitypub_url: format!("/users/alice/statuses/{id}"),
            content_html: "<p>Hello</p>".to_owned(),
            content_text: "Hello".to_owned(),
            spoiler_text: String::new(),
            sensitive: false,
            visibility: UiStatusVisibility::Public,
            created_at: "2026-07-29T00:00:00Z".to_owned(),
            edited_at: None,
            media: Vec::new(),
            poll: None,
            card: None,
            quote: None,
            replies_count: 0,
            reblogs_count: 0,
            favourites_count: 0,
            pinned: false,
        }
    }

    #[test]
    fn load_more_append_deduplicates_status_ids() {
        let first = Uuid::now_v7();
        let second = Uuid::now_v7();
        let mut current = vec![status(first)];
        append_unique_statuses(&mut current, &[status(first), status(second)]);
        assert_eq!(
            current.iter().map(|status| status.id).collect::<Vec<_>>(),
            vec![first, second]
        );
    }

    #[test]
    fn profile_structured_data_is_google_compatible_and_script_safe() {
        let header = UiProfileHeader {
            account: UiPublicAccount {
                id: Uuid::nil(),
                username: "alice".to_owned(),
                display_name: "Alice </script><script>alert(1)</script>".to_owned(),
                bio: "A profile & biography".to_owned(),
                avatar_url: Some("https://roosty.test/alice.png".to_owned()),
                header_url: None,
                fields: Vec::new(),
                created_at: "2026-07-29T00:00:00Z".to_owned(),
                followers_count: 4,
                following_count: 2,
                statuses_count: 8,
                limited: false,
                discoverable: true,
            },
            featured_tags: Vec::new(),
            profile_url: "https://roosty.test/@alice".to_owned(),
            activitypub_url: "https://roosty.test/users/alice".to_owned(),
            search_indexing_enabled: true,
        };

        let structured_data = profile_structured_data(&header);

        assert!(structured_data.contains("\"@type\":\"ProfilePage\""));
        assert!(structured_data.contains("\"@type\":\"Person\""));
        assert!(structured_data.contains("\"userInteractionCount\":4"));
        assert!(structured_data.contains("\"userInteractionCount\":8"));
        assert!(!structured_data.contains("</script>"));
        assert!(structured_data.contains("\\u003c/script\\u003e"));
    }
}

#[component]
fn AdminModerationPage() -> impl IntoView {
    let bootstrap = expect_context::<BootstrapResource>();
    let moderation = Resource::new_blocking(|| (), |_| load_admin_moderation());
    install_periodic_refresh(moderation);

    view! {
        <PageMetadata bootstrap page_title="Moderation" path="/admin/moderation"/>
        <PageFrame bootstrap login_next="/admin/moderation" wide=true>
            <AdminLayout active=AdminSection::Moderation>
                <AdminPageHeading title="Moderation" resource=moderation/>
                <Transition fallback=|| admin_loading("Loading moderation reports…")>
                    {Suspend::new(async move {
                        match moderation.await {
                            Ok(moderation) => admin_moderation_content(moderation),
                            Err(_) => admin_load_error("moderation reports"),
                        }
                    })}
                </Transition>
            </AdminLayout>
        </PageFrame>
    }
}

#[component]
fn AdminFederationPage() -> impl IntoView {
    let bootstrap = expect_context::<BootstrapResource>();
    let domain_blocks = Resource::new_blocking(|| (), |_| load_admin_domain_blocks());
    install_periodic_refresh(domain_blocks);

    view! {
        <PageMetadata bootstrap page_title="Federation" path="/admin/federation"/>
        <PageFrame bootstrap login_next="/admin/federation" wide=true>
            <AdminLayout active=AdminSection::Federation>
                <AdminPageHeading title="Federation" resource=domain_blocks/>
                <Transition fallback=|| admin_loading("Loading federation settings…")>
                    {Suspend::new(async move {
                        match domain_blocks.await {
                            Ok(domain_blocks) => admin_domain_blocks_content(domain_blocks),
                            Err(_) => admin_load_error("federation settings"),
                        }
                    })}
                </Transition>
            </AdminLayout>
        </PageFrame>
    }
}

#[component]
fn AdminWorkQueuePage() -> impl IntoView {
    let bootstrap = expect_context::<BootstrapResource>();
    let work_queue = Resource::new_blocking(|| (), |_| load_admin_work_queue());
    install_periodic_refresh(work_queue);

    view! {
        <PageMetadata bootstrap page_title="Work queue" path="/admin"/>
        <PageFrame bootstrap login_next="/admin" wide=true>
            <AdminLayout active=AdminSection::WorkQueue>
                <AdminPageHeading title="Work queue" resource=work_queue/>
                <Transition fallback=|| admin_loading("Loading durable work…")>
                    {Suspend::new(async move {
                        match work_queue.await {
                            Ok(work_queue) => admin_work_queue_content(work_queue),
                            Err(_) => admin_load_error("work queue"),
                        }
                    })}
                </Transition>
            </AdminLayout>
        </PageFrame>
    }
}

#[component]
fn AdminLocalAccountsPage() -> impl IntoView {
    view! { <AdminAccountsPage origin=UiAdminAccountOrigin::Local/> }
}

#[component]
fn AdminRemoteAccountsPage() -> impl IntoView {
    view! { <AdminAccountsPage origin=UiAdminAccountOrigin::Remote/> }
}

#[component]
fn AdminAccountsPage(origin: UiAdminAccountOrigin) -> impl IntoView {
    let bootstrap = expect_context::<BootstrapResource>();
    let query_map = use_query_map().get();
    let query = query_map.get("q").unwrap_or_default();
    let sort = parse_admin_account_sort(query_map.get("sort").as_deref(), origin);
    let direction = parse_admin_account_sort_direction(query_map.get("order").as_deref());
    let cursor = query_map.get("cursor");
    let search_value = query.clone();
    let accounts = Resource::new_blocking(
        move || (query.clone(), origin, sort, direction, cursor.clone()),
        |(query, origin, sort, direction, cursor)| {
            load_admin_accounts(query, origin, sort, direction, cursor)
        },
    );
    install_periodic_refresh(accounts);
    let (title, path, section) = match origin {
        UiAdminAccountOrigin::Local => (
            "Local accounts",
            "/admin/accounts",
            AdminSection::LocalAccounts,
        ),
        UiAdminAccountOrigin::Remote => (
            "Remote accounts",
            "/admin/remote-accounts",
            AdminSection::RemoteAccounts,
        ),
    };

    view! {
        <PageMetadata bootstrap page_title=title path=path/>
        <PageFrame bootstrap login_next=path wide=true>
            <AdminLayout active=section>
                <AdminPageHeading title=title resource=accounts/>
                <Transition fallback=|| admin_loading("Loading accounts…")>
                    {Suspend::new(async move {
                        match accounts.await {
                            Ok(accounts) => {
                                admin_accounts_content(
                                    accounts,
                                    search_value,
                                    origin,
                                    sort,
                                    direction,
                                    path,
                                )
                            }
                            Err(_) => admin_load_error("accounts"),
                        }
                    })}
                </Transition>
            </AdminLayout>
        </PageFrame>
    }
}

#[component]
fn AdminAuditLogPage() -> impl IntoView {
    let bootstrap = expect_context::<BootstrapResource>();
    let audit_log = Resource::new_blocking(|| (), |_| load_admin_audit_log());
    install_periodic_refresh(audit_log);

    view! {
        <PageMetadata bootstrap page_title="Audit log" path="/admin/audit-log"/>
        <PageFrame bootstrap login_next="/admin/audit-log" wide=true>
            <AdminLayout active=AdminSection::AuditLog>
                <AdminPageHeading title="Audit log" resource=audit_log/>
                <Transition fallback=|| admin_loading("Loading administrator activity…")>
                    {Suspend::new(async move {
                        match audit_log.await {
                            Ok(audit_log) => admin_audit_log_content(audit_log),
                            Err(_) => admin_load_error("audit log"),
                        }
                    })}
                </Transition>
            </AdminLayout>
        </PageFrame>
    }
}

#[component]
fn AdminPageHeading<T>(title: &'static str, resource: Resource<T>) -> impl IntoView
where
    T: DeserializeOwned + Serialize + Send + Sync + 'static,
{
    let refresh = resource;
    view! {
        <section class="grid gap-4 py-8">
            <div class="flex flex-col gap-4 sm:flex-row sm:items-end sm:justify-between">
                <PageTitle>{title}</PageTitle>
                <button
                    class="btn btn-outline"
                    type="button"
                    on:click=move |_| {
                        refresh.refetch();
                    }
                >
                    "Refresh"
                </button>
            </div>
            <p class="text-base-content/70">
                "Data refreshes every 15 seconds while this page is visible."
            </p>
        </section>
    }
}

#[cfg(feature = "hydrate")]
fn install_periodic_refresh<T>(resource: Resource<T>)
where
    T: DeserializeOwned + Serialize + Send + Sync + 'static,
{
    if let Ok(handle) = set_interval_with_handle(
        move || {
            if !document().hidden() {
                resource.refetch();
            }
        },
        std::time::Duration::from_secs(15),
    ) {
        on_cleanup(move || handle.clear());
    }
}

#[cfg(not(feature = "hydrate"))]
fn install_periodic_refresh<T>(_resource: Resource<T>)
where
    T: Send + Sync + 'static,
{
}

fn admin_loading(message: &'static str) -> AnyView {
    view! {
        <div class="py-8">
            <span class="loading loading-spinner" aria-hidden="true"></span>
            <span class="ml-3">{message}</span>
        </div>
    }
    .into_any()
}

fn admin_load_error(category: &'static str) -> AnyView {
    view! {
        <Notice kind=NoticeKind::Error>
            "Could not load the administrator " {category} ". Try again or check the server logs."
        </Notice>
    }
    .into_any()
}

fn admin_work_queue_content(work_queue: UiAdminWorkQueue) -> AnyView {
    let summary = work_queue.summary;
    view! {
        <section class="grid gap-6 pb-8">
            <div
                class="stats stats-vertical bg-base-100 w-full shadow lg:stats-horizontal"
                aria-label="Durable queue summary"
            >
                <article class="stat">
                    <strong class="stat-value">{summary.due}</strong>
                    <span class="stat-title">"Due"</span>
                </article>
                <article class="stat">
                    <strong class="stat-value">{summary.in_progress}</strong>
                    <span class="stat-title">"In progress"</span>
                </article>
                <article class="stat">
                    <strong class="stat-value">{summary.scheduled_retries}</strong>
                    <span class="stat-title">"Scheduled retries"</span>
                </article>
                <article class="stat">
                    <strong class="stat-value">{summary.permanently_failed}</strong>
                    <span class="stat-title">"Permanent failures"</span>
                </article>
            </div>
            {summary.oldest_due_at.map(|timestamp| view! {
                <Notice kind=NoticeKind::Error>"Oldest due job: " {timestamp}</Notice>
            })}
            <AdminPanel title="Durable work">
                <div class="overflow-x-auto">
                    <table class="table table-zebra">
                        <thead>
                            <tr>
                                <th>"Kind"</th>
                                <th>"State"</th>
                                <th>"Attempts"</th>
                                <th>"Run after"</th>
                                <th>"Last error"</th>
                            </tr>
                        </thead>
                        <tbody>
                            {work_queue.jobs.into_iter().map(|job| view! {
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
            </AdminPanel>
        </section>
    }
    .into_any()
}

fn admin_accounts_content(
    accounts: UiAdminAccounts,
    search_value: String,
    origin: UiAdminAccountOrigin,
    sort: UiAdminAccountSort,
    direction: UiAdminAccountSortDirection,
    path: &'static str,
) -> AnyView {
    match origin {
        UiAdminAccountOrigin::Local => {
            local_accounts_content(accounts, search_value, sort, direction, path)
        }
        UiAdminAccountOrigin::Remote => {
            remote_accounts_content(accounts, search_value, sort, direction, path)
        }
    }
}

fn parse_admin_account_sort(
    value: Option<&str>,
    origin: UiAdminAccountOrigin,
) -> UiAdminAccountSort {
    match value {
        Some("account") => UiAdminAccountSort::Account,
        Some("email") if origin == UiAdminAccountOrigin::Local => UiAdminAccountSort::Email,
        Some("role") if origin == UiAdminAccountOrigin::Local => UiAdminAccountSort::Role,
        Some("state") => UiAdminAccountSort::State,
        Some("created_at") | None => UiAdminAccountSort::CreatedAt,
        Some(_) => UiAdminAccountSort::CreatedAt,
    }
}

fn parse_admin_account_sort_direction(value: Option<&str>) -> UiAdminAccountSortDirection {
    match value {
        Some("ascending") => UiAdminAccountSortDirection::Ascending,
        Some("descending") | None => UiAdminAccountSortDirection::Descending,
        Some(_) => UiAdminAccountSortDirection::Descending,
    }
}

fn admin_accounts_url(
    path: &str,
    query: &str,
    sort: UiAdminAccountSort,
    direction: UiAdminAccountSortDirection,
    cursor: Option<&str>,
) -> String {
    let mut parameters = Vec::new();
    if !query.is_empty() {
        parameters.push(format!(
            "q={}",
            utf8_percent_encode(query, NON_ALPHANUMERIC)
        ));
    }
    parameters.push(format!("sort={}", sort.as_str()));
    parameters.push(format!("order={}", direction.as_str()));
    if let Some(cursor) = cursor {
        parameters.push(format!(
            "cursor={}",
            utf8_percent_encode(cursor, NON_ALPHANUMERIC)
        ));
    }
    format!("{path}?{}", parameters.join("&"))
}

#[component]
fn AdminAccountSortHeader(
    label: &'static str,
    column: UiAdminAccountSort,
    active_sort: UiAdminAccountSort,
    direction: UiAdminAccountSortDirection,
    query: String,
    path: &'static str,
) -> impl IntoView {
    let active = column == active_sort;
    let next_direction = if active {
        match direction {
            UiAdminAccountSortDirection::Ascending => UiAdminAccountSortDirection::Descending,
            UiAdminAccountSortDirection::Descending => UiAdminAccountSortDirection::Ascending,
        }
    } else if column == UiAdminAccountSort::CreatedAt {
        UiAdminAccountSortDirection::Descending
    } else {
        UiAdminAccountSortDirection::Ascending
    };
    let aria_sort = if active { direction.as_str() } else { "none" };
    let indicator = if active {
        match direction {
            UiAdminAccountSortDirection::Ascending => " ↑",
            UiAdminAccountSortDirection::Descending => " ↓",
        }
    } else {
        ""
    };
    let href = admin_accounts_url(path, &query, column, next_direction, None);
    view! {
        <th aria-sort=aria_sort>
            <a class="link link-hover whitespace-nowrap" href=href>{label}{indicator}</a>
        </th>
    }
}

#[component]
fn AdminAccountPagination(
    previous_cursor: Option<String>,
    next_cursor: Option<String>,
    query: String,
    sort: UiAdminAccountSort,
    direction: UiAdminAccountSortDirection,
    path: &'static str,
) -> impl IntoView {
    let previous_href = previous_cursor
        .as_deref()
        .map(|cursor| admin_accounts_url(path, &query, sort, direction, Some(cursor)));
    let next_href = next_cursor
        .as_deref()
        .map(|cursor| admin_accounts_url(path, &query, sort, direction, Some(cursor)));
    view! {
        <nav class="flex items-center justify-between gap-4 pt-4" aria-label="Account pages">
            {previous_href.map(|href| view! {
                <a class="btn btn-outline" href=href rel="prev">"Previous"</a>
            })}
            <span class="grow"></span>
            {next_href.map(|href| view! {
                <a class="btn btn-outline" href=href rel="next">"Next"</a>
            })}
        </nav>
    }
}

#[cfg(test)]
mod admin_account_tests {
    use super::{admin_accounts_url, parse_admin_account_sort};
    use crate::bootstrap::{UiAdminAccountOrigin, UiAdminAccountSort, UiAdminAccountSortDirection};

    #[test]
    fn account_page_urls_preserve_and_encode_view_state() {
        assert_eq!(
            admin_accounts_url(
                "/admin/accounts",
                "alice & bob",
                UiAdminAccountSort::Email,
                UiAdminAccountSortDirection::Ascending,
                Some("opaque_cursor"),
            ),
            "/admin/accounts?q=alice%20%26%20bob&sort=email&order=ascending&cursor=opaque%5Fcursor"
        );
    }

    #[test]
    fn remote_account_pages_reject_local_only_sort_columns() {
        assert_eq!(
            parse_admin_account_sort(Some("email"), UiAdminAccountOrigin::Remote),
            UiAdminAccountSort::CreatedAt
        );
        assert_eq!(
            parse_admin_account_sort(Some("role"), UiAdminAccountOrigin::Remote),
            UiAdminAccountSort::CreatedAt
        );
    }
}

fn local_accounts_content(
    accounts: UiAdminAccounts,
    search_value: String,
    sort: UiAdminAccountSort,
    direction: UiAdminAccountSortDirection,
    path: &'static str,
) -> AnyView {
    let csrf_create = accounts.csrf_token.clone();
    let csrf_actions = accounts.csrf_token;
    let previous_cursor = accounts.previous_cursor;
    let next_cursor = accounts.next_cursor;
    let account_rows = accounts.accounts;
    let accounts_empty = account_rows.is_empty();
    let search_input_value = search_value.clone();
    view! {
        <section class="grid gap-6 pb-8">
            <AdminPanel title="Create local account">
                <form class="fieldset max-w-xl gap-4" method="post" action="/admin/accounts">
                    <input type="hidden" name="csrf_token" value=csrf_create/>
                    <FormField label="Username">
                        <input class="input w-full" name="username" required minlength="2" maxlength="30"/>
                    </FormField>
                    <FormField label="Email">
                        <input class="input w-full" name="email" type="email" required/>
                    </FormField>
                    <label class="label justify-start gap-3">
                        <input class="checkbox" name="admin" type="checkbox" value="true"/>
                        <span>"Grant full administrator privileges"</span>
                    </label>
                    <label class="label items-start justify-start gap-3">
                        <input class="checkbox" type="checkbox" required/>
                        <span>
                            "I confirm this account creation and understand that administrator access is unrestricted."
                        </span>
                    </label>
                    <button class="btn btn-primary" type="submit">"Create account"</button>
                </form>
            </AdminPanel>
            <AdminPanel title="Local accounts">
                <form class="fieldset max-w-xl gap-4" method="get" action=path>
                    <input type="hidden" name="sort" value=sort.as_str()/>
                    <input type="hidden" name="order" value=direction.as_str()/>
                    <FormField label="Search local accounts">
                        <input
                            class="input w-full"
                            name="q"
                            value=search_input_value
                            placeholder="Username, display name, or email"
                        />
                    </FormField>
                    <div class="card-actions">
                        <button class="btn btn-primary" type="submit">"Search"</button>
                        <a
                            class="btn btn-ghost"
                            href=admin_accounts_url(path, "", sort, direction, None)
                        >
                            "Clear"
                        </a>
                    </div>
                </form>
                <div class="overflow-x-auto">
                    <table class="table table-zebra">
                        <thead>
                            <tr>
                                <AdminAccountSortHeader
                                    label="Account"
                                    column=UiAdminAccountSort::Account
                                    active_sort=sort
                                    direction
                                    query=search_value.clone()
                                    path
                                />
                                <AdminAccountSortHeader
                                    label="Email"
                                    column=UiAdminAccountSort::Email
                                    active_sort=sort
                                    direction
                                    query=search_value.clone()
                                    path
                                />
                                <AdminAccountSortHeader
                                    label="Role"
                                    column=UiAdminAccountSort::Role
                                    active_sort=sort
                                    direction
                                    query=search_value.clone()
                                    path
                                />
                                <AdminAccountSortHeader
                                    label="State"
                                    column=UiAdminAccountSort::State
                                    active_sort=sort
                                    direction
                                    query=search_value.clone()
                                    path
                                />
                                <AdminAccountSortHeader
                                    label="Created"
                                    column=UiAdminAccountSort::CreatedAt
                                    active_sort=sort
                                    direction
                                    query=search_value.clone()
                                    path
                                />
                                <th>"Actions"</th>
                            </tr>
                        </thead>
                        <tbody>
                            {accounts_empty.then(|| view! {
                                <tr><td colspan="6">"No local accounts match this search."</td></tr>
                            })}
                            {account_rows.into_iter().map(|account| {
                                let account_id = account.id.to_string();
                                let reset_id = account_id.clone();
                                let csrf_limit = csrf_actions.clone();
                                let csrf_suspend = csrf_actions.clone();
                                let csrf_reset = csrf_actions.clone();
                                let (action, title) = if account.limited {
                                    ("Unlimit", "Unlimit local account?")
                                } else {
                                    ("Limit", "Limit local account?")
                                };
                                let username = account.username;
                                let (suspend_action, suspend_title) = if account.suspended {
                                    ("Unsuspend", "Unsuspend local account?")
                                } else {
                                    ("Suspend", "Suspend local account?")
                                };
                                view! {
                                    <tr>
                                        <td>
                                            <strong>{username.clone()}</strong>
                                            <br/>
                                            <small>{account.display_name}</small>
                                        </td>
                                        <td>{account.email.unwrap_or_default()}</td>
                                        <td>{if account.is_admin { "Admin" } else { "User" }}</td>
                                        <td>{if account.suspended { "Suspended" } else if account.limited { "Limited" } else { "Active" }}</td>
                                        <td><time datetime=account.created_at.clone()>{account.created_at.clone()}</time></td>
                                        <td class="flex flex-wrap gap-2">
                                            <AdminActionModal
                                                id=format!("limit-{account_id}")
                                                trigger_label=action
                                                title
                                                message=format!(
                                                    "Are you sure you want to {} {username}?",
                                                    action.to_lowercase(),
                                                )
                                                form_action=format!("/admin/accounts/{account_id}/limit")
                                                csrf_token=csrf_limit
                                                limited=!account.limited
                                            />
                                            <AdminActionModal
                                                id=format!("suspend-{account_id}")
                                                trigger_label=suspend_action
                                                title=suspend_title
                                                message=format!(
                                                    "{} {username}? Suspension hides all content and severs follows.",
                                                    suspend_action,
                                                )
                                                form_action=format!("/admin/accounts/{account_id}/suspend")
                                                csrf_token=csrf_suspend
                                            />
                                            <AdminActionModal
                                                id=format!("reset-password-{reset_id}")
                                                trigger_label="Reset password"
                                                title="Reset local account password?"
                                                message=format!(
                                                    "Reset the password for {username}? The current password will stop working immediately."
                                                )
                                                form_action=format!("/admin/accounts/{reset_id}/reset-password")
                                                csrf_token=csrf_reset
                                            />
                                        </td>
                                    </tr>
                                }
                            }).collect_view()}
                        </tbody>
                    </table>
                </div>
                <AdminAccountPagination
                    previous_cursor
                    next_cursor
                    query=search_value
                    sort
                    direction
                    path
                />
            </AdminPanel>
        </section>
    }
    .into_any()
}

fn remote_accounts_content(
    accounts: UiAdminAccounts,
    search_value: String,
    sort: UiAdminAccountSort,
    direction: UiAdminAccountSortDirection,
    path: &'static str,
) -> AnyView {
    let csrf_actions = accounts.csrf_token;
    let previous_cursor = accounts.previous_cursor;
    let next_cursor = accounts.next_cursor;
    let account_rows = accounts.accounts;
    let accounts_empty = account_rows.is_empty();
    let search_input_value = search_value.clone();
    view! {
        <section class="pb-8">
            <AdminPanel title="Remote accounts">
                <form class="fieldset max-w-xl gap-4" method="get" action=path>
                    <input type="hidden" name="sort" value=sort.as_str()/>
                    <input type="hidden" name="order" value=direction.as_str()/>
                    <FormField label="Search remote accounts">
                        <input
                            class="input w-full"
                            name="q"
                            value=search_input_value
                            placeholder="Username, display name, or domain"
                        />
                    </FormField>
                    <div class="card-actions">
                        <button class="btn btn-primary" type="submit">"Search"</button>
                        <a
                            class="btn btn-ghost"
                            href=admin_accounts_url(path, "", sort, direction, None)
                        >
                            "Clear"
                        </a>
                    </div>
                </form>
                <div class="overflow-x-auto">
                    <table class="table table-zebra">
                        <thead>
                            <tr>
                                <AdminAccountSortHeader
                                    label="Account"
                                    column=UiAdminAccountSort::Account
                                    active_sort=sort
                                    direction
                                    query=search_value.clone()
                                    path
                                />
                                <AdminAccountSortHeader
                                    label="State"
                                    column=UiAdminAccountSort::State
                                    active_sort=sort
                                    direction
                                    query=search_value.clone()
                                    path
                                />
                                <AdminAccountSortHeader
                                    label="Created"
                                    column=UiAdminAccountSort::CreatedAt
                                    active_sort=sort
                                    direction
                                    query=search_value.clone()
                                    path
                                />
                                <th>"Actions"</th>
                            </tr>
                        </thead>
                        <tbody>
                            {accounts_empty.then(|| view! {
                                <tr><td colspan="4">"No remote accounts match this search."</td></tr>
                            })}
                            {account_rows.into_iter().map(|account| {
                                let account_id = account.id.to_string();
                                let csrf_limit = csrf_actions.clone();
                                let csrf_suspend = csrf_actions.clone();
                                let (action, title) = if account.limited {
                                    ("Unlimit", "Unlimit remote account?")
                                } else {
                                    ("Limit", "Limit remote account?")
                                };
                                let handle = account.domain.as_ref().map_or_else(
                                    || account.username.clone(),
                                    |domain| format!("{}@{domain}", account.username),
                                );
                                let (suspend_action, suspend_title) = if account.suspended {
                                    ("Unsuspend", "Unsuspend remote account?")
                                } else {
                                    ("Suspend", "Suspend remote account?")
                                };
                                view! {
                                    <tr>
                                        <td>
                                            <strong>{handle.clone()}</strong>
                                            <br/>
                                            <small>{account.display_name}</small>
                                        </td>
                                        <td>{if account.suspended { "Suspended" } else if account.limited { "Limited" } else { "Active" }}</td>
                                        <td><time datetime=account.created_at.clone()>{account.created_at.clone()}</time></td>
                                        <td>
                                            <AdminActionModal
                                                id=format!("limit-{account_id}")
                                                trigger_label=action
                                                title
                                                message=format!(
                                                    "Are you sure you want to {} {handle}?",
                                                    action.to_lowercase(),
                                                )
                                                form_action=format!("/admin/accounts/{account_id}/limit")
                                                csrf_token=csrf_limit
                                                limited=!account.limited
                                            />
                                            <AdminActionModal
                                                id=format!("suspend-{account_id}")
                                                trigger_label=suspend_action
                                                title=suspend_title
                                                message=format!(
                                                    "{} {handle}? Suspension purges cached content and severs follows.",
                                                    suspend_action,
                                                )
                                                form_action=format!("/admin/accounts/{account_id}/suspend")
                                                csrf_token=csrf_suspend
                                            />
                                        </td>
                                    </tr>
                                }
                            }).collect_view()}
                        </tbody>
                    </table>
                </div>
                <AdminAccountPagination
                    previous_cursor
                    next_cursor
                    query=search_value
                    sort
                    direction
                    path
                />
            </AdminPanel>
        </section>
    }
    .into_any()
}

fn admin_domain_blocks_content(domain_blocks: UiAdminDomainBlocks) -> AnyView {
    let create_csrf = domain_blocks.csrf_token.clone();
    let action_csrf = domain_blocks.csrf_token;
    view! {
        <section class="grid gap-6 pb-8">
            <p class="text-base-content/70">
                "Domain rules are stored in the database and apply consistently to every Roosty process."
            </p>
            <AdminPanel title="Add domain rule">
                <form class="fieldset grid gap-4 lg:grid-cols-2" method="post" action="/admin/federation">
                    <input type="hidden" name="csrf_token" value=create_csrf/>
                    <FormField label="Domain">
                        <input class="input w-full" name="domain" placeholder="example.org" required/>
                    </FormField>
                    <FormField label="Severity">
                        <select class="select w-full" name="severity">
                            <option value="noop">"No-op"</option>
                            <option value="silence">"Limit"</option>
                            <option value="suspend">"Suspend"</option>
                        </select>
                    </FormField>
                    <FormField label="Public comment">
                        <input class="input w-full" name="public_comment"/>
                    </FormField>
                    <FormField label="Private comment">
                        <input class="input w-full" name="private_comment"/>
                    </FormField>
                    <label class="label justify-start gap-3">
                        <input class="checkbox" name="reject_media" type="checkbox" value="true"/>
                        <span>"Reject media"</span>
                    </label>
                    <label class="label justify-start gap-3">
                        <input class="checkbox" name="reject_reports" type="checkbox" value="true"/>
                        <span>"Reject reports"</span>
                    </label>
                    <label class="label justify-start gap-3">
                        <input class="checkbox" name="obfuscate" type="checkbox" value="true"/>
                        <span>"Obfuscate domain in public listings"</span>
                    </label>
                    <div><button class="btn btn-primary" type="submit">"Add rule"</button></div>
                </form>
            </AdminPanel>
            <AdminPanel title="Domain rules">
                <div class="grid gap-4">
                    {domain_blocks.domain_blocks.into_iter().map(|block| {
                        let id = block.id;
                        let csrf = action_csrf.clone();
                        view! {
                            <form
                                class="card border-base-300 border"
                                method="post"
                                action=format!("/admin/federation/{id}")
                            >
                                <div class="card-body grid gap-4 lg:grid-cols-2">
                                    <h3 class="card-title lg:col-span-2">{block.domain}</h3>
                                    <input type="hidden" name="csrf_token" value=csrf/>
                                    <FormField label="Severity">
                                        <select class="select w-full" name="severity">
                                            <option value="noop" selected=block.severity == "noop">"No-op"</option>
                                            <option value="silence" selected=block.severity == "silence">"Limit"</option>
                                            <option value="suspend" selected=block.severity == "suspend">"Suspend"</option>
                                        </select>
                                    </FormField>
                                    <FormField label="Public comment">
                                        <input class="input w-full" name="public_comment" value=block.public_comment/>
                                    </FormField>
                                    <FormField label="Private comment">
                                        <input class="input w-full" name="private_comment" value=block.private_comment/>
                                    </FormField>
                                    <label class="label justify-start gap-3">
                                        <input class="checkbox" name="reject_media" type="checkbox" value="true" checked=block.reject_media/>
                                        <span>"Reject media"</span>
                                    </label>
                                    <label class="label justify-start gap-3">
                                        <input class="checkbox" name="reject_reports" type="checkbox" value="true" checked=block.reject_reports/>
                                        <span>"Reject reports"</span>
                                    </label>
                                    <label class="label justify-start gap-3">
                                        <input class="checkbox" name="obfuscate" type="checkbox" value="true" checked=block.obfuscate/>
                                        <span>"Obfuscate"</span>
                                    </label>
                                    <div class="card-actions lg:col-span-2">
                                        <button class="btn btn-primary" type="submit">"Save"</button>
                                        <button
                                            class="btn btn-error"
                                            type="submit"
                                            name="operation"
                                            value="delete"
                                        >
                                            "Delete"
                                        </button>
                                    </div>
                                </div>
                            </form>
                        }
                    }).collect_view()}
                </div>
            </AdminPanel>
        </section>
    }
    .into_any()
}

fn admin_moderation_content(moderation: UiAdminModeration) -> AnyView {
    let create_csrf = moderation.csrf_token.clone();
    let rule_csrf = moderation.csrf_token.clone();
    let report_csrf = moderation.csrf_token;
    view! {
        <section class="grid gap-6 pb-8">
            <AdminPanel title="Instance rules">
                <form class="flex gap-3" method="post" action="/admin/moderation/rules">
                    <input type="hidden" name="csrf_token" value=create_csrf/>
                    <input
                        class="input flex-1"
                        name="text"
                        maxlength="300"
                        placeholder="Describe prohibited conduct"
                        required
                    />
                    <button class="btn btn-primary" type="submit">"Add rule"</button>
                </form>
                <div class="mt-4 grid gap-3">
                    {moderation.rules.into_iter().map(|rule| {
                        let csrf = rule_csrf.clone();
                        view! {
                            <form
                                class="flex gap-3"
                                method="post"
                                action=format!("/admin/moderation/rules/{}", rule.id)
                            >
                                <input type="hidden" name="csrf_token" value=csrf/>
                                <input class="input flex-1" name="text" maxlength="300" value=rule.text required/>
                                <button class="btn btn-primary" type="submit">"Save"</button>
                                <button class="btn" name="operation" value="up" type="submit">"↑"</button>
                                <button class="btn" name="operation" value="down" type="submit">"↓"</button>
                                <button class="btn btn-error" name="operation" value="delete" type="submit">"Delete"</button>
                            </form>
                        }
                    }).collect_view()}
                </div>
            </AdminPanel>
            <AdminPanel title="Reports">
                <div class="grid gap-4">
                    {moderation.reports.into_iter().map(|report| {
                        let csrf = report_csrf.clone();
                        let action = format!("/admin/moderation/reports/{}", report.id);
                        view! {
                            <article class="card border-base-300 border">
                                <div class="card-body gap-3">
                                    <div class="flex flex-wrap items-center gap-2">
                                        <h3 class="card-title">{format!("{} → {}", report.source, report.target)}</h3>
                                        <span class="badge">{report.category}</span>
                                        {report.assigned.then(|| view! { <span class="badge badge-info">"Assigned"</span> })}
                                    </div>
                                    <p class="whitespace-pre-wrap">{report.comment}</p>
                                    <p class="text-sm text-base-content/70">
                                        {format!("{} reported post(s)", report.status_ids.len())}
                                    </p>
                                    <div class="flex flex-wrap gap-2">
                                        {report.status_ids.into_iter().map(|status_id| {
                                            let csrf = report_csrf.clone();
                                            view! {
                                                <form
                                                    method="post"
                                                    action=format!("/admin/moderation/reports/{}/statuses/{status_id}/delete", report.id)
                                                >
                                                    <input type="hidden" name="csrf_token" value=csrf/>
                                                    <button class="btn btn-xs btn-error" type="submit">
                                                        {format!("Remove post {status_id}")}
                                                    </button>
                                                </form>
                                            }
                                        }).collect_view()}
                                    </div>
                                    <form class="card-actions" method="post" action=action>
                                        <input type="hidden" name="csrf_token" value=csrf/>
                                        <button class="btn btn-sm" name="operation" value="assign" type="submit">"Assign to me"</button>
                                        <button
                                            class="btn btn-sm btn-primary"
                                            name="operation"
                                            value=if report.resolved { "reopen" } else { "resolve" }
                                            type="submit"
                                        >
                                            {if report.resolved { "Reopen" } else { "Resolve" }}
                                        </button>
                                    </form>
                                    <div class="card-actions">
                                        <form method="post" action=format!("/admin/accounts/{}/limit", report.target_id)>
                                            <input type="hidden" name="csrf_token" value=report_csrf.clone()/>
                                            <input type="hidden" name="limited" value="true"/>
                                            <button class="btn btn-sm btn-warning" type="submit">"Limit target"</button>
                                        </form>
                                        <form method="post" action=format!("/admin/accounts/{}/suspend", report.target_id)>
                                            <input type="hidden" name="csrf_token" value=report_csrf.clone()/>
                                            <button class="btn btn-sm btn-error" type="submit">"Suspend target"</button>
                                        </form>
                                    </div>
                                </div>
                            </article>
                        }
                    }).collect_view()}
                </div>
            </AdminPanel>
        </section>
    }
    .into_any()
}

fn admin_audit_log_content(audit_log: UiAdminAuditLog) -> AnyView {
    view! {
        <section class="pb-8">
            <AdminPanel title="Recent administrator activity">
                <ul class="list">
                    {audit_log.audit_entries.into_iter().map(|entry| view! {
                        <li class="list-row">
                            <time class="text-base-content/70">{entry.created_at}</time>
                            <strong>{entry.action}</strong>
                            <code>{entry.target_id}</code>
                            <span class="text-base-content/70">{entry.source}</span>
                        </li>
                    }).collect_view()}
                </ul>
            </AdminPanel>
        </section>
    }
    .into_any()
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
            <PageCard kind=PageCardKind::Form>
                <PageCardTitle context="Account access">"Sign in"</PageCardTitle>
                {error.map(|error| view! {
                    <Notice kind=NoticeKind::Error>{login_error_message(error)}</Notice>
                })}
                <form class="fieldset gap-4" method="post" action="/login">
                    <input type="hidden" name="next" value=next/>
                    <FormField label="Username or email">
                        <input class="input w-full" name="login" autocomplete="username" required autofocus/>
                    </FormField>
                    <FormField label="Password">
                        <input
                            class="input w-full"
                            name="password"
                            type="password"
                            autocomplete="current-password"
                            required
                        />
                    </FormField>
                    <div class="card-actions">
                        <button class="btn btn-primary" type="submit">"Sign in"</button>
                    </div>
                </form>
            </PageCard>
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
                                <PageCard kind=PageCardKind::Form>
                                    <PageCardTitle>"Sign in required"</PageCardTitle>
                                    <p>"Sign in before changing your password."</p>
                                    <div class="card-actions">
                                        <a class="btn btn-primary" href="/login?next=%2Fauth%2Fedit" rel="external">"Sign in"</a>
                                    </div>
                                </PageCard>
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
        <PageCard kind=PageCardKind::Form>
            <PageCardTitle context="Account security">"Change password"</PageCardTitle>
            {notice.map(|(message, success)| {
                let kind = if success { NoticeKind::Success } else { NoticeKind::Error };
                view! { <Notice kind>{message}</Notice> }
            })}
            <form class="fieldset gap-4" method="post" action="/auth">
                <FormField label="Current password">
                    <input
                        class="input w-full"
                        name="user[current_password]"
                        type="password"
                        autocomplete="current-password"
                        required
                        autofocus
                    />
                </FormField>
                <FormField label="New password">
                    <input
                        class="input w-full"
                        name="user[password]"
                        type="password"
                        autocomplete="new-password"
                        minlength="8"
                        required
                    />
                </FormField>
                <FormField label="Confirm new password">
                    <input
                        class="input w-full"
                        name="user[password_confirmation]"
                        type="password"
                        autocomplete="new-password"
                        minlength="8"
                        required
                    />
                </FormField>
                <div class="card-actions">
                    <button class="btn btn-primary" type="submit">"Change password"</button>
                </div>
            </form>
        </PageCard>
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
        <Page>
            <Hero>
                <PageTitle eyebrow="Welcome to">{name}</PageTitle>
                <p class="text-base-content/70 text-lg">{description}</p>
                <div class="card-actions">
                    <A attr:class="btn btn-primary" href="/about">"About this instance"</A>
                </div>
            </Hero>
        </Page>
    }
    .into_any()
}

fn about_content(name: String, description: String) -> AnyView {
    view! {
        <Page>
            <article>
                <PageTitle>"About " {name}</PageTitle>
                <div class="mt-8 flex max-w-3xl flex-col gap-6">
                    <p>{description}</p>
                    <p>
                        "This instance is part of the decentralized social web. People can connect across compatible servers without needing an account on the same site."
                    </p>
                </div>
            </article>
        </Page>
    }
    .into_any()
}

#[component]
fn PageFrame(
    bootstrap: BootstrapResource,
    login_next: &'static str,
    #[prop(default = false)] wide: bool,
    children: Children,
) -> impl IntoView {
    let brand = view! {
        <Suspense fallback=|| view! { <A attr:class="btn btn-ghost text-xl" href="/">"Roosty"</A> }>
            {move || bootstrap.get().map(instance_brand)}
        </Suspense>
    }
    .into_any();

    let main_class = if wide {
        "w-full grow"
    } else {
        "mx-auto w-full max-w-6xl grow px-4"
    };

    view! {
        <div class="bg-base-200 flex min-h-screen flex-col">
            <SiteHeader brand>
                <A attr:class="btn btn-ghost" href="/about">"About"</A>
                <Suspense fallback=move || view! { <span class="loading loading-dots loading-sm" aria-label="Checking session"></span> }>
                    {move || {
                        bootstrap
                            .get()
                            .map(|result| session_navigation(result, login_next))
                    }}
                </Suspense>
            </SiteHeader>
            <main class=main_class>{children()}</main>
            <Suspense fallback=|| view! { <SiteFooter/> }>
                {move || bootstrap.get().map(version_footer)}
            </Suspense>
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
    view! { <A attr:class="btn btn-ghost text-xl" href="/">{name}</A> }.into_any()
}

fn version_footer(result: Result<UiBootstrap, ServerFnError>) -> AnyView {
    match result {
        Ok(bootstrap) => {
            view! { <SiteFooter build_identifier=bootstrap.build_identifier/> }.into_any()
        }
        Err(_) => view! { <SiteFooter/> }.into_any(),
    }
}

fn session_navigation(
    result: Result<UiBootstrap, ServerFnError>,
    login_next: &'static str,
) -> AnyView {
    match result {
        Ok(bootstrap) => match bootstrap.account {
            Some(account) => {
                let is_admin = account.is_admin;
                view! {
                    {is_admin.then(|| view! { <A attr:class="btn btn-ghost" href="/admin">"Admin"</A> })}
                    <AccountMenu
                        username=account.username
                        display_name=account.display_name
                        avatar_url=account.avatar_url
                    />
                }
                .into_any()
            }
            None => {
                let href = format!("/login?next={login_next}");
                view! {
                    <a class="btn btn-ghost" href=href rel="external">"Sign in"</a>
                }
                .into_any()
            }
        },
        Err(_) => {
            view! { <span class="badge badge-warning">"Session unavailable"</span> }.into_any()
        }
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
        <main class="card border-base-300 bg-base-100 mx-auto my-12 w-full max-w-2xl border shadow-xl">
            <div class="card-body">
                <Title text="Page not found · Roosty"/>
                <PageCardTitle>"Page not found"</PageCardTitle>
                <div class="card-actions">
                    <A attr:class="btn btn-primary" href="/">"Return home"</A>
                </div>
            </div>
        </main>
    }
}
