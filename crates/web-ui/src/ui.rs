use leptos::prelude::*;

/// Administrator category selected in the shared navigation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AdminSection {
    WorkQueue,
    Accounts,
    AuditLog,
}

/// Render the responsive administrator drawer and category navigation.
#[component]
pub(crate) fn AdminLayout(active: AdminSection, children: Children) -> impl IntoView {
    let current = |section| (active == section).then_some("page");
    let selected = |section| (active == section).then_some("menu-active");

    view! {
        <div class="drawer lg:drawer-open">
            <input id="admin-navigation" type="checkbox" class="drawer-toggle"/>
            <div class="drawer-content">
                <div class="mx-auto w-full max-w-5xl px-4">
                    <div class="pt-4 lg:hidden">
                        <label for="admin-navigation" class="btn btn-outline drawer-button">
                            "Administration menu"
                        </label>
                    </div>
                    {children()}
                </div>
            </div>
            <div class="drawer-side">
                <label
                    for="admin-navigation"
                    aria-label="Close administration menu"
                    class="drawer-overlay"
                ></label>
                <nav aria-label="Administration">
                    <ul class="menu bg-base-200 min-h-full w-64 p-4">
                        <li class="menu-title">"Administration"</li>
                        <li>
                            <a
                                href="/admin"
                                class=selected(AdminSection::WorkQueue)
                                aria-current=current(AdminSection::WorkQueue)
                            >
                                "Work queue"
                            </a>
                        </li>
                        <li>
                            <a
                                href="/admin/accounts"
                                class=selected(AdminSection::Accounts)
                                aria-current=current(AdminSection::Accounts)
                            >
                                "Accounts"
                            </a>
                        </li>
                        <li>
                            <a
                                href="/admin/audit-log"
                                class=selected(AdminSection::AuditLog)
                                aria-current=current(AdminSection::AuditLog)
                            >
                                "Audit log"
                            </a>
                        </li>
                    </ul>
                </nav>
            </div>
        </div>
    }
}

/// Render the shared daisyUI navigation shell around a caller-provided brand and actions.
#[component]
pub(crate) fn SiteHeader(brand: AnyView, children: Children) -> impl IntoView {
    view! {
        <header class="border-base-300 bg-base-100 border-b">
            <div class="navbar mx-auto max-w-6xl flex-col items-stretch gap-2 px-4 sm:flex-row sm:items-center">
                <div class="navbar-start w-full min-w-0 sm:w-1/2">{brand}</div>
                <nav
                    class="navbar-end w-full flex-wrap justify-start gap-2 sm:w-1/2 sm:justify-end"
                    aria-label="Primary navigation"
                >
                    {children()}
                </nav>
            </div>
        </header>
    }
}

/// Render the shared single-line project attribution footer.
#[component]
pub(crate) fn SiteFooter(#[prop(optional)] build_identifier: Option<String>) -> impl IntoView {
    view! {
        <footer class="footer footer-center border-base-300 bg-base-100 border-t p-6 text-base-content/70">
            <p class="inline-flex flex-row items-center gap-1 whitespace-nowrap">
                <span>"Powered by"</span>
                <a href="https://github.com/ctron/roosty">"Roosty"</a>
                {build_identifier.map(|build_identifier| view! { <span>{build_identifier}</span> })}
            </p>
        </footer>
    }
}

/// Render the account actions as a consistently aligned native details menu.
#[component]
pub(crate) fn AccountMenu(
    username: String,
    display_name: String,
    avatar_url: Option<String>,
) -> impl IntoView {
    let initial = display_name
        .chars()
        .find(|character| !character.is_whitespace())
        .or_else(|| username.chars().next())
        .map(|character| character.to_uppercase().to_string())
        .unwrap_or_else(|| "?".to_owned());

    view! {
        <details class="dropdown dropdown-end">
            <summary class="btn btn-ghost" title=display_name>
                {avatar_url.map_or_else(
                    || view! {
                        <span class="avatar placeholder" aria-hidden="true">
                            <span class="bg-primary text-primary-content w-8 rounded-full">{initial}</span>
                        </span>
                    }.into_any(),
                    |avatar_url| view! {
                        <span class="avatar">
                            <span class="w-8 rounded-full"><img src=avatar_url alt=""/></span>
                        </span>
                    }.into_any(),
                )}
                <span>{username}</span>
            </summary>
            <form method="post" action="/logout">
                <ul class="menu dropdown-content rounded-box border-base-300 bg-base-100 z-10 mt-2 w-52 border p-2 shadow">
                    <li><a href="/auth/edit" rel="external">"Account"</a></li>
                    <li><button type="submit">"Log out"</button></li>
                </ul>
            </form>
        </details>
    }
}

