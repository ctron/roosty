//! Native Axum integration for the server-rendered and hydrated first-party UI.

use std::{
    borrow::Cow,
    future::Future,
    pin::Pin,
    sync::{Arc, OnceLock},
};

use axum::{
    Extension, Form, Router,
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, Request, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::post,
};
use leptos::prelude::provide_context;
use leptos_axum::{AxumRouteListing, LeptosRoutes, generate_route_list};
use roosty_core::{AccountId, Result as RoostyResult, RoostyError, StatusId};
use roosty_db::{
    AccountStatusTimelineOptions, AdminAccount, AdminAuditAction, AdminAuditEntry,
    AdminAuditSource, AdminAuditTargetKind, AdminJobDiagnostic, AdminJobSummary,
    FederationDomainBlock, FederationDomainBlockUpdate, InstanceRule, JobKind, LocalAccount,
    LocalStatus, NewFederationDomainBlock, NewJob, PollStatus, QuoteState, RemoteStatus,
    ReportAccount, ReportListOptions, ReportStatus, StatusContextItem, StatusContextParent,
    StatusReference, StatusVisibility, TimelineCursor,
};
use roosty_web_ui::{
    App, UiAccount, UiAdminAccount, UiAdminAccountOrigin, UiAdminAccounts, UiAdminAuditEntry,
    UiAdminAuditLog, UiAdminDomainBlock, UiAdminDomainBlocks, UiAdminJob, UiAdminJobSummary,
    UiAdminModeration, UiAdminWorkQueue, UiBackend, UiBootstrap, UiFeaturedTag, UiInstanceRule,
    UiMedia, UiMediaKind, UiModerationReport, UiPoll, UiPollOption, UiPreviewCard, UiProfileField,
    UiProfileHeader, UiProfileTab, UiProfileTimeline, UiPublicAccount, UiPublicPageError,
    UiServerContext, UiStatus, UiStatusAuthor, UiStatusPage, UiStatusThread, UiStatusVisibility,
    shell,
};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr};
use serde::Deserialize;
use serde_json::{Value, json};
use strum::ParseError;
use thiserror::Error;
use time::OffsetDateTime;
use tower_http::services::ServeDir;
use uuid::Uuid;

use crate::media::media_url;
use crate::version::build_identifier;
use crate::{
    admin::{self, AdminSource},
    auth::{account_id_from_session, csrf_token_from_session, validate_csrf_token},
    http::{ApiError, ApiResult, AppState, DatabaseContext, TransactionContext},
    statuses::delete_reported_status,
};

static UI_ROUTES: OnceLock<Vec<AxumRouteListing>> = OnceLock::new();

fn ui_routes() -> Vec<AxumRouteListing> {
    UI_ROUTES.get_or_init(|| generate_route_list(App)).clone()
}

/// Mount explicit UI routes, internal server functions, and generated browser assets.
pub fn router(state: &AppState, database: &DatabaseContext) -> Router<AppState> {
    let routes = ui_routes();
    let options = state.leptos_options.clone();
    let context = UiServerContext(Arc::new(RoostyUiBackend {
        state: state.clone(),
        database: database.clone(),
    }));
    let assets =
        ServeDir::new(std::path::Path::new(&*options.site_root).join(&*options.site_pkg_dir));

    Router::new()
        .route("/admin/accounts", post(create_admin_account))
        .route(
            "/admin/accounts/{account_id}/limit",
            post(limit_admin_account),
        )
        .route(
            "/admin/accounts/{account_id}/suspend",
            post(suspend_admin_account),
        )
        .route(
            "/admin/accounts/{account_id}/reset-password",
            post(reset_admin_password),
        )
        .route("/admin/federation", post(create_admin_domain_block))
        .route(
            "/admin/federation/{domain_block_id}",
            post(update_admin_domain_block),
        )
        .route("/admin/moderation/rules", post(create_admin_instance_rule))
        .route(
            "/admin/moderation/rules/{rule_id}",
            post(update_admin_instance_rule),
        )
        .route(
            "/admin/moderation/reports/{report_id}",
            post(update_admin_report),
        )
        .route(
            "/admin/moderation/reports/{report_id}/statuses/{status_id}/delete",
            post(delete_admin_report_status),
        )
        .leptos_routes_with_context(
            state,
            routes,
            move || provide_context(context.clone()),
            move || shell(options.clone()),
        )
        .nest_service("/pkg", assets)
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            protect_ui_route,
        ))
}

async fn protect_ui_route(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    request: Request<Body>,
    next: Next,
) -> ApiResult<Response> {
    let path = request.uri().path();
    if path != "/auth/edit" && !path.starts_with("/admin") {
        return Ok(next.run(request).await);
    }

    let Some(account_id) = account_id_from_session(&state, request.headers())? else {
        return Ok(redirect_login(&state, path));
    };
    if !path.starts_with("/admin") {
        return Ok(next.run(request).await);
    }
    let txn = database.begin_read().await?;
    let account = roosty_db::find_local_account_by_id(&txn, account_id).await?;
    txn.commit().await?;
    let Some(account) = account else {
        return Ok(redirect_login(&state, path));
    };
    if !account.is_admin {
        return Err(ApiError::Forbidden("This action is not allowed".into()));
    }
    Ok(next.run(request).await)
}

fn redirect_login(state: &AppState, next: &str) -> Response {
    let mut location = state.config.public_base_url.clone();
    location.set_path("/login");
    location.set_query(Some(login_return_query(next)));
    location.set_fragment(None);
    Redirect::to(location.as_str()).into_response()
}

fn login_return_query(next: &str) -> &'static str {
    match next {
        "/admin/jobs" => "next=%2Fadmin%2Fjobs",
        "/admin/accounts" => "next=%2Fadmin%2Faccounts",
        "/admin/remote-accounts" => "next=%2Fadmin%2Fremote-accounts",
        "/admin/federation" => "next=%2Fadmin%2Ffederation",
        "/admin/moderation" => "next=%2Fadmin%2Fmoderation",
        "/admin/audit-log" => "next=%2Fadmin%2Faudit-log",
        path if path.starts_with("/admin") => "next=%2Fadmin",
        _ => "next=%2Fauth%2Fedit",
    }
}

#[derive(Clone)]
struct RoostyUiBackend {
    state: AppState,
    database: DatabaseContext,
}

impl UiBackend for RoostyUiBackend {
    fn bootstrap(
        &self,
        cookie_header: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<UiBootstrap, String>> + Send + 'static>> {
        let state = self.state.clone();
        let database = self.database.clone();
        Box::pin(async move {
            let mut headers = HeaderMap::new();
            if let Some(cookie_header) = cookie_header {
                let value =
                    HeaderValue::from_str(&cookie_header).map_err(|error| error.to_string())?;
                headers.insert(header::COOKIE, value);
            }
            let account = match account_id_from_session(&state, &headers)
                .map_err(|error| error.to_string())?
            {
                Some(account_id) => {
                    let txn = database
                        .begin_read()
                        .await
                        .map_err(|error| error.to_string())?;
                    let account = roosty_db::find_local_account_by_id(&txn, account_id)
                        .await
                        .map_err(|error| error.to_string())?;
                    txn.commit().await.map_err(|error| error.to_string())?;
                    account.map(|account| UiAccount {
                        id: account.id.0,
                        username: account.username,
                        display_name: account.display_name,
                        avatar_url: account
                            .avatar_file_path
                            .as_deref()
                            .map(|path| crate::media::media_url(&state, path)),
                        is_admin: account.is_admin,
                    })
                }
                None => None,
            };
            let csrf_token =
                csrf_token_from_session(&state, &headers).map_err(|error| error.to_string())?;
            Ok(UiBootstrap {
                instance_name: state.config.instance_name.clone(),
                instance_description: state.config.instance_description.clone(),
                public_base_url: state.config.public_base_url.to_string(),
                build_identifier: build_identifier(),
                account,
                csrf_token,
            })
        })
    }

    fn profile_header(
        &self,
        _cookie_header: Option<String>,
        username: String,
    ) -> Pin<Box<dyn Future<Output = Result<UiProfileHeader, UiPublicPageError>> + Send + 'static>>
    {
        let state = self.state.clone();
        let database = self.database.clone();
        Box::pin(async move {
            let txn = database
                .begin_snapshot()
                .await
                .map_err(|_| UiPublicPageError::Internal)?;
            let account = active_local_profile(&txn, &username).await?;
            let account_dto = ui_public_account(&state, &txn, &account).await?;
            let featured_tags = roosty_db::local_featured_tags(&txn, account.id)
                .await
                .map_err(|_| UiPublicPageError::Internal)?
                .into_iter()
                .map(|tag| UiFeaturedTag {
                    name: tag.name,
                    statuses_count: u64::try_from(tag.statuses_count).unwrap_or_default(),
                })
                .collect();
            txn.commit()
                .await
                .map_err(|_| UiPublicPageError::Internal)?;
            Ok(UiProfileHeader {
                account: account_dto,
                featured_tags,
                profile_url: public_page_url(&state, &format!("/@{username}")),
                activitypub_url: public_page_url(&state, &format!("/users/{username}")),
            })
        })
    }

