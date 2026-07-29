//! Native Axum integration for the server-rendered and hydrated first-party UI.

use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, OnceLock},
};

use axum::{
    Form, Router,
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, Request, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::post,
};
use leptos::prelude::provide_context;
use leptos_axum::{AxumRouteListing, LeptosRoutes, generate_route_list};
use roosty_web_ui::{
    App, UiAccount, UiAdminAccount, UiAdminAccountOrigin, UiAdminAccounts, UiAdminAuditEntry,
    UiAdminAuditLog, UiAdminDomainBlock, UiAdminDomainBlocks, UiAdminJob, UiAdminJobSummary,
    UiAdminWorkQueue, UiBackend, UiBootstrap, UiServerContext, shell,
};
use sea_orm::TransactionTrait;
use serde::Deserialize;
use serde_json::json;
use time::OffsetDateTime;
use tower_http::services::ServeDir;
use uuid::Uuid;

use crate::{
    admin::{self, AdminSource},
    auth::{account_id_from_session, csrf_token_from_session, validate_csrf_token},
    http::AppState,
};

static UI_ROUTES: OnceLock<Vec<AxumRouteListing>> = OnceLock::new();

fn ui_routes() -> Vec<AxumRouteListing> {
    UI_ROUTES.get_or_init(|| generate_route_list(App)).clone()
}

