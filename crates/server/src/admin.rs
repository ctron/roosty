//! Administrator authorization, APIs, and transactional account operations.

use std::{borrow::Cow, path::Path as FsPath};

use axum::{
    Extension, Form, Json, Router,
    extract::{Path, Query, State},
    http::{HeaderValue, StatusCode, header, header::InvalidHeaderValue},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use roosty_core::{AccountId, RoostyError};
use roosty_db::{
    AdminAccount, AdminAuditAction, AdminAuditEntry, AdminAuditSource, AdminAuditTargetKind,
    AdminJobDiagnostic, AdminJobSummary, DbConnection, DomainBlockSeverity, FederationDomainBlock,
    FederationDomainBlockUpdate, JobKind, NewFederationDomainBlock, NewJob, ReportAccount,
};
use sea_orm::{DatabaseTransaction, DbErr, TransactionTrait};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::fs;
use uuid::Uuid;

use crate::{
    account_validation,
    auth::AuthenticatedAccessToken,
    federation::enqueue_actor_delete_in_transaction,
    http::{AppState, DatabaseContext},
    password,
};

type AdminResult<T> = Result<T, AdminApiError>;

#[derive(Debug, Error)]
enum AdminApiError {
    #[error(transparent)]
    Authorization(#[from] AdminAuthorizationError),
    #[error("{0}")]
    NotFound(Cow<'static, str>),
    #[error("{0}")]
    Forbidden(Cow<'static, str>),
    #[error("{0}")]
    Unprocessable(Cow<'static, str>),
    #[error(transparent)]
    Internal(RoostyError),
}

impl From<RoostyError> for AdminApiError {
    fn from(error: RoostyError) -> Self {
        match error {
            RoostyError::InvalidInput(reason) => Self::Unprocessable(Cow::Owned(reason)),
            error => Self::Internal(error),
        }
    }
}

impl From<DbErr> for AdminApiError {
    fn from(error: DbErr) -> Self {
        Self::Internal(error.into())
    }
}

impl From<InvalidHeaderValue> for AdminApiError {
    fn from(error: InvalidHeaderValue) -> Self {
        Self::Internal(RoostyError::Configuration(error.to_string()))
    }
}

impl IntoResponse for AdminApiError {
    fn into_response(self) -> Response {
        match self {
            Self::Authorization(error) => error.into_response(),
            Self::NotFound(reason) => api_error(StatusCode::NOT_FOUND, &reason),
            Self::Forbidden(reason) => api_error(StatusCode::FORBIDDEN, &reason),
            Self::Unprocessable(reason) => api_error(StatusCode::UNPROCESSABLE_ENTITY, &reason),
            Self::Internal(error) => server_error(error),
        }
    }
}

/// Mount Mastodon-compatible and Roosty-specific administrator routes.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v2/admin/accounts", get(accounts))
        .route("/api/v1/admin/accounts/{account_id}", get(account))
        .route(
            "/api/v1/admin/accounts/{account_id}/action",
            post(account_action),
        )
        .route(
            "/api/v1/admin/accounts/{account_id}/unsuspend",
            post(unsuspend_account),
        )
        .route(
            "/api/v1/admin/accounts/{account_id}",
            delete(delete_suspended_account),
        )
        .route(
            "/api/v1/admin/domain_blocks",
            get(domain_blocks).post(create_domain_block),
        )
        .route(
            "/api/v1/admin/domain_blocks/{domain_block_id}",
            get(domain_block)
                .put(update_domain_block)
                .delete(delete_domain_block),
        )
        .route(
            "/api/roosty/v1/admin/operations/summary",
            get(operations_summary),
        )
        .route("/api/roosty/v1/admin/jobs", get(jobs))
        .route("/api/roosty/v1/admin/audit-log", get(audit_log))
        .route("/api/roosty/v1/admin/accounts", post(create_account))
        .route(
            "/api/roosty/v1/admin/accounts/{account_id}/reset-password",
            post(reset_password),
        )
}

#[derive(Clone, Copy)]
pub(crate) enum AdminSource {
    Web,
    Api,
    Cli,
}

impl AdminSource {
    fn audit_source(self) -> AdminAuditSource {
        match self {
            Self::Web => AdminAuditSource::Web,
            Self::Api => AdminAuditSource::Api,
            Self::Cli => AdminAuditSource::Cli,
        }
    }
}

pub(crate) struct TemporaryCredential {
    pub account: AdminAccount,
    pub temporary_password: String,
}

pub(crate) async fn create_local_account(
    db: &DbConnection,
    actor: Option<AccountId>,
    source: AdminSource,
    username: &str,
    email: &str,
    is_admin: bool,
) -> Result<TemporaryCredential, RoostyError> {
    let txn = db.begin().await?;
    let credential =
        create_local_account_in_transaction(&txn, actor, source, username, email, is_admin).await?;
    txn.commit().await?;
    Ok(credential)
}

pub(crate) async fn create_local_account_in_transaction(
    txn: &DatabaseTransaction,
    actor: Option<AccountId>,
    source: AdminSource,
    username: &str,
    email: &str,
    is_admin: bool,
) -> Result<TemporaryCredential, RoostyError> {
    validate_username(username)?;
    validate_email(email)?;
    let temporary_password = password::generate_temporary_password();
    let password_hash = password::hash_password(&temporary_password)?;
    let id = if is_admin {
        roosty_db::create_admin_account_in_transaction(txn, username, email, &password_hash).await?
    } else {
        roosty_db::create_local_account_in_transaction(txn, username, email, &password_hash).await?
    };
    roosty_db::insert_admin_audit_entry(
        txn,
        actor,
        source.audit_source(),
        AdminAuditAction::AccountCreate,
        AdminAuditTargetKind::LocalAccount,
        &id.to_string(),
        json!({ "username": username, "is_admin": is_admin }),
    )
    .await?;
    let account = roosty_db::find_admin_account_by_id(txn, AccountId(id))
        .await?
        .ok_or_else(|| RoostyError::InvalidInput("created account was not found".to_owned()))?;
    Ok(TemporaryCredential {
        account,
        temporary_password,
    })
}

pub(crate) async fn reset_local_password(
    db: &DbConnection,
    actor: Option<AccountId>,
    source: AdminSource,
    account_id: AccountId,
) -> Result<TemporaryCredential, RoostyError> {
    let txn = db.begin().await?;
    let credential = reset_local_password_in_transaction(&txn, actor, source, account_id).await?;
    txn.commit().await?;
    Ok(credential)
}

pub(crate) async fn reset_local_password_in_transaction(
    txn: &DatabaseTransaction,
    actor: Option<AccountId>,
    source: AdminSource,
    account_id: AccountId,
) -> Result<TemporaryCredential, RoostyError> {
    let temporary_password = password::generate_temporary_password();
    let password_hash = password::hash_password(&temporary_password)?;
    let account =
        roosty_db::update_local_account_password_hash_by_id(txn, account_id, &password_hash)
            .await?;
    roosty_db::insert_admin_audit_entry(
        txn,
        actor,
        source.audit_source(),
        AdminAuditAction::AccountResetPassword,
        AdminAuditTargetKind::LocalAccount,
        &account.id.0.to_string(),
        json!({ "username": account.username }),
    )
    .await?;
    let account = roosty_db::find_admin_account_by_id(txn, account.id)
        .await?
        .ok_or_else(|| RoostyError::InvalidInput("reset account was not found".to_owned()))?;
    Ok(TemporaryCredential {
        account,
        temporary_password,
    })
}

pub(crate) async fn set_account_limited(
    db: &DbConnection,
    actor: Option<AccountId>,
    source: AdminSource,
    account_id: AccountId,
    limited: bool,
) -> Result<AdminAccount, RoostyError> {
    let txn = db.begin().await?;
    let account =
        set_account_limited_in_transaction(&txn, actor, source, account_id, limited).await?;
    txn.commit().await?;
    Ok(account)
}

pub(crate) async fn set_account_limited_in_transaction(
    txn: &DatabaseTransaction,
    actor: Option<AccountId>,
    source: AdminSource,
    account_id: AccountId,
    limited: bool,
) -> Result<AdminAccount, RoostyError> {
    let existing = roosty_db::find_admin_account_by_id(txn, account_id)
        .await?
        .ok_or_else(|| RoostyError::InvalidInput("account does not exist".to_owned()))?;
    if existing.domain.is_some() {
        roosty_db::set_remote_actor_limited_by_id(txn, account_id, limited).await?;
    } else {
        roosty_db::set_local_account_limited_by_id(txn, account_id, limited).await?;
    }
    roosty_db::insert_admin_audit_entry(
        txn,
        actor,
        source.audit_source(),
        if limited {
            AdminAuditAction::AccountLimit
        } else {
            AdminAuditAction::AccountUnlimit
        },
        if existing.domain.is_some() {
            AdminAuditTargetKind::RemoteActor
        } else {
            AdminAuditTargetKind::LocalAccount
        },
        &account_id.0.to_string(),
        json!({ "username": existing.username, "domain": existing.domain }),
    )
    .await?;
    let account = roosty_db::find_admin_account_by_id(txn, account_id)
        .await?
        .ok_or_else(|| RoostyError::InvalidInput("updated account was not found".to_owned()))?;
    Ok(account)
}

pub(crate) async fn set_account_suspended(
    state: &AppState,
    database: &DatabaseContext,
    actor: AccountId,
    source: AdminSource,
    account_id: AccountId,
    suspended: bool,
) -> Result<AdminAccount, RoostyError> {
    let txn = database.begin_write().await?;
    let account =
        set_account_suspended_in_transaction(state, &txn, actor, source, account_id, suspended)
            .await?;
    txn.commit().await?;
    Ok(account)
}

pub(crate) async fn set_account_suspended_in_transaction(
    state: &AppState,
    txn: &DatabaseTransaction,
    actor: AccountId,
    source: AdminSource,
    account_id: AccountId,
    suspended: bool,
) -> Result<AdminAccount, RoostyError> {
    if suspended && actor == account_id {
        return Err(RoostyError::InvalidInput(
            "administrators cannot suspend themselves".to_owned(),
        ));
    }
    let existing = roosty_db::find_admin_account_by_id(txn, account_id)
        .await?
        .ok_or_else(|| RoostyError::InvalidInput("account does not exist".to_owned()))?;
    if suspended && existing.is_admin && roosty_db::count_active_admin_accounts(txn).await? <= 1 {
        return Err(RoostyError::InvalidInput(
            "the final active administrator cannot be suspended".to_owned(),
        ));
    }
    if !suspended && existing.domain.is_none() && existing.data_purged_at.is_some() {
        return Err(RoostyError::InvalidInput(
            "an account cannot be unsuspended after its data was purged".to_owned(),
        ));
    }
    if suspended && !existing.suspended && existing.domain.is_none() {
        let local = roosty_db::find_local_account_by_id(txn, account_id)
            .await?
            .ok_or_else(|| RoostyError::InvalidInput("account does not exist".to_owned()))?;
        enqueue_actor_delete_in_transaction(state, txn, &local).await?;
    }
    let account = roosty_db::set_account_suspended_by_id(txn, account_id, suspended)
        .await?
        .ok_or_else(|| RoostyError::InvalidInput("account does not exist".to_owned()))?;
    roosty_db::insert_admin_audit_entry(
        txn,
        Some(actor),
        source.audit_source(),
        if suspended {
            AdminAuditAction::AccountSuspend
        } else {
            AdminAuditAction::AccountUnsuspend
        },
        if account.domain.is_some() {
            AdminAuditTargetKind::RemoteActor
        } else {
            AdminAuditTargetKind::LocalAccount
        },
        &account_id.0.to_string(),
        json!({"username": account.username, "domain": account.domain}),
    )
    .await?;
    Ok(account)
}

pub(crate) fn require_admin(
    token: &AuthenticatedAccessToken,
    permission: AdminPermission,
) -> Result<AccountId, AdminAuthorizationError> {
    if !token.grant.account.is_admin {
        return Err(AdminAuthorizationError::NotAdministrator);
    }
    let allowed = token
        .grant
        .scopes
        .split_ascii_whitespace()
        .any(|scope| permission.allows_scope(scope));
    if !allowed {
        return Err(AdminAuthorizationError::InsufficientScope);
    }
    Ok(token.grant.account.id)
}

#[derive(Debug, Error)]
pub(crate) enum AdminAuthorizationError {
    #[error("This action is not allowed")]
    NotAdministrator,
    #[error("This action requires an administrator OAuth scope")]
    InsufficientScope,
}

impl IntoResponse for AdminAuthorizationError {
    fn into_response(self) -> Response {
        api_error(StatusCode::FORBIDDEN, &self.to_string())
    }
}

/// Closed administrator capability checked after OAuth strings cross the wire boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AdminPermission {
    Read(AdminReadPermission),
    Write(AdminWritePermission),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AdminReadPermission {
    All,
    Accounts,
    DomainBlocks,
    Reports,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AdminWritePermission {
    Accounts,
    DomainBlocks,
    Reports,
}

impl AdminPermission {
    fn allows_scope(self, scope: &str) -> bool {
        match self {
            Self::Read(AdminReadPermission::All) => scope == "admin:read",
            Self::Read(AdminReadPermission::Accounts) => {
                matches!(scope, "admin:read" | "admin:read:accounts")
            }
            Self::Read(AdminReadPermission::DomainBlocks) => {
                matches!(scope, "admin:read" | "admin:read:domain_blocks")
            }
            Self::Read(AdminReadPermission::Reports) => {
                matches!(scope, "admin:read" | "admin:read:reports")
            }
            Self::Write(AdminWritePermission::Accounts) => {
                matches!(scope, "admin:write" | "admin:write:accounts")
            }
            Self::Write(AdminWritePermission::DomainBlocks) => {
                matches!(scope, "admin:write" | "admin:write:domain_blocks")
            }
            Self::Write(AdminWritePermission::Reports) => {
                matches!(scope, "admin:write" | "admin:write:reports")
            }
        }
    }
}

#[derive(Deserialize)]
struct DomainBlockQuery {
    limit: Option<u64>,
    max_id: Option<Uuid>,
}

#[derive(Deserialize)]
struct DomainBlockPath {
    domain_block_id: Uuid,
}

#[derive(Deserialize)]
struct CreateDomainBlock {
    domain: String,
    #[serde(default = "default_domain_block_severity")]
    severity: DomainBlockSeverity,
    #[serde(default)]
    reject_media: bool,
    #[serde(default)]
    reject_reports: bool,
    private_comment: Option<String>,
    public_comment: Option<String>,
    #[serde(default)]
    obfuscate: bool,
}

fn default_domain_block_severity() -> DomainBlockSeverity {
    DomainBlockSeverity::Silence
}

#[derive(Default, Deserialize)]
struct UpdateDomainBlock {
    severity: Option<DomainBlockSeverity>,
    reject_media: Option<bool>,
    reject_reports: Option<bool>,
    private_comment: Option<String>,
    public_comment: Option<String>,
    obfuscate: Option<bool>,
}

#[derive(Serialize)]
struct DomainBlockResponse {
    id: String,
    domain: String,
    digest: String,
    created_at: String,
    severity: DomainBlockSeverity,
    reject_media: bool,
    reject_reports: bool,
    private_comment: Option<String>,
    public_comment: Option<String>,
    obfuscate: bool,
}

impl From<FederationDomainBlock> for DomainBlockResponse {
    fn from(block: FederationDomainBlock) -> Self {
        let digest = format!("{:x}", Sha256::digest(block.domain.as_bytes()));
        Self {
            id: block.id.to_string(),
            domain: block.domain,
            digest,
            created_at: format_timestamp(block.created_at),
            severity: block.severity,
            reject_media: block.reject_media,
            reject_reports: block.reject_reports,
            private_comment: block.private_comment,
            public_comment: block.public_comment,
            obfuscate: block.obfuscate,
        }
    }
}

async fn domain_blocks(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    token: AuthenticatedAccessToken,
    Query(params): Query<DomainBlockQuery>,
) -> AdminResult<Response> {
    require_admin(
        &token,
        AdminPermission::Read(AdminReadPermission::DomainBlocks),
    )?;
    let limit = params.limit.unwrap_or(100).clamp(1, 200);
    let txn = database.begin_snapshot().await?;
    let mut blocks =
        roosty_db::list_federation_domain_blocks(&txn, limit.saturating_add(1), params.max_id)
            .await?;
    let has_more = blocks.len() > usize::try_from(limit).unwrap_or(usize::MAX);
    blocks.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    let next = has_more
        .then(|| blocks.last().map(|block| block.id))
        .flatten();
    let mut response = Json(
        blocks
            .into_iter()
            .map(DomainBlockResponse::from)
            .collect::<Vec<_>>(),
    )
    .into_response();
    if let Some(next) = next {
        let link = format!(
            "<{}/api/v1/admin/domain_blocks?limit={limit}&max_id={next}>; rel=\"next\"",
            state.config.public_base_url.as_str().trim_end_matches('/')
        );
        response
            .headers_mut()
            .insert(header::LINK, HeaderValue::from_str(&link)?);
    }
    txn.commit().await?;
    Ok(response)
}

async fn domain_block(
    Extension(database): Extension<DatabaseContext>,
    token: AuthenticatedAccessToken,
    Path(path): Path<DomainBlockPath>,
) -> AdminResult<Response> {
    require_admin(
        &token,
        AdminPermission::Read(AdminReadPermission::DomainBlocks),
    )?;
    let txn = database.begin_read().await?;
    let block = roosty_db::find_federation_domain_block(&txn, path.domain_block_id)
        .await?
        .ok_or(AdminApiError::NotFound("Record not found".into()))?;
    txn.commit().await?;
    Ok(Json(DomainBlockResponse::from(block)).into_response())
}

async fn create_domain_block(
    Extension(database): Extension<DatabaseContext>,
    token: AuthenticatedAccessToken,
    Form(input): Form<CreateDomainBlock>,
) -> AdminResult<Response> {
    let actor = require_admin(
        &token,
        AdminPermission::Write(AdminWritePermission::DomainBlocks),
    )?;
    let txn = database.begin_write().await?;
    let block = roosty_db::create_federation_domain_block(
        &txn,
        NewFederationDomainBlock {
            domain: input.domain,
            severity: input.severity,
            reject_media: input.reject_media,
            reject_reports: input.reject_reports,
            private_comment: input.private_comment,
            public_comment: input.public_comment,
            obfuscate: input.obfuscate,
        },
    )
    .await?;
    audit_domain_block(&txn, actor, "domain_block.create", &block).await?;
    enqueue_domain_reconciliation(&txn, &block).await?;
    txn.commit().await?;
    Ok(Json(DomainBlockResponse::from(block)).into_response())
}

async fn update_domain_block(
    Extension(database): Extension<DatabaseContext>,
    token: AuthenticatedAccessToken,
    Path(path): Path<DomainBlockPath>,
    Form(input): Form<UpdateDomainBlock>,
) -> AdminResult<Response> {
    let actor = require_admin(
        &token,
        AdminPermission::Write(AdminWritePermission::DomainBlocks),
    )?;
    let txn = database.begin_write().await?;
    roosty_db::find_federation_domain_block(&txn, path.domain_block_id)
        .await?
        .ok_or(AdminApiError::NotFound("Record not found".into()))?;
    let update = FederationDomainBlockUpdate {
        severity: input.severity,
        reject_media: input.reject_media,
        reject_reports: input.reject_reports,
        private_comment: input.private_comment.map(Some),
        public_comment: input.public_comment.map(Some),
        obfuscate: input.obfuscate,
    };
    let block =
        roosty_db::update_federation_domain_block(&txn, path.domain_block_id, update).await?;
    audit_domain_block(&txn, actor, "domain_block.update", &block).await?;
    enqueue_domain_reconciliation(&txn, &block).await?;
    txn.commit().await?;
    Ok(Json(DomainBlockResponse::from(block)).into_response())
}

async fn delete_domain_block(
    Extension(database): Extension<DatabaseContext>,
    token: AuthenticatedAccessToken,
    Path(path): Path<DomainBlockPath>,
) -> AdminResult<Response> {
    let actor = require_admin(
        &token,
        AdminPermission::Write(AdminWritePermission::DomainBlocks),
    )?;
    let txn = database.begin_write().await?;
    let block = roosty_db::delete_federation_domain_block(&txn, path.domain_block_id)
        .await?
        .ok_or(AdminApiError::NotFound("Record not found".into()))?;
    audit_domain_block(&txn, actor, "domain_block.delete", &block).await?;
    txn.commit().await?;
    Ok(Json(json!({})).into_response())
}

async fn audit_domain_block(
    txn: &DatabaseTransaction,
    actor: AccountId,
    action: &str,
    block: &FederationDomainBlock,
) -> Result<(), RoostyError> {
    roosty_db::insert_admin_audit_entry(
        txn,
        Some(actor),
        AdminAuditSource::Api,
        match action {
            "domain_block.create" => AdminAuditAction::DomainBlockCreate,
            "domain_block.update" => AdminAuditAction::DomainBlockUpdate,
            _ => AdminAuditAction::DomainBlockDelete,
        },
        AdminAuditTargetKind::FederationDomain,
        &block.id.to_string(),
        json!({"domain": block.domain, "severity": block.severity}),
    )
    .await?;
    Ok(())
}

async fn enqueue_domain_reconciliation(
    txn: &DatabaseTransaction,
    block: &FederationDomainBlock,
) -> Result<(), RoostyError> {
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

#[derive(Deserialize)]
struct AccountQuery {
    origin: Option<String>,
    status: Option<String>,
    username: Option<String>,
    display_name: Option<String>,
    by_domain: Option<String>,
    email: Option<String>,
    limit: Option<u64>,
    max_id: Option<Uuid>,
}

async fn accounts(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    token: AuthenticatedAccessToken,
    Query(params): Query<AccountQuery>,
) -> AdminResult<Response> {
    require_admin(&token, AdminPermission::Read(AdminReadPermission::Accounts))?;
    let (limited, suspended) = match params.status.as_deref() {
        Some("silenced") => (Some(true), None),
        Some("suspended") => (None, Some(true)),
        Some("active") => (Some(false), Some(false)),
        Some(_) => return Ok(Json(Vec::<AdminAccountResponse>::new()).into_response()),
        None => (None, None),
    };
    let query = [
        params.username,
        params.display_name,
        params.by_domain,
        params.email,
    ]
    .into_iter()
    .flatten()
    .find(|value| !value.trim().is_empty())
    .unwrap_or_default();
    let limit = params.limit.unwrap_or(40).clamp(1, 100);
    let txn = database.begin_snapshot().await?;
    let mut accounts = roosty_db::list_admin_accounts(
        &txn,
        &query,
        params.origin.as_deref(),
        limited,
        suspended,
        limit.saturating_add(1),
        params.max_id,
    )
    .await?;
    let page_len = usize::try_from(limit).unwrap_or(usize::MAX);
    let has_more = accounts.len() > page_len;
    accounts.truncate(page_len);
    let next = has_more
        .then(|| accounts.last().map(|account| account.id.0))
        .flatten();
    let body = accounts
        .into_iter()
        .map(AdminAccountResponse::from)
        .collect::<Vec<_>>();
    let mut response = Json(body).into_response();
    if let Some(next) = next {
        let link = format!(
            "<{}/api/v2/admin/accounts?limit={limit}&max_id={next}>; rel=\"next\"",
            state.config.public_base_url.as_str().trim_end_matches('/')
        );
        response
            .headers_mut()
            .insert(header::LINK, HeaderValue::from_str(&link)?);
    }
    txn.commit().await?;
    Ok(response)
}

async fn account(
    Extension(database): Extension<DatabaseContext>,
    token: AuthenticatedAccessToken,
    Path(account_id): Path<Uuid>,
) -> AdminResult<Response> {
    require_admin(&token, AdminPermission::Read(AdminReadPermission::Accounts))?;
    let txn = database.begin_read().await?;
    let account = roosty_db::find_admin_account_by_id(&txn, AccountId(account_id))
        .await?
        .ok_or(AdminApiError::NotFound("Record not found".into()))?;
    txn.commit().await?;
    Ok(Json(AdminAccountResponse::from(account)).into_response())
}

#[derive(Deserialize)]
struct AccountAction {
    #[serde(rename = "type")]
    action_type: String,
    report_id: Option<Uuid>,
}

async fn account_action(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    token: AuthenticatedAccessToken,
    Path(account_id): Path<Uuid>,
    Form(action): Form<AccountAction>,
) -> AdminResult<Response> {
    let actor = require_admin(
        &token,
        AdminPermission::Write(AdminWritePermission::Accounts),
    )?;
    if let Some(report_id) = action.report_id {
        return account_action_for_report(
            &state,
            &database,
            actor,
            AccountId(account_id),
            report_id,
            &action.action_type,
        )
        .await;
    }
    match action.action_type.as_str() {
        "silence" => {
            let txn = database.begin_write().await?;
            set_account_limited_in_transaction(
                &txn,
                Some(actor),
                AdminSource::Api,
                AccountId(account_id),
                true,
            )
            .await?;
            txn.commit().await?;
            Ok(StatusCode::OK.into_response())
        }
        "suspend" => {
            set_account_suspended(
                &state,
                &database,
                actor,
                AdminSource::Api,
                AccountId(account_id),
                true,
            )
            .await?;
            Ok(Json(json!({})).into_response())
        }
        "none" => Ok(Json(json!({})).into_response()),
        _ => Err(AdminApiError::Unprocessable(
            "Only none, silence, and suspend actions are supported".into(),
        )),
    }
}

async fn account_action_for_report(
    state: &AppState,
    database: &DatabaseContext,
    actor: AccountId,
    account_id: AccountId,
    report_id: Uuid,
    action_type: &str,
) -> AdminResult<Response> {
    let txn = database.begin_write().await?;
    let report = roosty_db::find_moderation_report(&txn, report_id)
        .await?
        .ok_or(AdminApiError::NotFound("Record not found".into()))?;
    let target_id = match report.target {
        ReportAccount::Local(id) | ReportAccount::Remote(id) => id,
    };
    if target_id != account_id {
        return Err(AdminApiError::Unprocessable(
            "Report target does not match account".into(),
        ));
    }
    match action_type {
        "silence" => {
            set_account_limited_in_transaction(
                &txn,
                Some(actor),
                AdminSource::Api,
                account_id,
                true,
            )
            .await?;
        }
        "suspend" => {
            set_account_suspended_in_transaction(
                state,
                &txn,
                actor,
                AdminSource::Api,
                account_id,
                true,
            )
            .await?;
        }
        "none" => {
            roosty_db::find_admin_account_by_id(&txn, account_id)
                .await?
                .ok_or(AdminApiError::NotFound("Record not found".into()))?;
        }
        _ => {
            return Err(AdminApiError::Unprocessable(
                "Only none, silence, and suspend actions are supported".into(),
            ));
        }
    }
    roosty_db::set_moderation_report_resolved(&txn, report_id, Some(actor)).await?;
    roosty_db::insert_admin_audit_entry(
        &txn,
        Some(actor),
        AdminSource::Api.audit_source(),
        AdminAuditAction::ReportResolve,
        AdminAuditTargetKind::Report,
        &report_id.to_string(),
        json!({"account_id": account_id.0, "action": action_type}),
    )
    .await?;
    txn.commit().await?;
    Ok(Json(json!({})).into_response())
}

async fn unsuspend_account(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    token: AuthenticatedAccessToken,
    Path(account_id): Path<Uuid>,
) -> AdminResult<Response> {
    let actor = require_admin(
        &token,
        AdminPermission::Write(AdminWritePermission::Accounts),
    )?;
    let account = set_account_suspended(
        &state,
        &database,
        actor,
        AdminSource::Api,
        AccountId(account_id),
        false,
    )
    .await?;
    Ok(Json(AdminAccountResponse::from(account)).into_response())
}

async fn delete_suspended_account(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    token: AuthenticatedAccessToken,
    Path(account_id): Path<Uuid>,
) -> AdminResult<Response> {
    let actor = require_admin(
        &token,
        AdminPermission::Write(AdminWritePermission::Accounts),
    )?;
    let account_id = AccountId(account_id);
    let txn = database.begin_write().await?;
    let account = roosty_db::find_admin_account_by_id(&txn, account_id)
        .await?
        .ok_or(AdminApiError::NotFound("Record not found".into()))?;
    if !account.suspended {
        return Err(AdminApiError::Forbidden("Account is not suspended".into()));
    }
    let paths = if account.domain.is_none() {
        roosty_db::purge_suspended_local_account(&txn, account_id).await?
    } else {
        Vec::new()
    };
    roosty_db::insert_admin_audit_entry(
        &txn,
        Some(actor),
        AdminSource::Api.audit_source(),
        AdminAuditAction::AccountPurge,
        if account.domain.is_some() {
            AdminAuditTargetKind::RemoteActor
        } else {
            AdminAuditTargetKind::LocalAccount
        },
        &account_id.0.to_string(),
        json!({"username": account.username, "domain": account.domain}),
    )
    .await?;
    txn.commit().await?;
    for path in paths {
        fs::remove_file(FsPath::new(&state.config.media_root).join(path))
            .await
            .ok();
    }
    let txn = database.begin_read().await?;
    let account = roosty_db::find_admin_account_by_id(&txn, account_id)
        .await?
        .ok_or(AdminApiError::NotFound("Record not found".into()))?;
    txn.commit().await?;
    Ok(Json(AdminAccountResponse::from(account)).into_response())
}

async fn operations_summary(
    Extension(database): Extension<DatabaseContext>,
    token: AuthenticatedAccessToken,
) -> AdminResult<Response> {
    require_admin(&token, AdminPermission::Read(AdminReadPermission::All))?;
    let txn = database.begin_snapshot().await?;
    let summary = roosty_db::admin_job_summary(&txn).await?;
    txn.commit().await?;
    Ok(Json(OperationSummaryResponse::from(summary)).into_response())
}

#[derive(Deserialize)]
struct PageQuery {
    limit: Option<u64>,
    max_id: Option<Uuid>,
}

async fn jobs(
    Extension(database): Extension<DatabaseContext>,
    token: AuthenticatedAccessToken,
    Query(params): Query<PageQuery>,
) -> AdminResult<Response> {
    require_admin(&token, AdminPermission::Read(AdminReadPermission::All))?;
    let txn = database.begin_snapshot().await?;
    let jobs = roosty_db::admin_job_diagnostics(
        &txn,
        params.limit.unwrap_or(40).clamp(1, 100),
        params.max_id,
    )
    .await?;
    txn.commit().await?;
    Ok(Json(
        jobs.into_iter()
            .map(AdminJobResponse::from)
            .collect::<Vec<_>>(),
    )
    .into_response())
}

async fn audit_log(
    Extension(database): Extension<DatabaseContext>,
    token: AuthenticatedAccessToken,
    Query(params): Query<PageQuery>,
) -> AdminResult<Response> {
    require_admin(&token, AdminPermission::Read(AdminReadPermission::All))?;
    let txn = database.begin_snapshot().await?;
    let entries = roosty_db::list_admin_audit_entries(
        &txn,
        params.limit.unwrap_or(40).clamp(1, 100),
        params.max_id,
    )
    .await?;
    txn.commit().await?;
    Ok(Json(
        entries
            .into_iter()
            .map(AdminAuditResponse::from)
            .collect::<Vec<_>>(),
    )
    .into_response())
}

#[derive(Deserialize)]
struct CreateAccountRequest {
    username: String,
    email: String,
    #[serde(default)]
    admin: bool,
}

async fn create_account(
    Extension(database): Extension<DatabaseContext>,
    token: AuthenticatedAccessToken,
    Json(request): Json<CreateAccountRequest>,
) -> AdminResult<Response> {
    let actor = require_admin(
        &token,
        AdminPermission::Write(AdminWritePermission::Accounts),
    )?;
    let txn = database.begin_write().await?;
    let result = create_local_account_in_transaction(
        &txn,
        Some(actor),
        AdminSource::Api,
        &request.username,
        &request.email,
        request.admin,
    )
    .await?;
    txn.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(TemporaryCredentialResponse::from(result)),
    )
        .into_response())
}

async fn reset_password(
    Extension(database): Extension<DatabaseContext>,
    token: AuthenticatedAccessToken,
    Path(account_id): Path<Uuid>,
) -> AdminResult<Response> {
    let actor = require_admin(
        &token,
        AdminPermission::Write(AdminWritePermission::Accounts),
    )?;
    let txn = database.begin_write().await?;
    roosty_db::find_admin_account_by_id(&txn, AccountId(account_id))
        .await?
        .ok_or(AdminApiError::NotFound("Record not found".into()))?;
    let result = reset_local_password_in_transaction(
        &txn,
        Some(actor),
        AdminSource::Api,
        AccountId(account_id),
    )
    .await?;
    txn.commit().await?;
    Ok(Json(TemporaryCredentialResponse::from(result)).into_response())
}

#[derive(Serialize)]
pub(crate) struct AdminAccountResponse {
    id: String,
    username: String,
    domain: Option<String>,
    created_at: String,
    email: String,
    ip: Option<String>,
    ips: Vec<Value>,
    locale: Option<String>,
    invite_request: Option<String>,
    role: Option<AdminRoleResponse>,
    confirmed: bool,
    approved: bool,
    disabled: bool,
    sensitized: bool,
    silenced: bool,
    suspended: bool,
    account: Value,
}

impl From<AdminAccount> for AdminAccountResponse {
    fn from(account: AdminAccount) -> Self {
        let acct = account.domain.as_ref().map_or_else(
            || account.username.clone(),
            |domain| format!("{}@{domain}", account.username),
        );
        let role = account.domain.is_none().then(|| AdminRoleResponse {
            id: if account.is_admin { "1" } else { "0" }.to_owned(),
            name: if account.is_admin { "Admin" } else { "User" }.to_owned(),
            permissions: if account.is_admin { "1" } else { "0" }.to_owned(),
        });
        Self {
            id: account.id.0.to_string(),
            username: account.username.clone(),
            domain: account.domain,
            created_at: format_timestamp(account.created_at),
            email: account.email.unwrap_or_default(),
            ip: None,
            ips: Vec::new(),
            locale: None,
            invite_request: None,
            role,
            confirmed: true,
            approved: true,
            disabled: false,
            sensitized: false,
            silenced: account.limited,
            suspended: account.suspended,
            account: json!({
                "id": account.id.0.to_string(),
                "username": account.username,
                "acct": acct,
                "display_name": account.display_name,
                "limited": account.limited,
                "created_at": format_timestamp(account.created_at),
            }),
        }
    }
}

#[derive(Serialize)]
struct AdminRoleResponse {
    id: String,
    name: String,
    permissions: String,
}

#[derive(Serialize)]
struct TemporaryCredentialResponse {
    account: AdminAccountResponse,
    temporary_password: String,
}

impl From<TemporaryCredential> for TemporaryCredentialResponse {
    fn from(result: TemporaryCredential) -> Self {
        Self {
            account: result.account.into(),
            temporary_password: result.temporary_password,
        }
    }
}

#[derive(Serialize)]
struct OperationSummaryResponse {
    due: u64,
    in_progress: u64,
    scheduled_retries: u64,
    permanently_failed: u64,
    oldest_due_at: Option<String>,
}

impl From<AdminJobSummary> for OperationSummaryResponse {
    fn from(summary: AdminJobSummary) -> Self {
        Self {
            due: summary.due,
            in_progress: summary.in_progress,
            scheduled_retries: summary.scheduled_retries,
            permanently_failed: summary.permanently_failed,
            oldest_due_at: summary.oldest_due_at.map(format_timestamp),
        }
    }
}

#[derive(Serialize)]
struct AdminJobResponse {
    id: String,
    kind: String,
    state: &'static str,
    attempts: u32,
    run_after: String,
    locked_at: Option<String>,
    last_error: Option<String>,
    created_at: String,
    completed_at: Option<String>,
    permanently_failed_at: Option<String>,
}

impl From<AdminJobDiagnostic> for AdminJobResponse {
    fn from(job: AdminJobDiagnostic) -> Self {
        let state = if job.permanently_failed_at.is_some() {
            "permanently_failed"
        } else if job.locked_at.is_some() {
            "in_progress"
        } else if job.attempts > 0 {
            "retry_scheduled"
        } else {
            "due"
        };
        Self {
            id: job.id.0.to_string(),
            kind: job.kind.as_str().to_owned(),
            state,
            attempts: job.attempts,
            run_after: format_timestamp(job.run_after),
            locked_at: job.locked_at.map(format_timestamp),
            last_error: job.last_error,
            created_at: format_timestamp(job.created_at),
            completed_at: job.completed_at.map(format_timestamp),
            permanently_failed_at: job.permanently_failed_at.map(format_timestamp),
        }
    }
}

#[derive(Serialize)]
struct AdminAuditResponse {
    id: String,
    actor_account_id: Option<String>,
    source: String,
    action: String,
    target_kind: String,
    target_id: String,
    metadata: Value,
    created_at: String,
}

impl From<AdminAuditEntry> for AdminAuditResponse {
    fn from(entry: AdminAuditEntry) -> Self {
        Self {
            id: entry.id.to_string(),
            actor_account_id: entry.actor_account_id.map(|id| id.0.to_string()),
            source: entry.source.to_string(),
            action: entry.action.to_string(),
            target_kind: entry.target_kind.to_string(),
            target_id: entry.target_id,
            metadata: entry.metadata,
            created_at: format_timestamp(entry.created_at),
        }
    }
}

fn format_timestamp(timestamp: OffsetDateTime) -> String {
    timestamp
        .format(&Rfc3339)
        .unwrap_or_else(|_| timestamp.unix_timestamp().to_string())
}

fn validate_username(username: &str) -> Result<(), RoostyError> {
    account_validation::username(username)
        .map_err(|reason| RoostyError::InvalidInput(format!("username {reason}")))
}

fn validate_email(email: &str) -> Result<(), RoostyError> {
    account_validation::email(email)
        .map_err(|reason| RoostyError::InvalidInput(format!("email {reason}")))
}

fn api_error(status: StatusCode, description: &str) -> Response {
    (status, Json(json!({ "error": description }))).into_response()
}

fn server_error(error: RoostyError) -> Response {
    tracing::error!(%error, "administrator request failed");
    api_error(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
}

#[cfg(test)]
mod tests {
    use super::{AdminPermission, AdminReadPermission, AdminWritePermission};

    /// Given administrator scopes, when a granular account read is checked, then its exact and
    /// umbrella grants work without treating ordinary read scopes as privileged.
    #[test]
    fn administrator_scope_matching_is_hierarchical_only_within_admin_scopes() {
        let account_read = AdminPermission::Read(AdminReadPermission::Accounts);
        assert!(account_read.allows_scope("admin:read:accounts"));
        assert!(account_read.allows_scope("admin:read"));
        assert!(!account_read.allows_scope("read:accounts"));
        assert!(!account_read.allows_scope("admin:write"));

        let dashboard_read = AdminPermission::Read(AdminReadPermission::All);
        assert!(dashboard_read.allows_scope("admin:read"));
        assert!(!dashboard_read.allows_scope("admin:read:accounts"));

        let account_write = AdminPermission::Write(AdminWritePermission::Accounts);
        assert!(account_write.allows_scope("admin:write"));
        assert!(account_write.allows_scope("admin:write:accounts"));
        assert!(!account_write.allows_scope("admin:read"));

        let domain_read = AdminPermission::Read(AdminReadPermission::DomainBlocks);
        assert!(domain_read.allows_scope("admin:read"));
        assert!(domain_read.allows_scope("admin:read:domain_blocks"));
        assert!(!domain_read.allows_scope("admin:read:accounts"));

        let domain_write = AdminPermission::Write(AdminWritePermission::DomainBlocks);
        assert!(domain_write.allows_scope("admin:write"));
        assert!(domain_write.allows_scope("admin:write:domain_blocks"));
        assert!(!domain_write.allows_scope("admin:write:accounts"));
    }
}