    fn profile_timeline(
        &self,
        cookie_header: Option<String>,
        username: String,
        tab: UiProfileTab,
        hashtag: Option<String>,
        max_id: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<UiProfileTimeline, UiPublicPageError>> + Send + 'static>>
    {
        let state = self.state.clone();
        let database = self.database.clone();
        Box::pin(async move {
            let cursor = parse_ui_cursor(max_id.as_deref())?;
            let txn = database
                .begin_snapshot()
                .await
                .map_err(|_| UiPublicPageError::Internal)?;
            let viewer = ui_viewer(&state, &txn, cookie_header).await?;
            let account = active_local_profile(&txn, &username).await?;
            let blocked = match viewer {
                Some(viewer) if viewer != account.id => {
                    roosty_db::local_accounts_are_blocked(&txn, viewer, account.id)
                        .await
                        .map_err(|_| UiPublicPageError::Internal)?
                }
                _ => false,
            };
            let timeline = if blocked {
                UiStatusPage {
                    statuses: Vec::new(),
                    next_cursor: None,
                }
            } else {
                ui_profile_status_page(
                    &state,
                    &txn,
                    &account,
                    viewer,
                    &tab,
                    hashtag.as_deref(),
                    cursor,
                )
                .await?
            };
            let pinned_statuses = if blocked || !matches!(tab, UiProfileTab::Posts) {
                Vec::new()
            } else {
                let pins = roosty_db::pinned_local_statuses_by_account(
                    &txn,
                    account.id,
                    crate::statuses::MAX_PINNED_STATUSES,
                    TimelineCursor::default(),
                )
                .await
                .map_err(|_| UiPublicPageError::Internal)?;
                ui_local_statuses(&state, &txn, pins.items, &account, viewer, true).await?
            };
            txn.commit()
                .await
                .map_err(|_| UiPublicPageError::Internal)?;

            Ok(UiProfileTimeline {
                tab,
                hashtag,
                pinned_statuses,
                timeline,
            })
        })
    }

    fn profile_statuses(
        &self,
        cookie_header: Option<String>,
        username: String,
        tab: UiProfileTab,
        hashtag: Option<String>,
        max_id: String,
    ) -> Pin<Box<dyn Future<Output = Result<UiStatusPage, UiPublicPageError>> + Send + 'static>>
    {
        let state = self.state.clone();
        let database = self.database.clone();
        Box::pin(async move {
            let cursor = parse_ui_cursor(Some(&max_id))?;
            let txn = database
                .begin_snapshot()
                .await
                .map_err(|_| UiPublicPageError::Internal)?;
            let viewer = ui_viewer(&state, &txn, cookie_header).await?;
            let account = active_local_profile(&txn, &username).await?;
            if viewer.is_some_and(|viewer| viewer != account.id)
                && roosty_db::local_accounts_are_blocked(
                    &txn,
                    viewer.unwrap_or(account.id),
                    account.id,
                )
                .await
                .map_err(|_| UiPublicPageError::Internal)?
            {
                return Ok(UiStatusPage {
                    statuses: Vec::new(),
                    next_cursor: None,
                });
            }
            let page = ui_profile_status_page(
                &state,
                &txn,
                &account,
                viewer,
                &tab,
                hashtag.as_deref(),
                cursor,
            )
            .await?;
            txn.commit()
                .await
                .map_err(|_| UiPublicPageError::Internal)?;
            Ok(page)
        })
    }

    fn status_thread(
        &self,
        cookie_header: Option<String>,
        username: String,
        status_id: String,
    ) -> Pin<Box<dyn Future<Output = Result<UiStatusThread, UiPublicPageError>> + Send + 'static>>
    {
        let state = self.state.clone();
        let database = self.database.clone();
        Box::pin(async move {
            let status_id = Uuid::parse_str(&status_id)
                .map(StatusId)
                .map_err(|_| UiPublicPageError::NotFound)?;
            let txn = database
                .begin_snapshot()
                .await
                .map_err(|_| UiPublicPageError::Internal)?;
            let viewer = ui_viewer(&state, &txn, cookie_header).await?;
            let account = active_local_profile(&txn, &username).await?;
            let Some((focus, ancestors, descendants)) =
                crate::statuses::visible_status_thread_on(&txn, status_id, viewer)
                    .await
                    .map_err(|_| UiPublicPageError::Internal)?
            else {
                return Err(UiPublicPageError::NotFound);
            };
            let StatusContextItem::Local(local_focus) = &focus else {
                return Err(UiPublicPageError::NotFound);
            };
            if local_focus.account_id != account.id {
                return Err(UiPublicPageError::NotFound);
            }
            if viewer.is_some_and(|viewer| viewer != account.id)
                && roosty_db::local_accounts_are_blocked(
                    &txn,
                    viewer.unwrap_or(account.id),
                    account.id,
                )
                .await
                .map_err(|_| UiPublicPageError::Internal)?
            {
                return Err(UiPublicPageError::NotFound);
            }
            let account_dto = ui_public_account(&state, &txn, &account).await?;
            let focus = ui_context_status(&state, &txn, focus, viewer, true).await?;
            let mut ancestor_dtos = Vec::with_capacity(ancestors.len());
            for item in ancestors {
                ancestor_dtos.push(ui_context_status(&state, &txn, item, viewer, true).await?);
            }
            let mut descendant_dtos = Vec::with_capacity(descendants.len());
            for item in descendants {
                descendant_dtos.push(ui_context_status(&state, &txn, item, viewer, true).await?);
            }
            txn.commit()
                .await
                .map_err(|_| UiPublicPageError::Internal)?;
            let canonical_url = public_page_url(&state, &format!("/@{username}/{}", status_id.0));
            Ok(UiStatusThread {
                noindex: !matches!(
                    focus.visibility,
                    UiStatusVisibility::Public | UiStatusVisibility::Unlisted
                ),
                activitypub_url: public_page_url(
                    &state,
                    &format!("/users/{username}/statuses/{}", status_id.0),
                ),
                canonical_url,
                account: account_dto,
                ancestors: ancestor_dtos,
                status: focus,
                descendants: descendant_dtos,
            })
        })
    }

    fn admin_work_queue(
        &self,
        cookie_header: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<UiAdminWorkQueue, String>> + Send + 'static>> {
        let state = self.state.clone();
        let database = self.database.clone();
        Box::pin(async move {
            authenticated_admin_headers(&state, &database, cookie_header).await?;
            let txn = database
                .begin_snapshot()
                .await
                .map_err(|error| error.to_string())?;
            let (summary, jobs) = tokio::try_join!(
                roosty_db::admin_job_summary(&txn),
                roosty_db::admin_job_diagnostics(&txn, 40, None),
            )
            .map_err(|error| error.to_string())?;
            txn.commit().await.map_err(|error| error.to_string())?;
            Ok(UiAdminWorkQueue {
                summary: ui_admin_job_summary(summary),
                jobs: jobs.into_iter().map(ui_admin_job).collect(),
            })
        })
    }

    fn admin_accounts(
        &self,
        cookie_header: Option<String>,
        query: String,
        origin: UiAdminAccountOrigin,
    ) -> Pin<Box<dyn Future<Output = Result<UiAdminAccounts, String>> + Send + 'static>> {
        let state = self.state.clone();
        let database = self.database.clone();
        Box::pin(async move {
            let headers = authenticated_admin_headers(&state, &database, cookie_header).await?;
            let csrf_token = csrf_token_from_session(&state, &headers)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "administrator session required".to_owned())?;
            let txn = database
                .begin_snapshot()
                .await
                .map_err(|error| error.to_string())?;
            let accounts = roosty_db::list_admin_accounts(
                &txn,
                &query,
                Some(origin.as_str()),
                None,
                None,
                100,
                None,
            )
            .await
            .map_err(|error| error.to_string())?;
            txn.commit().await.map_err(|error| error.to_string())?;
            Ok(UiAdminAccounts {
                csrf_token,
                accounts: accounts.into_iter().map(ui_admin_account).collect(),
            })
        })
    }

    fn admin_audit_log(
        &self,
        cookie_header: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<UiAdminAuditLog, String>> + Send + 'static>> {
        let state = self.state.clone();
        let database = self.database.clone();
        Box::pin(async move {
            authenticated_admin_headers(&state, &database, cookie_header).await?;
            let txn = database
                .begin_snapshot()
                .await
                .map_err(|error| error.to_string())?;
            let audit_entries = roosty_db::list_admin_audit_entries(&txn, 20, None)
                .await
                .map_err(|error| error.to_string())?;
            txn.commit().await.map_err(|error| error.to_string())?;
            Ok(UiAdminAuditLog {
                audit_entries: audit_entries
                    .into_iter()
                    .map(ui_admin_audit_entry)
                    .collect(),
            })
        })
    }

    fn admin_domain_blocks(
        &self,
        cookie_header: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<UiAdminDomainBlocks, String>> + Send + 'static>> {
        let state = self.state.clone();
        let database = self.database.clone();
        Box::pin(async move {
            let headers = authenticated_admin_headers(&state, &database, cookie_header).await?;
            let csrf_token = csrf_token_from_session(&state, &headers)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "administrator session required".to_owned())?;
            let txn = database
                .begin_snapshot()
                .await
                .map_err(|error| error.to_string())?;
            let domain_blocks = roosty_db::list_federation_domain_blocks(&txn, 200, None)
                .await
                .map_err(|error| error.to_string())?
                .into_iter()
                .map(ui_admin_domain_block)
                .collect();
            txn.commit().await.map_err(|error| error.to_string())?;
            Ok(UiAdminDomainBlocks {
                csrf_token,
                domain_blocks,
            })
        })
    }

    fn admin_moderation(
        &self,
        cookie_header: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<UiAdminModeration, String>> + Send + 'static>> {
        let state = self.state.clone();
        let database = self.database.clone();
        Box::pin(async move {
            let headers = authenticated_admin_headers(&state, &database, cookie_header).await?;
            let csrf_token = csrf_token_from_session(&state, &headers)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "administrator session required".to_owned())?;
            let txn = database
                .begin_snapshot()
                .await
                .map_err(|error| error.to_string())?;
            let (rules, reports) = tokio::try_join!(
                roosty_db::list_instance_rules(&txn),
                roosty_db::list_moderation_reports(
                    &txn,
                    ReportListOptions {
                        resolved: None,
                        limit: 100,
                        ..Default::default()
                    },
                ),
            )
            .map_err(|error| error.to_string())?;
            txn.commit().await.map_err(|error| error.to_string())?;
            Ok(UiAdminModeration {
                csrf_token,
                rules: rules
                    .into_iter()
                    .map(|rule| UiInstanceRule {
                        id: rule.id,
                        text: rule.text,
                    })
                    .collect(),
                reports: reports
                    .into_iter()
                    .map(|report| UiModerationReport {
                        id: report.id,
                        category: report.category.to_string(),
                        comment: report.comment,
                        source: report_account_label(report.source),
                        target: report_account_label(report.target),
                        target_id: match report.target {
                            ReportAccount::Local(id) | ReportAccount::Remote(id) => id.0,
                        },
                        resolved: report.action_taken_at.is_some(),
                        assigned: report.assigned_account_id.is_some(),
                        status_ids: report
                            .statuses
                            .into_iter()
                            .map(|status| match status {
                                ReportStatus::Local(id) | ReportStatus::Remote(id) => id.0,
                            })
                            .collect(),
                    })
                    .collect(),
            })
        })
    }
}