/// Layout variants supported by the shared page card.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum PageCardKind {
    #[default]
    Standard,
    Form,
}

/// Semantic severity used by the shared notice component.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NoticeKind {
    Error,
    Success,
    #[cfg(feature = "ssr")]
    Warning,
}

/// Constrain standard page content to the shared readable width and vertical rhythm.
#[component]
pub(crate) fn Page(children: Children) -> impl IntoView {
    view! {
        <div class="mx-auto w-full max-w-4xl py-12 sm:py-20">{children()}</div>
    }
}

/// Render a consistently structured daisyUI card for top-level page content.
#[component]
pub(crate) fn PageCard(
    #[prop(default = PageCardKind::Standard)] kind: PageCardKind,
    children: Children,
) -> impl IntoView {
    let class = match kind {
        PageCardKind::Standard => {
            "card border-base-300 bg-base-100 mx-auto my-6 w-full max-w-3xl border shadow-xl sm:my-12"
        }
        PageCardKind::Form => {
            "card border-base-300 bg-base-100 mx-auto my-6 w-full max-w-xl border shadow-xl sm:my-12"
        }
    };

    view! {
        <section class=class>
            <div class="card-body">{children()}</div>
        </section>
    }
}

/// Render prominent welcome content with the daisyUI hero structure.
#[component]
pub(crate) fn Hero(children: Children) -> impl IntoView {
    view! {
        <section class="hero bg-base-100 w-full rounded-box shadow-xl">
            <div class="hero-content py-12 sm:py-20">
                <div class="flex max-w-2xl flex-col items-start gap-4">{children()}</div>
            </div>
        </section>
    }
}

/// Render the primary page heading with optional supporting context.
#[component]
pub(crate) fn PageTitle(
    #[prop(optional)] eyebrow: Option<&'static str>,
    children: Children,
) -> impl IntoView {
    view! {
        <header>
            {eyebrow.map(|eyebrow| view! {
                <p class="text-primary text-sm font-semibold uppercase tracking-wide">{eyebrow}</p>
            })}
            <h1 class="text-3xl font-bold sm:text-4xl">{children()}</h1>
        </header>
    }
}

/// Render an `h1` using the title treatment native to its parent daisyUI card.
#[component]
pub(crate) fn PageCardTitle(
    #[prop(optional)] context: Option<&'static str>,
    children: Children,
) -> impl IntoView {
    view! {
        <header>
            {context.map(|context| view! { <p>{context}</p> })}
            <h1 class="card-title">{children()}</h1>
        </header>
    }
}

/// Render a titled card in the administrator dashboard.
#[component]
pub(crate) fn AdminPanel(title: &'static str, children: Children) -> impl IntoView {
    view! {
        <section class="card border-base-300 bg-base-100 border">
            <div class="card-body">
                <h2 class="card-title">{title}</h2>
                {children()}
            </div>
        </section>
    }
}

/// Render the required confirmation control used by sensitive administrator actions.
#[component]
pub(crate) fn ConfirmationCheckbox() -> impl IntoView {
    view! {
        <label class="label gap-2">
            <input class="checkbox checkbox-sm" type="checkbox" required/>
            <span>"Confirm"</span>
        </label>
    }
}

/// Render a consistently styled status or error notice.
#[component]
pub(crate) fn Notice(kind: NoticeKind, children: Children) -> impl IntoView {
    let (class, role) = match kind {
        NoticeKind::Error => ("alert alert-error", "alert"),
        NoticeKind::Success => ("alert alert-success", "status"),
        #[cfg(feature = "ssr")]
        NoticeKind::Warning => ("alert alert-warning", "status"),
    };

    view! {
        <div class=class role=role>
            <span>{children()}</span>
        </div>
    }
}

