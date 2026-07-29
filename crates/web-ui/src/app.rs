use leptos::prelude::*;
use leptos_meta::{HashedStylesheet, Link, Meta, MetaTags, Title, provide_meta_context};
use leptos_router::{
    components::{A, Route, Router, Routes},
    hooks::use_query_map,
    path,
};
use serde::{Serialize, de::DeserializeOwned};

use crate::{
    bootstrap::{
        UiAdminAccountOrigin, UiAdminAccounts, UiAdminAuditLog, UiAdminDomainBlocks,
        UiAdminWorkQueue, UiBootstrap, load_admin_accounts, load_admin_audit_log,
        load_admin_domain_blocks, load_admin_work_queue, load_bootstrap,
    },
    forms::{LoginError, PasswordChangeResult},
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
        let hash_path = std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
            .unwrap_or_default()
            .join(options.hash_file.as_ref());
        if let Ok(hashes) = std::fs::read_to_string(hash_path)
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
                <Route path=path!("admin/audit-log") view=AdminAuditLogPage/>
            </Routes>
        </Router>
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
    let query = use_query_map().get().get("q").unwrap_or_default();
    let search_value = query.clone();
    let accounts = Resource::new_blocking(
        move || (query.clone(), origin),
        |(query, origin)| load_admin_accounts(query, origin),
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
                                admin_accounts_content(accounts, search_value, origin, path)
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
    path: &'static str,
) -> AnyView {
    match origin {
        UiAdminAccountOrigin::Local => local_accounts_content(accounts, search_value, path),
        UiAdminAccountOrigin::Remote => remote_accounts_content(accounts, search_value, path),
    }
}

fn local_accounts_content(
    accounts: UiAdminAccounts,
    search_value: String,
    path: &'static str,
) -> AnyView {
    let csrf_create = accounts.csrf_token.clone();
    let csrf_actions = accounts.csrf_token;
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
                    <FormField label="Search local accounts">
                        <input
                            class="input w-full"
                            name="q"
                            value=search_value
                            placeholder="Username, display name, or email"
                        />
                    </FormField>
                    <div class="card-actions">
                        <button class="btn btn-primary" type="submit">"Search"</button>
                        <a class="btn btn-ghost" href=path>"Clear"</a>
                    </div>
                </form>
                <div class="overflow-x-auto">
                    <table class="table table-zebra">
                        <thead>
                            <tr>
                                <th>"Account"</th>
                                <th>"Email"</th>
                                <th>"Role"</th>
                                <th>"State"</th>
                                <th>"Actions"</th>
                            </tr>
                        </thead>
                        <tbody>
                            {accounts.accounts.into_iter().map(|account| {
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
            </AdminPanel>
        </section>
    }
    .into_any()
}

fn remote_accounts_content(
    accounts: UiAdminAccounts,
    search_value: String,
    path: &'static str,
) -> AnyView {
    let csrf_actions = accounts.csrf_token;
    view! {
        <section class="pb-8">
            <AdminPanel title="Remote accounts">
                <form class="fieldset max-w-xl gap-4" method="get" action=path>
                    <FormField label="Search remote accounts">
                        <input
                            class="input w-full"
                            name="q"
                            value=search_value
                            placeholder="Username, display name, or domain"
                        />
                    </FormField>
                    <div class="card-actions">
                        <button class="btn btn-primary" type="submit">"Search"</button>
                        <a class="btn btn-ghost" href=path>"Clear"</a>
                    </div>
                </form>
                <div class="overflow-x-auto">
                    <table class="table table-zebra">
                        <thead>
                            <tr>
                                <th>"Account"</th>
                                <th>"State"</th>
                                <th>"Actions"</th>
                            </tr>
                        </thead>
                        <tbody>
                            {accounts.accounts.into_iter().map(|account| {
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