fn parse_ui_cursor(value: Option<&str>) -> Result<TimelineCursor, UiPublicPageError> {
    let max_id = value
        .map(Uuid::parse_str)
        .transpose()
        .map_err(|_| UiPublicPageError::BadRequest)?
        .map(StatusId);
    Ok(TimelineCursor {
        max_id,
        ..Default::default()
    })
}

async fn ui_viewer(
    state: &AppState,
    db: &impl ConnectionTrait,
    cookie_header: Option<String>,
) -> Result<Option<AccountId>, UiPublicPageError> {
    let headers = cookie_headers(cookie_header).map_err(|_| UiPublicPageError::BadRequest)?;
    let viewer =
        account_id_from_session(state, &headers).map_err(|_| UiPublicPageError::Internal)?;
    match viewer {
        Some(account_id) => Ok(roosty_db::find_local_account_by_id(db, account_id)
            .await
            .map_err(|_| UiPublicPageError::Internal)?
            .filter(|account| account.suspended_at.is_none())
            .map(|account| account.id)),
        None => Ok(None),
    }
}

async fn active_local_profile(
    db: &impl ConnectionTrait,
    username: &str,
) -> Result<LocalAccount, UiPublicPageError> {
    roosty_db::find_local_account_by_username(db, username)
        .await
        .map_err(|_| UiPublicPageError::Internal)?
        .filter(|account| account.suspended_at.is_none() && account.data_purged_at.is_none())
        .ok_or(UiPublicPageError::NotFound)
}

async fn ui_public_account(
    state: &AppState,
    db: &impl ConnectionTrait,
    account: &LocalAccount,
) -> Result<UiPublicAccount, UiPublicPageError> {
    let followers_count = roosty_db::count_local_followers(db, account.id)
        .await
        .map_err(|_| UiPublicPageError::Internal)?
        + roosty_db::count_remote_followers(db, account.id)
            .await
            .map_err(|_| UiPublicPageError::Internal)?;
    let following_count = roosty_db::count_local_following(db, account.id)
        .await
        .map_err(|_| UiPublicPageError::Internal)?
        + roosty_db::count_remote_following(db, account.id)
            .await
            .map_err(|_| UiPublicPageError::Internal)?;
    let statuses_count = roosty_db::count_local_statuses_by_account(db, account.id)
        .await
        .map_err(|_| UiPublicPageError::Internal)?;
    let fields = account
        .profile_fields
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|field| {
            Some(UiProfileField {
                name: field.get("name")?.as_str()?.to_owned(),
                value: field.get("value")?.as_str()?.to_owned(),
            })
        })
        .collect();
    Ok(UiPublicAccount {
        id: account.id.0,
        username: account.username.clone(),
        display_name: if account.display_name.is_empty() {
            account.username.clone()
        } else {
            account.display_name.clone()
        },
        bio: account.note.clone(),
        avatar_url: account
            .avatar_file_path
            .as_deref()
            .map(|path| media_url(state, path)),
        header_url: account
            .header_file_path
            .as_deref()
            .map(|path| media_url(state, path)),
        fields,
        created_at: format_timestamp(account.created_at),
        followers_count,
        following_count,
        statuses_count,
        limited: account.limited_at.is_some(),
        discoverable: account.discoverable,
    })
}

async fn ui_profile_status_page(
    state: &AppState,
    db: &impl ConnectionTrait,
    account: &LocalAccount,
    viewer: Option<AccountId>,
    tab: &UiProfileTab,
    hashtag: Option<&str>,
    cursor: TimelineCursor,
) -> Result<UiStatusPage, UiPublicPageError> {
    let tagged = match tab {
        UiProfileTab::Tagged => Some(
            roosty_db::normalize_featured_tag_name(hashtag.unwrap_or_default())
                .ok_or(UiPublicPageError::NotFound)?,
        ),
        _ => None,
    };
    let page = roosty_db::local_statuses_by_account(
        db,
        account.id,
        viewer,
        20,
        cursor,
        AccountStatusTimelineOptions {
            exclude_replies: matches!(tab, UiProfileTab::Posts),
            only_media: matches!(tab, UiProfileTab::Media),
            tagged,
        },
    )
    .await
    .map_err(|_| UiPublicPageError::Internal)?;
    let next_cursor = page.has_more.then_some(page.last_cursor).flatten();
    let statuses = ui_local_statuses(state, db, page.items, account, viewer, false).await?;
    Ok(UiStatusPage {
        statuses,
        next_cursor,
    })
}

async fn ui_local_statuses(
    state: &AppState,
    db: &impl ConnectionTrait,
    statuses: Vec<LocalStatus>,
    account: &LocalAccount,
    viewer: Option<AccountId>,
    pinned: bool,
) -> Result<Vec<UiStatus>, UiPublicPageError> {
    let mut result = Vec::with_capacity(statuses.len());
    for status in statuses {
        if crate::statuses::status_visible_to_viewer_on(db, &status, viewer)
            .await
            .map_err(|_| UiPublicPageError::Internal)?
        {
            result.push(ui_local_status(state, db, status, account, viewer, pinned, true).await?);
        }
    }
    Ok(result)
}

fn ui_visibility(visibility: StatusVisibility) -> UiStatusVisibility {
    match visibility {
        StatusVisibility::Public => UiStatusVisibility::Public,
        StatusVisibility::Unlisted => UiStatusVisibility::Unlisted,
        StatusVisibility::Private => UiStatusVisibility::Private,
        StatusVisibility::Direct => UiStatusVisibility::Direct,
    }
}

fn ui_media_kind(content_type: Option<&str>) -> UiMediaKind {
    match content_type.unwrap_or_default().split('/').next() {
        Some("image") => UiMediaKind::Image,
        Some("video") => UiMediaKind::Video,
        Some("audio") => UiMediaKind::Audio,
        _ => UiMediaKind::Unknown,
    }
}