/// Wrap a native form control with the shared daisyUI field structure.
#[component]
pub(crate) fn FormField(label: &'static str, children: Children) -> impl IntoView {
    view! {
        <label class="fieldset">
            <span class="fieldset-legend">{label}</span>
            {children()}
        </label>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_layout_exposes_all_categories_in_a_responsive_drawer() {
        let html = view! {
            <AdminLayout active=AdminSection::Accounts>
                <p>"Account content"</p>
            </AdminLayout>
        }
        .to_html();

        assert!(html.contains("class=\"drawer lg:drawer-open\""));
        assert!(html.contains("class=\"drawer-toggle\""));
        assert!(html.contains("class=\"btn btn-outline drawer-button\""));
        assert!(html.contains("href=\"/admin\""));
        assert!(html.contains("href=\"/admin/accounts\""));
        assert!(html.contains("href=\"/admin/audit-log\""));
        assert!(html.contains("class=\"menu bg-base-200 min-h-full w-64 p-4\""));
        assert!(html.contains("class=\"menu-title\""));
        assert!(html.contains("aria-label=\"Administration\""));
        assert_eq!(html.matches("aria-current=\"page\"").count(), 1);
        assert_eq!(html.matches("menu-active").count(), 1);
    }

    #[test]
    fn site_header_owns_the_shared_navbar_structure() {
        let html = view! {
            <SiteHeader brand=view! { <a class="btn btn-ghost text-xl" href="/">"Roosty"</a> }.into_any()>
                <a class="btn btn-ghost btn-sm" href="/about">"About"</a>
            </SiteHeader>
        }
        .to_html();

        assert!(html.contains("<header class=\"border-base-300 bg-base-100 border-b\">"));
        assert!(html.contains("class=\"navbar mx-auto max-w-6xl"));
        assert!(html.contains("class=\"navbar-start w-full min-w-0 sm:w-1/2\""));
        assert!(html.contains("class=\"navbar-end w-full flex-wrap justify-start gap-2"));
        assert!(html.contains("aria-label=\"Primary navigation\""));
    }

    #[test]
    fn account_menu_uses_an_initial_and_preserves_native_account_actions() {
        let html = view! {
            <AccountMenu
                username="alice".to_owned()
                display_name=" Alice Example".to_owned()
                avatar_url=None
            />
        }
        .to_html();

        assert!(html.contains("<details class=\"dropdown dropdown-end\">"));
        assert!(html.contains("class=\"btn btn-ghost\""));
        assert!(html.contains(">A</span>"));
        assert!(html.contains("class=\"menu dropdown-content rounded-box"));
        assert!(html.contains("<li><a href=\"/auth/edit\" rel=\"external\">Account</a></li>"));
        assert!(html.contains("<li><button type=\"submit\">Log out</button></li>"));
        assert!(html.contains("href=\"/auth/edit\" rel=\"external\""));
        assert!(html.contains("method=\"post\" action=\"/logout\""));
        assert!(!html.contains("btn btn-ghost btn-sm justify-start"));
    }

    #[test]
    fn account_menu_renders_a_supplied_avatar() {
        let html = view! {
            <AccountMenu
                username="alice".to_owned()
                display_name="Alice Example".to_owned()
                avatar_url=Some("/avatars/alice.png".to_owned())
            />
        }
        .to_html();

        assert!(html.contains("<img src=\"/avatars/alice.png\" alt=\"\">"));
        assert!(!html.contains("avatar placeholder"));
    }

    #[test]
    fn site_footer_keeps_attribution_on_one_line() {
        let html = view! {
            <SiteFooter build_identifier="v1.2.3".to_owned()/>
        }
        .to_html();

        assert!(html.contains("<footer class=\"footer footer-center"));
        assert!(
            html.contains(
                "<p class=\"inline-flex flex-row items-center gap-1 whitespace-nowrap\">"
            )
        );
        assert!(html.contains("<span>Powered by</span>"));
        assert!(html.contains(">Roosty</a>"));
        assert!(html.contains("<span>v1.2.3</span>"));
    }

    #[test]
    fn error_notices_use_alert_semantics() {
        let html = view! {
            <Notice kind=NoticeKind::Error>"Something failed"</Notice>
        }
        .to_html();

        assert!(html.contains("class=\"alert alert-error\""));
        assert!(html.contains("role=\"alert\""));
    }

    #[test]
    fn success_notices_use_status_semantics() {
        let html = view! {
            <Notice kind=NoticeKind::Success>"Saved"</Notice>
        }
        .to_html();

        assert!(html.contains("class=\"alert alert-success\""));
        assert!(html.contains("role=\"status\""));
    }

    #[test]
    fn page_card_variants_own_their_layout_classes() {
        for (kind, expected_width) in [
            (PageCardKind::Standard, "max-w-3xl"),
            (PageCardKind::Form, "max-w-xl"),
        ] {
            let html = view! { <PageCard kind>"Content"</PageCard> }.to_html();

            assert!(html.contains("class=\"card"));
            assert!(html.contains("border-base-300 bg-base-100"));
            assert!(html.contains(expected_width));
            assert!(html.contains("<div class=\"card-body\">Content</div>"));
        }
    }

    #[test]
    fn page_owns_the_standard_content_width_and_spacing() {
        let html = view! { <Page>"Content"</Page> }.to_html();

        assert_eq!(
            html,
            "<div class=\"mx-auto w-full max-w-4xl py-12 sm:py-20\">Content</div>"
        );
    }

    #[test]
    fn hero_uses_the_daisyui_hero_structure() {
        let html = view! { <Hero>"Welcome"</Hero> }.to_html();

        assert!(html.contains("<section class=\"hero bg-base-100"));
        assert!(html.contains("<div class=\"hero-content"));
        assert!(html.contains("flex max-w-2xl flex-col items-start gap-4"));
        assert!(!html.contains("text-center"));
        assert!(!html.contains("items-center"));
        assert!(html.contains(">Welcome</div>"));
    }

    #[test]
    fn page_title_and_admin_panel_preserve_semantic_headings() {
        let title = view! {
            <PageTitle eyebrow="Account access">"Sign in"</PageTitle>
        }
        .to_html();
        let panel = view! {
            <AdminPanel title="Accounts">"Panel content"</AdminPanel>
        }
        .to_html();

        assert!(title.starts_with("<header>"));
        assert!(title.contains(
            "class=\"text-primary text-sm font-semibold uppercase tracking-wide\">Account access</p>"
        ));
        assert!(title.contains("class=\"text-3xl font-bold sm:text-4xl\">Sign in</h1>"));
        assert!(!title.contains("badge"));
        assert!(!title.contains("card-title"));
        assert!(panel.contains("<h2 class=\"card-title\">Accounts</h2>"));
        assert!(panel.contains("Panel content"));
    }

    #[test]
    fn page_card_title_uses_the_native_card_title_treatment() {
        let title = view! {
            <PageCardTitle context="About this instance">"Roosty"</PageCardTitle>
        }
        .to_html();

        assert!(title.contains("<p>About this instance</p>"));
        assert!(title.contains("<h1 class=\"card-title\">Roosty</h1>"));
        assert!(!title.contains("badge"));
    }

    #[test]
    fn page_title_omits_absent_supporting_context() {
        let title = view! { <PageTitle>"Page not found"</PageTitle> }.to_html();

        assert_eq!(
            title,
            "<header><!><h1 class=\"text-3xl font-bold sm:text-4xl\">Page not found</h1></header>"
        );
    }

    #[test]
    fn stylesheet_contains_only_framework_loading_base_scale_and_color_themes() {
        let stylesheet = include_str!("../style/main.css");

        assert!(!stylesheet.lines().any(|line| line.starts_with('.')));
        assert!(!stylesheet.contains("@theme"));
        assert!(stylesheet.contains("font-size: 106.25%"));
        assert!(stylesheet.contains("name: \"light\""));
        assert!(stylesheet.contains("name: \"dark\""));
        assert!(stylesheet.contains("--color-primary: #246b57"));
        assert!(stylesheet.contains("--color-primary: #71cfb1"));
        assert!(!stylesheet.contains("--radius-"));
        assert!(!stylesheet.contains("--depth:"));
    }

    #[cfg(feature = "ssr")]
    #[test]
    fn confirmation_and_warning_components_preserve_accessibility_contracts() {
        let checkbox = view! { <ConfirmationCheckbox/> }.to_html();
        let warning = view! {
            <Notice kind=NoticeKind::Warning>"Keep this private"</Notice>
        }
        .to_html();

        assert!(checkbox.contains("class=\"checkbox checkbox-sm\""));
        assert!(checkbox.contains("type=\"checkbox\" required"));
        assert!(warning.contains("class=\"alert alert-warning\""));
        assert!(warning.contains("role=\"status\""));
    }
}