/// Mount explicit UI routes, internal server functions, and generated browser assets.
pub fn router(state: &AppState) -> Router<AppState> {
    let routes = ui_routes();
    let options = state.leptos_options.clone();
    let context = UiServerContext(Arc::new(RoostyUiBackend {
        state: state.clone(),
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
    request: Request<Body>,
    next: Next,
) -> Response {
    let path = request.uri().path();
    if path != "/auth/edit" && !path.starts_with("/admin") {
        return next.run(request).await;
    }

    match account_id_from_session(&state, request.headers()) {
        Ok(Some(account_id)) => {
            if !path.starts_with("/admin") {
                return next.run(request).await;
            }
            match roosty_db::find_local_account_by_id(&state.db, account_id).await {
                Ok(Some(account)) if account.is_admin => next.run(request).await,
                Ok(Some(_)) => StatusCode::FORBIDDEN.into_response(),
                Ok(None) => redirect_login(&state, path),
                Err(error) => {
                    tracing::error!(%error, "failed to authorize administrator route");
                    StatusCode::INTERNAL_SERVER_ERROR.into_response()
                }
            }
        }
        Ok(None) => redirect_login(&state, path),
        Err(error) => {
            tracing::error!(%error, "failed to validate browser session");
            (StatusCode::INTERNAL_SERVER_ERROR, "internal server error\n").into_response()
        }
    }
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
        "/admin/audit-log" => "next=%2Fadmin%2Faudit-log",
        path if path.starts_with("/admin") => "next=%2Fadmin",
        _ => "next=%2Fauth%2Fedit",
    }
}

#[derive(Clone)]
struct RoostyUiBackend {
    state: AppState,
}

impl UiBackend for RoostyUiBackend {
    fn bootstrap(
        &self,
        cookie_header: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<UiBootstrap, String>> + Send + 'static>> {
        let state = self.state.clone();
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
                Some(account_id) => roosty_db::find_local_account_by_id(&state.db, account_id)
                    .await
                    .map_err(|error| error.to_string())?
                    .map(|account| UiAccount {
                        id: account.id.0,
                        username: account.username,
                        display_name: account.display_name,
                        avatar_url: account
                            .avatar_file_path
                            .as_deref()
                            .map(|path| crate::media::media_url(&state, path)),
                        is_admin: account.is_admin,
                    }),
                None => None,
            };
            let csrf_token =
                csrf_token_from_session(&state, &headers).map_err(|error| error.to_string())?;
            Ok(UiBootstrap {
                instance_name: state.config.instance_name.clone(),
                instance_description: state.config.instance_description.clone(),
                public_base_url: state.config.public_base_url.to_string(),
                build_identifier: crate::version::build_identifier(),
                account,
                csrf_token,
            })
        })
    }

    fn admin_work_queue(
        &self,
        cookie_header: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<UiAdminWorkQueue, String>> + Send + 'static>> {
        let state = self.state.clone();
        Box::pin(async move {
            authenticated_admin_headers(&state, cookie_header).await?;
            let (summary, jobs) = tokio::try_join!(
                roosty_db::admin_job_summary(&state.db),
                roosty_db::admin_job_diagnostics(&state.db, 40, None),
            )
            .map_err(|error| error.to_string())?;
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
        Box::pin(async move {
            let headers = authenticated_admin_headers(&state, cookie_header).await?;
            let csrf_token = csrf_token_from_session(&state, &headers)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "administrator session required".to_owned())?;
            let accounts = roosty_db::list_admin_accounts(
                &state.db,
                &query,
                Some(origin.as_str()),
                None,
                None,
                100,
                None,
            )
            .await
            .map_err(|error| error.to_string())?;
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
        Box::pin(async move {
            authenticated_admin_headers(&state, cookie_header).await?;
            let audit_entries = roosty_db::list_admin_audit_entries(&state.db, 20, None)
                .await
                .map_err(|error| error.to_string())?;
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
        Box::pin(async move {
            let headers = authenticated_admin_headers(&state, cookie_header).await?;
            let csrf_token = csrf_token_from_session(&state, &headers)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "administrator session required".to_owned())?;
            let domain_blocks = roosty_db::list_federation_domain_blocks(&state.db, 200, None)
                .await
                .map_err(|error| error.to_string())?
                .into_iter()
                .map(ui_admin_domain_block)
                .collect();
            Ok(UiAdminDomainBlocks {
                csrf_token,
                domain_blocks,
            })
        })
    }
}

async fn authenticated_admin_headers(
    state: &AppState,
    cookie_header: Option<String>,
) -> Result<HeaderMap, String> {
    let headers = cookie_headers(cookie_header)?;
    let account_id = account_id_from_session(state, &headers)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "administrator session required".to_owned())?;
    roosty_db::find_local_account_by_id(&state.db, account_id)
        .await
        .map_err(|error| error.to_string())?
        .filter(|account| account.is_admin)
        .ok_or_else(|| "administrator session required".to_owned())?;
    Ok(headers)
}

fn ui_admin_job_summary(summary: roosty_db::AdminJobSummary) -> UiAdminJobSummary {
    UiAdminJobSummary {
        due: summary.due,
        in_progress: summary.in_progress,
        scheduled_retries: summary.scheduled_retries,
        permanently_failed: summary.permanently_failed,
        oldest_due_at: summary.oldest_due_at.map(format_timestamp),
    }
}

fn ui_admin_job(job: roosty_db::AdminJobDiagnostic) -> UiAdminJob {
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

fn ui_admin_account(account: roosty_db::AdminAccount) -> UiAdminAccount {
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

fn ui_admin_domain_block(block: roosty_db::FederationDomainBlock) -> UiAdminDomainBlock {
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

fn ui_admin_audit_entry(entry: roosty_db::AdminAuditEntry) -> UiAdminAuditEntry {
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

async fn authenticated_admin_form(
    state: &AppState,
    headers: &HeaderMap,
    csrf_token: &str,
) -> Result<roosty_core::AccountId, Response> {
    if !validate_csrf_token(state, headers, csrf_token).map_err(|error| {
        tracing::error!(%error, "failed to validate administrator CSRF token");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    })? {
        return Err(StatusCode::FORBIDDEN.into_response());
    }
    let account_id = account_id_from_session(state, headers)
        .map_err(|error| {
            tracing::error!(%error, "failed to validate administrator session");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        })?
        .ok_or_else(|| StatusCode::UNAUTHORIZED.into_response())?;
    let account = roosty_db::find_local_account_by_id(&state.db, account_id)
        .await
        .map_err(|error| {
            tracing::error!(%error, "failed to load administrator account");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        })?
        .filter(|account| account.is_admin)
        .ok_or_else(|| StatusCode::FORBIDDEN.into_response())?;
    Ok(account.id)
}

async fn create_admin_account(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<CreateAccountForm>,
) -> Response {
    let actor = match authenticated_admin_form(&state, &headers, &form.csrf_token).await {
        Ok(actor) => actor,
        Err(response) => return response,
    };
    match admin::create_local_account(
        &state.db,
        Some(actor),
        AdminSource::Web,
        &form.username,
        &form.email,
        form.admin,
    )
    .await
    {
        Ok(result) => temporary_password_page(
            &state,
            "Account created",
            &result.account.username,
            &result.temporary_password,
        ),
        Err(error) => admin_form_error(error),
    }
}

async fn limit_admin_account(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(account_id): Path<Uuid>,
    Form(form): Form<LimitAccountForm>,
) -> Response {
    let actor = match authenticated_admin_form(&state, &headers, &form.csrf_token).await {
        Ok(actor) => actor,
        Err(response) => return response,
    };
    match admin::set_account_limited(
        &state.db,
        Some(actor),
        AdminSource::Web,
        roosty_core::AccountId(account_id),
        form.limited,
    )
    .await
    {
        Ok(account) => Redirect::to(if account.domain.is_some() {
            "/admin/remote-accounts"
        } else {
            "/admin/accounts"
        })
        .into_response(),
        Err(error) => admin_form_error(error),
    }
}

async fn suspend_admin_account(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(account_id): Path<Uuid>,
    Form(form): Form<CsrfForm>,
) -> Response {
    let actor = match authenticated_admin_form(&state, &headers, &form.csrf_token).await {
        Ok(actor) => actor,
        Err(response) => return response,
    };
    let account_id = roosty_core::AccountId(account_id);
    let suspended = match roosty_db::find_admin_account_by_id(&state.db, account_id).await {
        Ok(Some(account)) => !account.suspended,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(error) => return admin_form_error(error),
    };
    match admin::set_account_suspended(&state, actor, AdminSource::Web, account_id, suspended).await
    {
        Ok(account) => Redirect::to(if account.domain.is_some() {
            "/admin/remote-accounts"
        } else {
            "/admin/accounts"
        })
        .into_response(),
        Err(error) => admin_form_error(error),
    }
}

async fn create_admin_domain_block(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<DomainBlockForm>,
) -> Response {
    let actor = match authenticated_admin_form(&state, &headers, &form.csrf_token).await {
        Ok(actor) => actor,
        Err(response) => return response,
    };
    let severity = match form.severity.parse() {
        Ok(severity) => severity,
        Err(_) => return (StatusCode::UNPROCESSABLE_ENTITY, "invalid severity").into_response(),
    };
    let txn = match state.db.begin().await {
        Ok(txn) => txn,
        Err(error) => return admin_form_error(error.into()),
    };
    let block = match roosty_db::create_federation_domain_block(
        &txn,
        roosty_db::NewFederationDomainBlock {
            domain: form.domain,
            severity,
            reject_media: form.reject_media,
            reject_reports: form.reject_reports,
            private_comment: nonempty(form.private_comment),
            public_comment: nonempty(form.public_comment),
            obfuscate: form.obfuscate,
        },
    )
    .await
    {
        Ok(block) => block,
        Err(error) => return admin_form_error(error),
    };
    if let Err(error) = audit_web_domain_block(
        &txn,
        actor,
        roosty_db::AdminAuditAction::DomainBlockCreate,
        &block,
    )
    .await
    {
        return admin_form_error(error);
    }
    if let Err(error) = enqueue_web_domain_reconciliation(&txn, &block).await {
        return admin_form_error(error);
    }
    match txn.commit().await {
        Ok(()) => Redirect::to("/admin/federation").into_response(),
        Err(error) => admin_form_error(error.into()),
    }
}

async fn update_admin_domain_block(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(domain_block_id): Path<Uuid>,
    Form(form): Form<DomainBlockForm>,
) -> Response {
    let actor = match authenticated_admin_form(&state, &headers, &form.csrf_token).await {
        Ok(actor) => actor,
        Err(response) => return response,
    };
    let txn = match state.db.begin().await {
        Ok(txn) => txn,
        Err(error) => return admin_form_error(error.into()),
    };
    if form.operation.as_deref() == Some("delete") {
        let block = match roosty_db::delete_federation_domain_block(&txn, domain_block_id).await {
            Ok(Some(block)) => block,
            Ok(None) => return StatusCode::NOT_FOUND.into_response(),
            Err(error) => return admin_form_error(error),
        };
        if let Err(error) = audit_web_domain_block(
            &txn,
            actor,
            roosty_db::AdminAuditAction::DomainBlockDelete,
            &block,
        )
        .await
        {
            return admin_form_error(error);
        }
    } else {
        let severity = match form.severity.parse() {
            Ok(severity) => severity,
            Err(_) => {
                return (StatusCode::UNPROCESSABLE_ENTITY, "invalid severity").into_response();
            }
        };
        let block = match roosty_db::update_federation_domain_block(
            &txn,
            domain_block_id,
            roosty_db::FederationDomainBlockUpdate {
                severity: Some(severity),
                reject_media: Some(form.reject_media),
                reject_reports: Some(form.reject_reports),
                private_comment: Some(nonempty(form.private_comment)),
                public_comment: Some(nonempty(form.public_comment)),
                obfuscate: Some(form.obfuscate),
            },
        )
        .await
        {
            Ok(block) => block,
            Err(error) => return admin_form_error(error),
        };
        if let Err(error) = audit_web_domain_block(
            &txn,
            actor,
            roosty_db::AdminAuditAction::DomainBlockUpdate,
            &block,
        )
        .await
        {
            return admin_form_error(error);
        }
        if let Err(error) = enqueue_web_domain_reconciliation(&txn, &block).await {
            return admin_form_error(error);
        }
    }
    match txn.commit().await {
        Ok(()) => Redirect::to("/admin/federation").into_response(),
        Err(error) => admin_form_error(error.into()),
    }
}

fn nonempty(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}

async fn audit_web_domain_block(
    txn: &sea_orm::DatabaseTransaction,
    actor: roosty_core::AccountId,
    action: roosty_db::AdminAuditAction,
    block: &roosty_db::FederationDomainBlock,
) -> roosty_core::Result<()> {
    roosty_db::insert_admin_audit_entry(
        txn,
        Some(actor),
        roosty_db::AdminAuditSource::Web,
        action,
        roosty_db::AdminAuditTargetKind::FederationDomain,
        &block.id.to_string(),
        json!({"domain": block.domain, "severity": block.severity}),
    )
    .await?;
    Ok(())
}

async fn enqueue_web_domain_reconciliation(
    txn: &sea_orm::DatabaseTransaction,
    block: &roosty_db::FederationDomainBlock,
) -> roosty_core::Result<()> {
    roosty_db::enqueue_job_in_transaction(
        txn,
        roosty_db::NewJob {
            kind: roosty_db::JobKind::DomainModerationReconcile,
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
    headers: HeaderMap,
    Path(account_id): Path<Uuid>,
    Form(form): Form<CsrfForm>,
) -> Response {
    let actor = match authenticated_admin_form(&state, &headers, &form.csrf_token).await {
        Ok(actor) => actor,
        Err(response) => return response,
    };
    match admin::reset_local_password(
        &state.db,
        Some(actor),
        AdminSource::Web,
        roosty_core::AccountId(account_id),
    )
    .await
    {
        Ok(result) => temporary_password_page(
            &state,
            "Password reset",
            &result.account.username,
            &result.temporary_password,
        ),
        Err(error) => admin_form_error(error),
    }
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
    use roosty_web_ui::{UiAccount, UiBackend, UiBootstrap, UiServerContext, shell};
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