async fn ui_local_status(
    state: &AppState,
    db: &impl ConnectionTrait,
    status: LocalStatus,
    account: &LocalAccount,
    viewer: Option<AccountId>,
    pinned: bool,
    include_quote: bool,
) -> Result<UiStatus, UiPublicPageError> {
    let id = status.id;
    let path = format!("/@{}/{}", account.username, id.0);
    let activitypub_url = public_page_url(
        state,
        &format!("/users/{}/statuses/{}", account.username, id.0),
    );
    let media = roosty_db::local_media_attachments_for_status(db, id)
        .await
        .map_err(|_| UiPublicPageError::Internal)?
        .into_iter()
        .map(|media| UiMedia {
            kind: ui_media_kind(Some(&media.content_type)),
            url: crate::media::media_url(state, &media.file_path),
            preview_url: media
                .preview_file_path
                .as_deref()
                .map(|path| crate::media::media_url(state, path)),
            description: media.description,
        })
        .collect();
    let poll = ui_poll(db, PollStatus::Local(id)).await?;
    let card = ui_preview_card(state, db, StatusReference::Local(id)).await?;
    let quote = if include_quote {
        Box::pin(ui_status_quote(
            state,
            db,
            StatusReference::Local(id),
            viewer,
        ))
        .await?
    } else {
        None
    };
    Ok(UiStatus {
        id: id.0,
        author: UiStatusAuthor {
            display_name: if account.display_name.is_empty() {
                account.username.clone()
            } else {
                account.display_name.clone()
            },
            handle: format!("@{}", account.username),
            url: public_page_url(state, &format!("/@{}", account.username)),
            avatar_url: account
                .avatar_file_path
                .as_deref()
                .map(|path| crate::media::media_url(state, path)),
            local: true,
        },
        url: public_page_url(state, &path),
        activitypub_url,
        content_html: crate::statuses::status_content_html_with_mentions_and_tags(
            &TransactionContext::new(state, db),
            &status.content,
            &[],
            &[],
            &[],
        ),
        spoiler_text: status.spoiler_text,
        sensitive: status.sensitive,
        visibility: ui_visibility(status.visibility),
        created_at: format_timestamp(status.created_at),
        edited_at: (status.updated_at != status.created_at)
            .then(|| format_timestamp(status.updated_at)),
        media,
        poll,
        card,
        quote,
        replies_count: roosty_db::count_status_context_replies(db, StatusContextParent::Local(id))
            .await
            .map_err(|_| UiPublicPageError::Internal)?,
        reblogs_count: roosty_db::count_local_reblogs(db, id)
            .await
            .map_err(|_| UiPublicPageError::Internal)?,
        favourites_count: roosty_db::count_local_favourites(db, id)
            .await
            .map_err(|_| UiPublicPageError::Internal)?,
        pinned,
    })
}

async fn ui_remote_status(
    state: &AppState,
    db: &impl ConnectionTrait,
    status: RemoteStatus,
    viewer: Option<AccountId>,
    include_quote: bool,
) -> Result<UiStatus, UiPublicPageError> {
    let actor = roosty_db::find_remote_actor_by_id(db, status.remote_actor_id)
        .await
        .map_err(|_| UiPublicPageError::Internal)?
        .filter(|actor| {
            actor.deleted_at.is_none()
                && actor.suspended_at.is_none()
                && actor.data_purged_at.is_none()
        })
        .ok_or(UiPublicPageError::NotFound)?;
    let id = status.id;
    let media = roosty_db::remote_media_attachments_for_status(db, id)
        .await
        .map_err(|_| UiPublicPageError::Internal)?
        .into_iter()
        .map(|media| UiMedia {
            kind: ui_media_kind(media.content_type.as_deref()),
            url: media
                .file_path
                .as_deref()
                .map(|path| crate::media::media_url(state, path))
                .unwrap_or(media.remote_url),
            preview_url: media
                .preview_file_path
                .as_deref()
                .map(|path| crate::media::media_url(state, path)),
            description: media.description,
        })
        .collect();
    let sensitive = status
        .object
        .get("sensitive")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let spoiler_text = status
        .object
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let quote = if include_quote {
        Box::pin(ui_status_quote(
            state,
            db,
            StatusReference::Remote(id),
            viewer,
        ))
        .await?
    } else {
        None
    };
    Ok(UiStatus {
        id: id.0,
        author: UiStatusAuthor {
            display_name: if actor.display_name.is_empty() {
                actor.username.clone()
            } else {
                actor.display_name.clone()
            },
            handle: format!("@{}@{}", actor.username, actor.domain),
            url: actor.activitypub_id.clone(),
            avatar_url: None,
            local: false,
        },
        url: status.activitypub_id.clone(),
        activitypub_url: status.activitypub_id,
        content_html: crate::statuses::sanitize_remote_status_html(&status.content),
        spoiler_text,
        sensitive,
        visibility: ui_visibility(status.visibility),
        created_at: format_timestamp(status.published_at),
        edited_at: (status.updated_at != status.published_at)
            .then(|| format_timestamp(status.updated_at)),
        media,
        poll: ui_poll(db, PollStatus::Remote(id)).await?,
        card: ui_preview_card(state, db, StatusReference::Remote(id)).await?,
        quote,
        replies_count: roosty_db::count_status_context_replies(db, StatusContextParent::Remote(id))
            .await
            .map_err(|_| UiPublicPageError::Internal)?,
        reblogs_count: 0,
        favourites_count: 0,
        pinned: roosty_db::is_remote_status_pinned(db, id)
            .await
            .map_err(|_| UiPublicPageError::Internal)?,
    })
}

async fn ui_context_status(
    state: &AppState,
    db: &impl ConnectionTrait,
    item: StatusContextItem,
    viewer: Option<AccountId>,
    include_quote: bool,
) -> Result<UiStatus, UiPublicPageError> {
    match item {
        StatusContextItem::Local(status) => {
            let account = roosty_db::find_local_account_by_id(db, status.account_id)
                .await
                .map_err(|_| UiPublicPageError::Internal)?
                .ok_or(UiPublicPageError::NotFound)?;
            let pinned = roosty_db::is_local_status_pinned(db, status.id)
                .await
                .map_err(|_| UiPublicPageError::Internal)?;
            ui_local_status(state, db, status, &account, viewer, pinned, include_quote).await
        }
        StatusContextItem::Remote(status) => {
            ui_remote_status(state, db, status, viewer, include_quote).await
        }
    }
}

async fn ui_poll(
    db: &impl ConnectionTrait,
    status: PollStatus,
) -> Result<Option<UiPoll>, UiPublicPageError> {
    Ok(roosty_db::find_poll_for_status(db, status)
        .await
        .map_err(|_| UiPublicPageError::Internal)?
        .map(|poll| {
            let hide_totals = poll.hide_totals && !poll.expired(OffsetDateTime::now_utc());
            UiPoll {
                multiple: poll.multiple,
                expired: poll.expired(OffsetDateTime::now_utc()),
                voters_count: (!hide_totals).then_some(poll.voters_count).flatten(),
                options: poll
                    .options
                    .into_iter()
                    .map(|option| UiPollOption {
                        title: option.title,
                        votes_count: (!hide_totals).then_some(option.votes_count),
                    })
                    .collect(),
            }
        }))
}

async fn ui_preview_card(
    state: &AppState,
    db: &impl ConnectionTrait,
    status: StatusReference,
) -> Result<Option<UiPreviewCard>, UiPublicPageError> {
    Ok(roosty_db::preview_card_for_status(db, status)
        .await
        .map_err(|_| UiPublicPageError::Internal)?
        .map(|card| UiPreviewCard {
            url: card.url,
            title: card.title,
            description: card.description,
            provider_name: card.provider_name,
            image_url: card
                .image_file_path
                .as_deref()
                .map(|path| crate::media::media_url(state, path)),
        }))
}

async fn ui_status_quote(
    state: &AppState,
    db: &impl ConnectionTrait,
    status: StatusReference,
    viewer: Option<AccountId>,
) -> Result<Option<Box<UiStatus>>, UiPublicPageError> {
    let Some(quote) = roosty_db::quote_for_status(db, status)
        .await
        .map_err(|_| UiPublicPageError::Internal)?
    else {
        return Ok(None);
    };
    if quote.state != QuoteState::Accepted {
        return Ok(None);
    }
    let Some(target) = quote.quoted_status else {
        return Ok(None);
    };
    let item = match target {
        StatusReference::Local(id) => roosty_db::find_local_status_by_id(db, id)
            .await
            .map_err(|_| UiPublicPageError::Internal)?
            .map(StatusContextItem::Local),
        StatusReference::Remote(id) => roosty_db::find_remote_status_by_id(db, id)
            .await
            .map_err(|_| UiPublicPageError::Internal)?
            .map(StatusContextItem::Remote),
    };
    let Some(item) = item else {
        return Ok(None);
    };
    if !crate::statuses::status_context_item_visible(db, &item, viewer)
        .await
        .map_err(|_| UiPublicPageError::Internal)?
    {
        return Ok(None);
    }
    Ok(Some(Box::new(
        ui_context_status(state, db, item, viewer, false).await?,
    )))
}

fn public_page_url(state: &AppState, path: &str) -> String {
    state
        .config
        .public_base_url
        .join(path.trim_start_matches('/'))
        .map_or_else(
            |_| {
                format!(
                    "{}/{}",
                    state.config.public_base_url.as_str().trim_end_matches('/'),
                    path.trim_start_matches('/')
                )
            },
            |url| url.to_string(),
        )
}

fn report_account_label(account: ReportAccount) -> String {
    match account {
        ReportAccount::Local(id) => format!("local:{}", id.0),
        ReportAccount::Remote(id) => format!("remote:{}", id.0),
    }
}

async fn authenticated_admin_headers(
    state: &AppState,
    database: &DatabaseContext,
    cookie_header: Option<String>,
) -> Result<HeaderMap, String> {
    let headers = cookie_headers(cookie_header)?;
    let account_id = account_id_from_session(state, &headers)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "administrator session required".to_owned())?;
    let txn = database
        .begin_read()
        .await
        .map_err(|error| error.to_string())?;
    let account = roosty_db::find_local_account_by_id(&txn, account_id)
        .await
        .map_err(|error| error.to_string())?
        .filter(|account| account.is_admin)
        .ok_or_else(|| "administrator session required".to_owned())?;
    txn.commit().await.map_err(|error| error.to_string())?;
    let _ = account;
    Ok(headers)
}

fn ui_admin_job_summary(summary: AdminJobSummary) -> UiAdminJobSummary {
    UiAdminJobSummary {
        due: summary.due,
        in_progress: summary.in_progress,
        scheduled_retries: summary.scheduled_retries,
        permanently_failed: summary.permanently_failed,
        oldest_due_at: summary.oldest_due_at.map(format_timestamp),
    }
}

fn ui_admin_job(job: AdminJobDiagnostic) -> UiAdminJob {
    UiAdminJob {
        id: job.id.0,
        kind: job.kind.as_str().to_owned(),
        state: if job.permanently_failed_at.is_some() {
            "permanently_failed"
        } else if job.locked_at.is_some() {
            "in_progress"
        } else if job.attempts > 0 {
            "retry_scheduled"
        } else {
            "due"
        }
        .to_owned(),
        attempts: job.attempts,
        run_after: format_timestamp(job.run_after),
        last_error: job.last_error,
    }
}

fn ui_admin_account(account: AdminAccount) -> UiAdminAccount {
    UiAdminAccount {
        id: account.id.0,
        username: account.username,
        domain: account.domain,
        email: account.email,
        display_name: account.display_name,
        is_admin: account.is_admin,
        limited: account.limited,
        suspended: account.suspended,
    }
}

fn ui_admin_domain_block(block: FederationDomainBlock) -> UiAdminDomainBlock {
    UiAdminDomainBlock {
        id: block.id,
        domain: block.domain,
        severity: block.severity.to_string(),
        reject_media: block.reject_media,
        reject_reports: block.reject_reports,
        private_comment: block.private_comment.unwrap_or_default(),
        public_comment: block.public_comment.unwrap_or_default(),
        obfuscate: block.obfuscate,
    }
}

fn ui_admin_audit_entry(entry: AdminAuditEntry) -> UiAdminAuditEntry {
    UiAdminAuditEntry {
        id: entry.id,
        action: entry.action.to_string(),
        source: entry.source.to_string(),
        target_kind: entry.target_kind.to_string(),
        target_id: entry.target_id,
        created_at: format_timestamp(entry.created_at),
    }
}

fn cookie_headers(cookie_header: Option<String>) -> Result<HeaderMap, String> {
    let mut headers = HeaderMap::new();
    if let Some(cookie_header) = cookie_header {
        let value = HeaderValue::from_str(&cookie_header).map_err(|error| error.to_string())?;
        headers.insert(header::COOKIE, value);
    }
    Ok(headers)
}

fn format_timestamp(timestamp: OffsetDateTime) -> String {
    timestamp
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| timestamp.unix_timestamp().to_string())
}

#[derive(Deserialize)]
struct CreateAccountForm {
    csrf_token: String,
    username: String,
    email: String,
    #[serde(default)]
    admin: bool,
}

#[derive(Deserialize)]
struct LimitAccountForm {
    csrf_token: String,
    limited: bool,
}

#[derive(Deserialize)]
struct CsrfForm {
    csrf_token: String,
}

#[derive(Deserialize)]
struct DomainBlockForm {
    csrf_token: String,
    #[serde(default)]
    domain: String,
    severity: String,
    #[serde(default)]
    reject_media: bool,
    #[serde(default)]
    reject_reports: bool,
    #[serde(default)]
    private_comment: String,
    #[serde(default)]
    public_comment: String,
    #[serde(default)]
    obfuscate: bool,
    operation: Option<String>,
}

#[derive(Deserialize)]
struct InstanceRuleForm {
    csrf_token: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    operation: InstanceRuleOperation,
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum InstanceRuleOperation {
    #[default]
    Save,
    Delete,
    Up,
    Down,
}

#[derive(Deserialize)]
struct ReportActionForm {
    csrf_token: String,
    operation: ReportOperation,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReportOperation {
    Assign,
    Resolve,
    Reopen,
}

type WebAdminResult<T> = Result<T, WebAdminError>;

#[derive(Debug, Error)]
enum WebAdminError {
    #[error("administrator session required")]
    Unauthorized,
    #[error("{0}")]
    Forbidden(Cow<'static, str>),
    #[error("Record not found")]
    NotFound,
    #[error("{0}")]
    Unprocessable(Cow<'static, str>),
    #[error(transparent)]
    Internal(RoostyError),
}

impl From<RoostyError> for WebAdminError {
    fn from(error: RoostyError) -> Self {
        match error {
            RoostyError::InvalidInput(reason) => Self::Unprocessable(Cow::Owned(reason)),
            error => Self::Internal(error),
        }
    }
}

impl From<DbErr> for WebAdminError {
    fn from(error: DbErr) -> Self {
        Self::Internal(error.into())
    }
}

impl From<ParseError> for WebAdminError {
    fn from(_: ParseError) -> Self {
        Self::Unprocessable("invalid value".into())
    }
}

impl IntoResponse for WebAdminError {
    fn into_response(self) -> Response {
        match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED.into_response(),
            Self::Forbidden(reason) => (StatusCode::FORBIDDEN, reason).into_response(),
            Self::NotFound => StatusCode::NOT_FOUND.into_response(),
            Self::Unprocessable(reason) => {
                (StatusCode::UNPROCESSABLE_ENTITY, reason).into_response()
            }
            Self::Internal(error) => {
                tracing::error!(%error, "administrator form failed");
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        }
    }
}

async fn authenticated_admin_form(
    state: &AppState,
    database: &DatabaseContext,
    headers: &HeaderMap,
    csrf_token: &str,
) -> WebAdminResult<AccountId> {
    if !validate_csrf_token(state, headers, csrf_token)? {
        return Err(WebAdminError::Forbidden("invalid CSRF token".into()));
    }
    let account_id = account_id_from_session(state, headers)?.ok_or(WebAdminError::Unauthorized)?;
    let txn = database.begin_read().await?;
    let account = roosty_db::find_local_account_by_id(&txn, account_id)
        .await?
        .filter(|account| account.is_admin)
        .ok_or(WebAdminError::Forbidden(
            "administrator privileges are required".into(),
        ))?;
    txn.commit().await?;
    Ok(account.id)
}

async fn create_admin_account(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    headers: HeaderMap,
    Form(form): Form<CreateAccountForm>,
) -> WebAdminResult<Response> {
    let actor = authenticated_admin_form(&state, &database, &headers, &form.csrf_token).await?;
    let txn = database.begin_write().await?;
    let result = admin::create_local_account_in_transaction(
        &txn,
        Some(actor),
        AdminSource::Web,
        &form.username,
        &form.email,
        form.admin,
    )
    .await?;
    txn.commit().await?;
    Ok(temporary_password_page(
        &state,
        "Account created",
        &result.account.username,
        &result.temporary_password,
    ))
}

async fn limit_admin_account(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    headers: HeaderMap,
    Path(account_id): Path<Uuid>,
    Form(form): Form<LimitAccountForm>,
) -> WebAdminResult<Response> {
    let actor = authenticated_admin_form(&state, &database, &headers, &form.csrf_token).await?;
    let txn = database.begin_write().await?;
    let account = admin::set_account_limited_in_transaction(
        &txn,
        Some(actor),
        AdminSource::Web,
        AccountId(account_id),
        form.limited,
    )
    .await?;
    txn.commit().await?;
    Ok(Redirect::to(if account.domain.is_some() {
        "/admin/remote-accounts"
    } else {
        "/admin/accounts"
    })
    .into_response())
}

async fn suspend_admin_account(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    headers: HeaderMap,
    Path(account_id): Path<Uuid>,
    Form(form): Form<CsrfForm>,
) -> WebAdminResult<Response> {
    let actor = authenticated_admin_form(&state, &database, &headers, &form.csrf_token).await?;
    let account_id = AccountId(account_id);
    let txn = database.begin_write().await?;
    let suspended = !roosty_db::find_admin_account_by_id(&txn, account_id)
        .await?
        .ok_or(WebAdminError::NotFound)?
        .suspended;
    let account = admin::set_account_suspended_in_transaction(
        &state,
        &txn,
        actor,
        AdminSource::Web,
        account_id,
        suspended,
    )
    .await?;
    txn.commit().await?;
    Ok(Redirect::to(if account.domain.is_some() {
        "/admin/remote-accounts"
    } else {
        "/admin/accounts"
    })
    .into_response())
}

async fn create_admin_domain_block(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    headers: HeaderMap,
    Form(form): Form<DomainBlockForm>,
) -> WebAdminResult<Response> {
    let actor = authenticated_admin_form(&state, &database, &headers, &form.csrf_token).await?;
    let severity = form.severity.parse()?;
    let txn = database.begin_write().await?;
    let block = roosty_db::create_federation_domain_block(
        &txn,
        NewFederationDomainBlock {
            domain: form.domain,
            severity,
            reject_media: form.reject_media,
            reject_reports: form.reject_reports,
            private_comment: nonempty(form.private_comment),
            public_comment: nonempty(form.public_comment),
            obfuscate: form.obfuscate,
        },
    )
    .await?;
    audit_web_domain_block(&txn, actor, AdminAuditAction::DomainBlockCreate, &block).await?;
    enqueue_web_domain_reconciliation(&txn, &block).await?;
    txn.commit().await?;
    Ok(Redirect::to("/admin/federation").into_response())
}

async fn create_admin_instance_rule(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    headers: HeaderMap,
    Form(form): Form<InstanceRuleForm>,
) -> WebAdminResult<Response> {
    let actor = authenticated_admin_form(&state, &database, &headers, &form.csrf_token).await?;
    let txn = database.begin_write().await?;
    let rule = roosty_db::create_instance_rule(&txn, &form.text).await?;
    audit_web_rule(&txn, actor, AdminAuditAction::InstanceRuleCreate, &rule).await?;
    txn.commit().await?;
    Ok(Redirect::to("/admin/moderation").into_response())
}

async fn update_admin_instance_rule(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    headers: HeaderMap,
    Path(rule_id): Path<Uuid>,
    Form(form): Form<InstanceRuleForm>,
) -> WebAdminResult<Response> {
    let actor = authenticated_admin_form(&state, &database, &headers, &form.csrf_token).await?;
    let txn = database.begin_write().await?;
    if matches!(
        form.operation,
        InstanceRuleOperation::Up | InstanceRuleOperation::Down
    ) {
        let rules = roosty_db::list_instance_rules(&txn).await?;
        let mut ids = rules.iter().map(|rule| rule.id).collect::<Vec<_>>();
        let Some(index) = ids.iter().position(|id| *id == rule_id) else {
            return Err(WebAdminError::NotFound);
        };
        let destination = if matches!(form.operation, InstanceRuleOperation::Up) {
            index.saturating_sub(1)
        } else {
            (index + 1).min(ids.len().saturating_sub(1))
        };
        ids.swap(index, destination);
        roosty_db::reorder_instance_rules(&txn, &ids).await?;
        roosty_db::insert_admin_audit_entry(
            &txn,
            Some(actor),
            AdminAuditSource::Web,
            AdminAuditAction::InstanceRuleReorder,
            AdminAuditTargetKind::InstanceRule,
            &rule_id.to_string(),
            json!({"rule_ids": ids}),
        )
        .await?;
        txn.commit().await?;
        return Ok(Redirect::to("/admin/moderation").into_response());
    }
    let (rule, action) = if matches!(form.operation, InstanceRuleOperation::Delete) {
        (
            roosty_db::discard_instance_rule(&txn, rule_id)
                .await?
                .ok_or(WebAdminError::NotFound)?,
            AdminAuditAction::InstanceRuleDelete,
        )
    } else {
        (
            roosty_db::update_instance_rule(&txn, rule_id, &form.text)
                .await?
                .ok_or(WebAdminError::NotFound)?,
            AdminAuditAction::InstanceRuleUpdate,
        )
    };
    audit_web_rule(&txn, actor, action, &rule).await?;
    txn.commit().await?;
    Ok(Redirect::to("/admin/moderation").into_response())
}

async fn update_admin_report(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    headers: HeaderMap,
    Path(report_id): Path<Uuid>,
    Form(form): Form<ReportActionForm>,
) -> WebAdminResult<Response> {
    let actor = authenticated_admin_form(&state, &database, &headers, &form.csrf_token).await?;
    let txn = database.begin_write().await?;
    let report = match form.operation {
        ReportOperation::Assign => {
            roosty_db::assign_moderation_report(&txn, report_id, Some(actor)).await
        }
        ReportOperation::Resolve => {
            roosty_db::set_moderation_report_resolved(&txn, report_id, Some(actor)).await
        }
        ReportOperation::Reopen => {
            roosty_db::set_moderation_report_resolved(&txn, report_id, None).await
        }
    }?
    .ok_or(WebAdminError::NotFound)?;
    let _ = report;
    let action = match form.operation {
        ReportOperation::Assign => AdminAuditAction::ReportAssign,
        ReportOperation::Resolve => AdminAuditAction::ReportResolve,
        ReportOperation::Reopen => AdminAuditAction::ReportReopen,
    };
    roosty_db::insert_admin_audit_entry(
        &txn,
        Some(actor),
        AdminAuditSource::Web,
        action,
        AdminAuditTargetKind::Report,
        &report_id.to_string(),
        json!({}),
    )
    .await?;
    txn.commit().await?;
    Ok(Redirect::to("/admin/moderation").into_response())
}

async fn delete_admin_report_status(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    headers: HeaderMap,
    Path((report_id, status_id)): Path<(Uuid, Uuid)>,
    Form(form): Form<CsrfForm>,
) -> WebAdminResult<Response> {
    let actor = authenticated_admin_form(&state, &database, &headers, &form.csrf_token).await?;
    let txn = database.begin_write().await?;
    let report = roosty_db::find_moderation_report(&txn, report_id)
        .await?
        .ok_or(WebAdminError::NotFound)?;
    let reference = report
        .statuses
        .into_iter()
        .find(|reference| match reference {
            ReportStatus::Local(id) | ReportStatus::Remote(id) => id.0 == status_id,
        })
        .ok_or(WebAdminError::NotFound)?;
    let context = TransactionContext::new(&state, &txn);
    if !delete_reported_status(&context, reference).await? {
        return Err(WebAdminError::NotFound);
    }
    roosty_db::insert_admin_audit_entry(
        &txn,
        Some(actor),
        AdminAuditSource::Web,
        AdminAuditAction::ReportUpdate,
        AdminAuditTargetKind::Report,
        &report_id.to_string(),
        json!({"removed_status_id": status_id}),
    )
    .await?;
    txn.commit().await?;
    Ok(Redirect::to("/admin/moderation").into_response())
}

async fn audit_web_rule(
    txn: &DatabaseTransaction,
    actor: AccountId,
    action: AdminAuditAction,
    rule: &InstanceRule,
) -> RoostyResult<()> {
    roosty_db::insert_admin_audit_entry(
        txn,
        Some(actor),
        AdminAuditSource::Web,
        action,
        AdminAuditTargetKind::InstanceRule,
        &rule.id.to_string(),
        json!({"text": rule.text}),
    )
    .await?;
    Ok(())
}

async fn update_admin_domain_block(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    headers: HeaderMap,
    Path(domain_block_id): Path<Uuid>,
    Form(form): Form<DomainBlockForm>,
) -> WebAdminResult<Response> {
    let actor = authenticated_admin_form(&state, &database, &headers, &form.csrf_token).await?;
    let txn = database.begin_write().await?;
    if form.operation.as_deref() == Some("delete") {
        let block = roosty_db::delete_federation_domain_block(&txn, domain_block_id)
            .await?
            .ok_or(WebAdminError::NotFound)?;
        audit_web_domain_block(&txn, actor, AdminAuditAction::DomainBlockDelete, &block).await?;
    } else {
        let severity = form.severity.parse()?;
        let block = roosty_db::update_federation_domain_block(
            &txn,
            domain_block_id,
            FederationDomainBlockUpdate {
                severity: Some(severity),
                reject_media: Some(form.reject_media),
                reject_reports: Some(form.reject_reports),
                private_comment: Some(nonempty(form.private_comment)),
                public_comment: Some(nonempty(form.public_comment)),
                obfuscate: Some(form.obfuscate),
            },
        )
        .await?;
        audit_web_domain_block(&txn, actor, AdminAuditAction::DomainBlockUpdate, &block).await?;
        enqueue_web_domain_reconciliation(&txn, &block).await?;
    }
    txn.commit().await?;
    Ok(Redirect::to("/admin/federation").into_response())
}

fn nonempty(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}

async fn audit_web_domain_block(
    txn: &DatabaseTransaction,
    actor: AccountId,
    action: AdminAuditAction,
    block: &FederationDomainBlock,
) -> RoostyResult<()> {
    roosty_db::insert_admin_audit_entry(
        txn,
        Some(actor),
        AdminAuditSource::Web,
        action,
        AdminAuditTargetKind::FederationDomain,
        &block.id.to_string(),
        json!({"domain": block.domain, "severity": block.severity}),
    )
    .await?;
    Ok(())
}

async fn enqueue_web_domain_reconciliation(
    txn: &DatabaseTransaction,
    block: &FederationDomainBlock,
) -> RoostyResult<()> {
    roosty_db::enqueue_job_in_transaction(
        txn,
        NewJob {
            kind: JobKind::DomainModerationReconcile,
            payload: json!({"domain_block_id": block.id}),
            deduplication_key: Some(format!("domain-moderation:{}", block.id)),
            run_after: OffsetDateTime::now_utc(),
        },
    )
    .await?;
    Ok(())
}

async fn reset_admin_password(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    headers: HeaderMap,
    Path(account_id): Path<Uuid>,
    Form(form): Form<CsrfForm>,
) -> WebAdminResult<Response> {
    let actor = authenticated_admin_form(&state, &database, &headers, &form.csrf_token).await?;
    let txn = database.begin_write().await?;
    roosty_db::find_admin_account_by_id(&txn, AccountId(account_id))
        .await?
        .ok_or(WebAdminError::NotFound)?;
    let result = admin::reset_local_password_in_transaction(
        &txn,
        Some(actor),
        AdminSource::Web,
        AccountId(account_id),
    )
    .await?;
    txn.commit().await?;
    Ok(temporary_password_page(
        &state,
        "Password reset",
        &result.account.username,
        &result.temporary_password,
    ))
}

fn temporary_password_page(
    state: &AppState,
    title: &str,
    username: &str,
    temporary_password: &str,
) -> Response {
    let stylesheet_href = roosty_web_ui::stylesheet_href(&state.leptos_options);
    Html(format!(
        "<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><link rel=\"stylesheet\" href=\"{stylesheet_href}\"><title>{title}</title><main class=\"card border-base-300 bg-base-100 mx-auto my-12 w-full max-w-xl border shadow-xl\"><div class=\"card-body\"><h1 class=\"card-title text-3xl\">{title}</h1><p>Temporary password for <strong>{username}</strong>:</p><div class=\"mockup-code\"><pre class=\"px-4\"><code class=\"break-all select-all\">{temporary_password}</code></pre></div><div class=\"alert alert-warning\"><span>This password is shown only once. Transfer it securely.</span></div><div class=\"card-actions\"><a class=\"btn btn-primary\" href=\"/admin/accounts\">Return to accounts</a></div></div></main></html>"
    ))
    .into_response()
}

fn admin_form_error(error: roosty_core::RoostyError) -> Response {
    let status = if matches!(error, roosty_core::RoostyError::InvalidInput(_)) {
        StatusCode::UNPROCESSABLE_ENTITY
    } else {
        tracing::error!(%error, "administrator form failed");
        StatusCode::INTERNAL_SERVER_ERROR
    };
    (status, error.to_string()).into_response()
}

#[cfg(test)]
mod tests {
    use std::{future::Future, pin::Pin, sync::Arc};

    use axum::{
        Router,
        body::{Body, to_bytes},
        extract::FromRef,
        http::{Request, StatusCode, header},
    };
    use leptos::{config::LeptosOptions, prelude::provide_context};
    use leptos_axum::LeptosRoutes;
    use roosty_web_ui::{
        UiAccount, UiBackend, UiBootstrap, UiProfileHeader, UiProfileTab, UiProfileTimeline,
        UiPublicAccount, UiPublicPageError, UiServerContext, UiStatus, UiStatusAuthor,
        UiStatusPage, UiStatusThread, UiStatusVisibility, shell,
    };
    use serde_json::Value;
    use tower::ServiceExt;
    use tower_http::services::ServeDir;
    use uuid::Uuid;

    /// Given the UI routes, when Leptos enumerates them, then every direct entry point is
    /// registered with Axum rather than relying on a catch-all fallback.
    #[tokio::test]
    async fn generated_routes_include_welcome_and_about() {
        let paths = super::ui_routes()
            .into_iter()
            .map(|route| route.path().to_owned())
            .collect::<Vec<_>>();

        assert!(paths.iter().any(|path| path == "/"));
        assert!(paths.iter().any(|path| path == "/about"));
        assert!(paths.iter().any(|path| path == "/login"));
        assert!(paths.iter().any(|path| path == "/auth/edit"));
        assert!(paths.iter().any(|path| path == "/admin"));
        assert!(paths.iter().any(|path| path == "/admin/jobs"));
        assert!(paths.iter().any(|path| path == "/admin/accounts"));
        assert!(paths.iter().any(|path| path == "/admin/remote-accounts"));
        assert!(paths.iter().any(|path| path == "/admin/audit-log"));
        assert!(paths.iter().any(|path| path == "/@{username}"));
        assert!(paths.iter().any(|path| path == "/@{username}/with_replies"));
        assert!(paths.iter().any(|path| path == "/@{username}/media"));
        assert!(
            paths
                .iter()
                .any(|path| path == "/@{username}/tagged/{hashtag}")
        );
        assert!(paths.iter().any(|path| path == "/@{username}/{status_id}"));
        assert!(!paths.iter().any(|path| path.starts_with("/api/")));
        assert!(!paths.iter().any(|path| path.starts_with("/oauth/")));
        assert!(!paths.iter().any(|path| path.starts_with("/users/")));
    }

    #[test]
    fn login_returns_to_the_requested_administration_category() {
        assert_eq!(
            super::login_return_query("/admin/jobs"),
            "next=%2Fadmin%2Fjobs"
        );
        assert_eq!(
            super::login_return_query("/admin/accounts"),
            "next=%2Fadmin%2Faccounts"
        );
        assert_eq!(
            super::login_return_query("/admin/remote-accounts"),
            "next=%2Fadmin%2Fremote-accounts"
        );
        assert_eq!(
            super::login_return_query("/admin/audit-log"),
            "next=%2Fadmin%2Faudit-log"
        );
        assert_eq!(super::login_return_query("/admin"), "next=%2Fadmin");
        assert_eq!(
            super::login_return_query("/auth/edit"),
            "next=%2Fauth%2Fedit"
        );
    }

    /// Given a failed credential submission, when the redirected login page renders, then the new
    /// shell preserves the safe return path and displays an accessible error beside the form.
    #[tokio::test]
    async fn renders_login_form_with_redirect_state() {
        let response = test_router()
            .oneshot(
                Request::get("/login?next=%2Fabout&error=invalid_credentials")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains(">Sign in</h1>"));
        assert!(html.contains("action=\"/login\""));
        assert!(html.contains("name=\"next\" value=\"/about\""));
        assert!(html.contains("Invalid username or password."));
        assert!(html.contains("class=\"input w-full\""));
        assert!(html.contains("class=\"btn btn-primary\""));
        assert!(html.contains("class=\"alert alert-error\""));
        assert!(html.contains("role=\"alert\""));
    }

    /// Given a signed-in visitor, when the password form is requested, then all fields retain the
    /// existing server handler names and a typed redirect result is presented accessibly.
    #[tokio::test]
    async fn renders_authenticated_password_form_and_result() {
        let response = test_router()
            .oneshot(
                Request::get("/auth/edit?result=current_password_incorrect")
                    .header(header::COOKIE, "roosty_session=test-session")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains(">Change password</h1>"));
        assert!(html.contains("action=\"/auth\""));
        assert!(html.contains("name=\"user[current_password]\""));
        assert!(html.contains("name=\"user[password]\""));
        assert!(html.contains("name=\"user[password_confirmation]\""));
        assert!(html.contains("Current password is incorrect."));
        assert!(html.contains("role=\"alert\""));
    }

    /// Given an anonymous visitor, when either UI route is requested directly, then the initial
    /// HTML contains route-specific content, SEO metadata, hydration, and a safe login return path.
    #[tokio::test]
    async fn renders_deep_links_with_metadata_and_session_navigation() {
        let app = test_router();
        for (path, marker, title, login_next) in [
            ("/", "Welcome to", "Welcome · Test Roosty", "/"),
            (
                "/about",
                "decentralized social web",
                "About · Test Roosty",
                "/about",
            ),
        ] {
            let response = app
                .clone()
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::OK);
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let html = String::from_utf8(body.to_vec()).unwrap();
            assert!(html.contains("<html lang=\"en\">"));
            assert!(html.contains(marker), "missing page marker in {path}");
            if path == "/about" {
                assert!(html.contains(">About "));
                assert!(html.contains("Test Roosty</h1>"));
            } else {
                assert!(html.contains(">Test Roosty</h1>"));
            }
            assert!(html.contains("class=\"btn btn-ghost text-xl\">Test Roosty</a>"));
            assert!(html.contains("A test social server"));
            assert!(html.contains("href=\"https://github.com/ctron/roosty\">Roosty</a>"));
            assert!(html.contains("v1.2.3"));
            assert!(html.contains(&format!("<title>{title}</title>")));
            assert!(html.contains(&format!(
                "href=\"https://roosty.test{path}\" rel=\"canonical\""
            )));
            assert!(html.contains(&format!("href=\"/login?next={login_next}\"")));
            assert!(html.contains(&format!(
                "href=\"/login?next={login_next}\" rel=\"external\""
            )));
            let login_href = format!("href=\"/login?next={login_next}\"");
            let login_href_offset = html.find(&login_href).expect("missing login link");
            let login_link_start = html[..login_href_offset]
                .rfind("<a")
                .expect("login href was not on a link");
            let login_link_end = html[login_href_offset..]
                .find("</a>")
                .map(|offset| login_href_offset + offset)
                .expect("login link was not closed");
            let login_link = &html[login_link_start..login_link_end];
            assert!(login_link.contains("class=\"btn btn-ghost\""));
            assert!(html.contains("/pkg/roosty-web.") && html.contains(".js"));
            if path == "/" {
                assert!(html.contains(">About this instance</a>"));
            }
        }
    }

    /// Given the hydrated frontend bundle, when it is requested through the application router,
    /// then the asset is served successfully as JavaScript rather than an HTML fallback.
    #[tokio::test]
    async fn serves_hydration_bundle_as_javascript() {
        let html_response = test_router()
            .clone()
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let html = String::from_utf8(
            to_bytes(html_response.into_body(), usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        let bundle_start = html
            .find("/pkg/roosty-web.")
            .expect("SSR HTML did not reference a hashed JavaScript bundle");
        let bundle_end = html[bundle_start..]
            .find('"')
            .map(|offset| bundle_start + offset)
            .expect("JavaScript bundle reference was not quoted");
        let bundle_path = &html[bundle_start..bundle_end];

        let response = test_router()
            .oneshot(Request::get(bundle_path).body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("text/javascript")
        );
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let javascript = String::from_utf8(body.to_vec()).unwrap();
        assert!(javascript.len() > 100);
        assert!(!javascript.contains("<html"));
    }

    /// Given an instance without an operator description, when its welcome page is rendered, then
    /// visitors see neutral instance copy rather than project marketing or an empty lead.
    #[tokio::test]
    async fn renders_neutral_missing_description_fallback() {
        let response = test_router_with_description(None)
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("A place to connect on the social web."));
        assert!(html.contains(
            "<meta name=\"description\" content=\"A place to connect on the social web.\">"
        ));
        assert!(!html.contains("built in Rust"));
    }

    /// Given a session cookie, when the welcome page is rendered, then the server-side bootstrap
    /// passes the request cookie to the backend and renders authenticated navigation immediately.
    #[tokio::test]
    async fn renders_authenticated_session_navigation() {
        let response = test_router()
            .oneshot(
                Request::get("/")
                    .header(header::COOKIE, "roosty_session=test-session")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("alice"));
        assert!(html.contains("class=\"navbar mx-auto max-w-6xl"));
        assert!(html.contains("<details class=\"dropdown dropdown-end\">"));
        assert!(html.contains("class=\"avatar"));
        assert!(html.contains("<ul class=\"menu dropdown-content rounded-box"));
        assert!(html.contains("href=\"/auth/edit\" rel=\"external\""));
        assert!(html.contains("method=\"post\" action=\"/logout\""));
        assert!(!html.contains("/login?next="));
    }

    #[tokio::test]
    async fn renders_public_profile_and_status_metadata_with_private_cache_policy() {
        for (path, marker, canonical, og_type) in [
            (
                "/@alice",
                "Profile post",
                "https://roosty.test/@alice",
                "profile",
            ),
            (
                "/@alice/0198a31c-2c00-7000-8000-000000000001",
                "Focused status",
                "https://roosty.test/@alice/0198a31c-2c00-7000-8000-000000000001",
                "article",
            ),
        ] {
            let response = test_router()
                .oneshot(
                    Request::get(path)
                        .header(header::COOKIE, "roosty_session=test-session")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.headers().get(header::CACHE_CONTROL).unwrap(),
                "private, no-store"
            );
            assert_eq!(response.headers().get(header::VARY).unwrap(), "Cookie");
            let html = String::from_utf8(
                to_bytes(response.into_body(), usize::MAX)
                    .await
                    .unwrap()
                    .to_vec(),
            )
            .unwrap();
            assert!(html.contains(marker));
            assert!(
                html.contains(&format!("href=\"{canonical}\" rel=\"canonical\"")),
                "canonical metadata missing from {path}"
            );
            assert!(html.contains("property=\"og:type\""));
            assert!(html.contains(&format!("content=\"{og_type}\"")));
            assert!(html.contains("application/activity+json"));
            assert!(html.contains("fediverse:creator"));
            assert!(html.contains("h-card"));
            assert!(html.contains("h-entry"));
            if path == "/@alice" {
                let script_type = "type=\"application/ld+json\"";
                let type_position = html.find(script_type).expect("profile JSON-LD script type");
                let script_content = html[type_position..]
                    .split_once('>')
                    .and_then(|(_, rest)| rest.split_once("</script>"))
                    .map(|(script, _)| script)
                    .expect("profile JSON-LD script content");
                let structured_data: Value =
                    serde_json::from_str(script_content).expect("valid profile JSON-LD");
                assert_eq!(structured_data["@type"], "ProfilePage");
                assert_eq!(structured_data["mainEntity"]["@type"], "Person");
            }
        }
    }

    #[tokio::test]
    async fn invalid_profile_cursor_returns_bad_request_document() {
        let response = test_router()
            .oneshot(
                Request::get("/@alice?max_id=invalid")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[derive(Clone)]
    struct TestState {
        options: LeptosOptions,
    }

    impl FromRef<TestState> for LeptosOptions {
        fn from_ref(state: &TestState) -> Self {
            state.options.clone()
        }
    }

    #[derive(Clone)]
    struct TestBackend {
        instance_description: Option<String>,
    }

    impl UiBackend for TestBackend {
        fn bootstrap(
            &self,
            cookie_header: Option<String>,
        ) -> Pin<Box<dyn Future<Output = Result<UiBootstrap, String>> + Send + 'static>> {
            let instance_description = self.instance_description.clone();
            Box::pin(async move {
                let account = cookie_header
                    .filter(|value| value.contains("roosty_session=test-session"))
                    .map(|_| UiAccount {
                        id: Uuid::nil(),
                        username: "alice".to_owned(),
                        display_name: "Alice".to_owned(),
                        avatar_url: None,
                        is_admin: false,
                    });
                Ok(UiBootstrap {
                    instance_name: "Test Roosty".to_owned(),
                    instance_description,
                    public_base_url: "https://roosty.test".to_owned(),
                    build_identifier: "v1.2.3".to_owned(),
                    account,
                    csrf_token: None,
                })
            })
        }

        fn profile_header(
            &self,
            _cookie_header: Option<String>,
            username: String,
        ) -> Pin<
            Box<dyn Future<Output = Result<UiProfileHeader, UiPublicPageError>> + Send + 'static>,
        > {
            Box::pin(async move {
                if username != "alice" {
                    return Err(UiPublicPageError::NotFound);
                }
                Ok(UiProfileHeader {
                    account: public_account(),
                    featured_tags: Vec::new(),
                    profile_url: "https://roosty.test/@alice".to_owned(),
                    activitypub_url: "https://roosty.test/users/alice".to_owned(),
                })
            })
        }

        fn profile_timeline(
            &self,
            _cookie_header: Option<String>,
            username: String,
            tab: UiProfileTab,
            hashtag: Option<String>,
            max_id: Option<String>,
        ) -> Pin<
            Box<dyn Future<Output = Result<UiProfileTimeline, UiPublicPageError>> + Send + 'static>,
        > {
            Box::pin(async move {
                if max_id
                    .as_deref()
                    .is_some_and(|cursor| Uuid::parse_str(cursor).is_err())
                {
                    return Err(UiPublicPageError::BadRequest);
                }
                if username != "alice" {
                    return Err(UiPublicPageError::NotFound);
                }
                Ok(UiProfileTimeline {
                    tab,
                    hashtag,
                    pinned_statuses: Vec::new(),
                    timeline: UiStatusPage {
                        statuses: vec![public_status("Profile post")],
                        next_cursor: None,
                    },
                })
            })
        }

        fn status_thread(
            &self,
            _cookie_header: Option<String>,
            username: String,
            status_id: String,
        ) -> Pin<Box<dyn Future<Output = Result<UiStatusThread, UiPublicPageError>> + Send + 'static>>
        {
            Box::pin(async move {
                if username != "alice" || status_id != "0198a31c-2c00-7000-8000-000000000001" {
                    return Err(UiPublicPageError::NotFound);
                }
                Ok(UiStatusThread {
                    account: public_account(),
                    ancestors: Vec::new(),
                    status: public_status("Focused status"),
                    descendants: Vec::new(),
                    canonical_url: format!("https://roosty.test/@alice/{status_id}"),
                    activitypub_url: format!(
                        "https://roosty.test/users/alice/statuses/{status_id}"
                    ),
                    noindex: false,
                })
            })
        }
    }

    fn public_account() -> UiPublicAccount {
        UiPublicAccount {
            id: Uuid::nil(),
            username: "alice".to_owned(),
            display_name: "Alice".to_owned(),
            bio: "A test profile".to_owned(),
            avatar_url: None,
            header_url: None,
            fields: Vec::new(),
            created_at: "2026-07-29T00:00:00Z".to_owned(),
            followers_count: 1,
            following_count: 2,
            statuses_count: 3,
            limited: false,
            discoverable: true,
        }
    }

    fn public_status(content: &str) -> UiStatus {
        let id = Uuid::parse_str("0198a31c-2c00-7000-8000-000000000001").unwrap();
        UiStatus {
            id,
            author: UiStatusAuthor {
                display_name: "Alice".to_owned(),
                handle: "@alice".to_owned(),
                url: "https://roosty.test/@alice".to_owned(),
                avatar_url: None,
                local: true,
            },
            url: format!("https://roosty.test/@alice/{id}"),
            activitypub_url: format!("https://roosty.test/users/alice/statuses/{id}"),
            content_html: format!("<p>{content}</p>"),
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

    fn test_router() -> Router {
        test_router_with_description(Some("A test social server".to_owned()))
    }

    fn test_router_with_description(instance_description: Option<String>) -> Router {
        let options = LeptosOptions::builder()
            .output_name("roosty-web")
            .site_root(concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/site"))
            .site_pkg_dir("pkg")
            .hash_file(concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/release/hash.txt").into())
            .hash_files(true)
            .build();
        let state = TestState {
            options: options.clone(),
        };
        let context = UiServerContext(Arc::new(TestBackend {
            instance_description,
        }));

        Router::new()
            .leptos_routes_with_context(
                &state,
                super::ui_routes(),
                move || provide_context(context.clone()),
                move || shell(options.clone()),
            )
            .nest_service(
                "/pkg",
                ServeDir::new(
                    std::path::Path::new(&*state.options.site_root)
                        .join(&*state.options.site_pkg_dir),
                ),
            )
            .with_state(state)
    }
}
