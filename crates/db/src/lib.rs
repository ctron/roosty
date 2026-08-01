#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use linkify::{LinkFinder, LinkKind};
use rand_core::{OsRng, RngCore};
use roosty_core::{
    AccountId, AccountRelationshipError, JobClaimId, JobId, Result, RoostyError, StatusId,
};
use scraper::{Html, Selector};
use sea_orm::{
    AccessMode, ActiveModelTrait, ActiveValue, ColumnTrait, Condition, ConnectionTrait, Database,
    DatabaseBackend, DatabaseConnection, DatabaseTransaction, DbErr, DeriveValueType, EntityTrait,
    FromQueryResult, IntoActiveModel, ModelTrait, PaginatorTrait, QueryFilter, QueryOrder,
    QuerySelect, Select, Set, Statement, TransactionTrait, TryFromU64, TryInsertResult,
    sea_query::{Expr, Func, OnConflict, Query},
};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};
use std::{
    cmp::Reverse,
    collections::{HashMap, HashSet},
    mem,
    net::IpAddr,
    str::FromStr,
};
use strum::{Display, EnumString, IntoStaticStr};
use time::{Date, Duration, OffsetDateTime};
use url::{Host, Url};
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

/// Closed Mastodon status visibility values, serialized as text at persistence and API boundaries.
#[derive(
    Clone,
    Copy,
    Debug,
    DeriveValueType,
    Display,
    EnumString,
    Eq,
    IntoStaticStr,
    PartialEq,
    Serialize,
    Deserialize,
)]
#[sea_orm(value_type = "String")]
#[strum(serialize_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum StatusVisibility {
    Public,
    Unlisted,
    Private,
    Direct,
}

/// Action applied when a Mastodon notification-policy predicate matches.
#[derive(
    Clone,
    Copy,
    Debug,
    DeriveValueType,
    Display,
    EnumString,
    Eq,
    IntoStaticStr,
    PartialEq,
    Serialize,
    Deserialize,
)]
#[sea_orm(value_type = "String")]
#[strum(serialize_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum NotificationPolicyAction {
    Accept,
    Filter,
    Drop,
}

/// Mastodon-compatible enforcement level for one federated domain.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    DeriveValueType,
    Display,
    EnumString,
    Eq,
    IntoStaticStr,
    Ord,
    PartialEq,
    PartialOrd,
    Serialize,
    Deserialize,
)]
#[sea_orm(value_type = "String")]
#[strum(serialize_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum DomainBlockSeverity {
    #[default]
    Noop,
    Silence,
    Suspend,
}

/// Lifecycle state of a sender-scoped filtered-notification request.
#[derive(
    Clone, Copy, Debug, DeriveValueType, Display, EnumString, Eq, IntoStaticStr, PartialEq,
)]
#[sea_orm(value_type = "String")]
#[strum(serialize_all = "snake_case")]
pub enum NotificationRequestState {
    Pending,
    Merging,
    Dismissed,
}

/// Closed event names persisted in the cross-process streaming log.
#[derive(
    Clone, Copy, Debug, DeriveValueType, Display, EnumString, Eq, IntoStaticStr, PartialEq,
)]
#[sea_orm(value_type = "String")]
#[strum(serialize_all = "snake_case")]
pub enum StreamingEventKind {
    Update,
    StatusUpdate,
    Notification,
    Conversation,
    Delete,
    NotificationsMerged,
}

/// Origin of a status-like event used for public stream routing.
#[derive(
    Clone, Copy, Debug, DeriveValueType, Display, EnumString, Eq, IntoStaticStr, PartialEq,
)]
#[sea_orm(value_type = "String")]
#[strum(serialize_all = "snake_case")]
pub enum StreamingStatusOrigin {
    Local,
    Remote,
}

/// A streaming event ready to be persisted and announced to other processes.
#[derive(Clone, Debug)]
pub struct NewStreamingEvent {
    pub origin_process_id: Uuid,
    pub kind: StreamingEventKind,
    pub payload: String,
    pub account_id: AccountId,
    pub recipient_ids: Vec<AccountId>,
    pub notification_recipient_ids: Vec<AccountId>,
    pub visibility: StatusVisibility,
    pub status_origin: StreamingStatusOrigin,
    pub has_media: bool,
}

/// One ordered event recovered from the retained cross-process log.
#[derive(Clone, Debug)]
pub struct RetainedStreamingEvent {
    pub sequence: i64,
    pub origin_process_id: Uuid,
    pub kind: StreamingEventKind,
    pub payload: String,
    pub account_id: AccountId,
    pub recipient_ids: Vec<AccountId>,
    pub notification_recipient_ids: Vec<AccountId>,
    pub visibility: StatusVisibility,
    pub status_origin: StreamingStatusOrigin,
    pub has_media: bool,
}

mod entity;
mod reports;

pub use reports::*;

impl StatusVisibility {
    /// Parse a persisted or wire visibility without accepting unknown values.
    pub fn parse(value: &str) -> Result<Self> {
        Ok(Self::from_str(value)?)
    }

    pub fn as_str(self) -> &'static str {
        self.into()
    }
}

use entity::{
    federation_domain_block, job, local_account, local_account_block, local_account_mute,
    local_actor_key, local_conversation, local_conversation_account,
    local_conversation_remote_participant, local_featured_tag, local_follow, local_list,
    local_list_local_member, local_list_remote_member, local_media_attachment, local_notification,
    local_notification_permission, local_notification_policy, local_notification_request,
    local_remote_account_block, local_remote_account_mute, local_remote_status_favourite,
    local_remote_status_reblog, local_status, local_status_bookmark, local_status_edit,
    local_status_edit_media, local_status_favourite, local_status_local_mention,
    local_status_local_recipient, local_status_pin, local_status_reblog,
    local_status_remote_mention, local_status_tag, local_tag, local_tag_follow,
    local_timeline_marker, oauth_access_token, oauth_application, oauth_authorization_code,
    preview_card, processed_inbox_activity, push_subscription, remote_actor, remote_custom_emoji,
    remote_featured_tag, remote_follow, remote_following, remote_local_account_block,
    remote_media_attachment, remote_profile_media, remote_status, remote_status_edit,
    remote_status_edit_media, remote_status_favourite, remote_status_local_mention,
    remote_status_local_recipient, remote_status_pin, remote_status_reblog,
    remote_status_remote_recipient, remote_status_tag, scheduled_status,
    status_creation_idempotency, status_preview_card, status_preview_scan, status_quote,
    status_search_document, streaming_event,
};

mod polls;
pub use polls::*;

/// Quote policy values authored by Mastodon-compatible clients.
#[derive(
    Clone,
    Copy,
    Debug,
    DeriveValueType,
    Display,
    EnumString,
    Eq,
    IntoStaticStr,
    PartialEq,
    Serialize,
    Deserialize,
)]
#[sea_orm(value_type = "String")]
#[strum(serialize_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum QuoteApprovalPolicy {
    Public,
    Followers,
    Nobody,
}

/// Closed reply filtering policies exposed by Mastodon's List entity.
#[derive(
    Clone,
    Copy,
    Debug,
    DeriveValueType,
    Display,
    EnumString,
    Eq,
    IntoStaticStr,
    PartialEq,
    Serialize,
    Deserialize,
)]
#[sea_orm(value_type = "String")]
#[strum(serialize_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ListRepliesPolicy {
    Followed,
    List,
    None,
}

/// A private timeline list owned by one local account.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalList {
    pub id: Uuid,
    pub account_id: AccountId,
    pub title: String,
    pub replies_policy: ListRepliesPolicy,
    pub exclusive: bool,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// A local or cached-remote account returned from a list membership page.
#[derive(Clone, Debug)]
pub enum ListAccount {
    Local(LocalAccount),
    Remote(RemoteActor),
}

/// Validation outcome when atomically adding accounts to a list.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddListAccountsResult {
    Added,
    ListNotFound,
    AccountNotFollowed,
    AlreadyPresent,
}

impl QuoteApprovalPolicy {
    pub fn parse(value: &str) -> Result<Self> {
        Self::from_str(value).map_err(Into::into)
    }
}

/// Durable consent state for one quote edge.
#[derive(
    Clone, Copy, Debug, DeriveValueType, Display, EnumString, Eq, IntoStaticStr, PartialEq,
)]
#[sea_orm(value_type = "String")]
#[strum(serialize_all = "snake_case")]
pub enum QuoteState {
    Pending,
    Accepted,
    Rejected,
    Revoked,
    Deleted,
}

/// A local or cached-remote status identity.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum StatusReference {
    Local(StatusId),
    Remote(StatusId),
}

/// Stored quote edge and its authorization lifecycle.
#[derive(Clone, Debug)]
pub struct StatusQuote {
    pub id: Uuid,
    pub quoting_status: StatusReference,
    pub quoted_status: Option<StatusReference>,
    pub quoted_activitypub_id: String,
    pub state: QuoteState,
    pub quote_request_id: Option<String>,
    pub authorization_id: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// Shared database connection type used across Roosty crates.
pub type DbConnection = DatabaseConnection;

/// Result of registering a durable inbound ActivityPub identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InboxReplayResult {
    /// This activity identity was not previously observed.
    New,
    /// The signer and canonical payload match an existing marker.
    Duplicate,
    /// The activity identity was reused by another signer or payload.
    Conflict,
}

/// Durable processing outcome stored for an inbound ActivityPub activity.
#[derive(
    Clone, Copy, Debug, DeriveValueType, Display, EnumString, Eq, IntoStaticStr, PartialEq,
)]
#[sea_orm(value_type = "String")]
#[strum(serialize_all = "snake_case")]
pub enum InboxActivityOutcome {
    Accepted,
    Ignored,
}

/// ActivityPub activity kinds recorded by the inbox replay ledger.
#[derive(
    Clone, Copy, Debug, DeriveValueType, Display, EnumString, Eq, IntoStaticStr, PartialEq,
)]
#[sea_orm(value_type = "String")]
pub enum InboxActivityType {
    Follow,
    Accept,
    Reject,
    Create,
    Update,
    Delete,
    Like,
    Announce,
    Undo,
    Move,
    Block,
    Add,
    Remove,
    Flag,
    #[strum(serialize = "https://w3id.org/fep/044f#QuoteRequest")]
    QuoteRequest,
}

/// Immutable metadata stored for a processed inbound ActivityPub activity.
#[derive(Clone, Copy, Debug)]
pub struct InboxActivityMetadata<'a> {
    pub activity_id: &'a str,
    pub remote_actor_id: AccountId,
    pub payload_digest: &'a [u8; 32],
    pub activity_type: InboxActivityType,
    pub outcome: InboxActivityOutcome,
}

/// A durable job to be inserted as part of a larger database operation.
///
/// Callers use this to implement the transactional-outbox pattern: the state
/// change and its eventual side effect become visible together.
#[derive(Clone, Debug)]
pub struct NewJob {
    /// Worker dispatch kind.
    pub kind: JobKind,
    /// Serialized worker input.
    pub payload: JsonValue,
    /// Optional active-job deduplication identity.
    pub deduplication_key: Option<String>,
    /// Earliest time at which a worker may claim the job.
    pub run_after: OffsetDateTime,
}

/// Derived status links persisted with a local status mutation.
#[derive(Clone, Debug, Default)]
pub struct LocalStatusMetadata {
    /// Schedule allowed to transfer its reserved media into this status.
    pub scheduled_status_id: Option<Uuid>,
    /// Normalized hashtag names linked to the status.
    pub tag_names: Vec<String>,
    /// Resolved remote actors explicitly mentioned by the status.
    pub remote_actor_ids: Vec<AccountId>,
    /// Local accounts explicitly addressed by a direct status.
    pub local_recipient_ids: Vec<AccountId>,
    /// Local accounts currently mentioned by the status, independent of visibility.
    pub local_mention_ids: Vec<AccountId>,
}

/// An attachment declared by a verified remote Note.
#[derive(Clone, Debug)]
pub struct NewRemoteMediaAttachment {
    pub remote_url: String,
    pub content_type: Option<String>,
    pub description: Option<String>,
}

/// Lifecycle state of a locally cached remote attachment.
#[derive(
    Clone, Copy, Debug, DeriveValueType, Display, EnumString, Eq, IntoStaticStr, PartialEq,
)]
#[sea_orm(value_type = "String")]
#[strum(serialize_all = "snake_case")]
pub enum RemoteMediaState {
    Pending,
    Ready,
    Failed,
}

/// Lifecycle state of an asynchronously fetched preview card.
#[derive(
    Clone, Copy, Debug, DeriveValueType, Display, EnumString, Eq, IntoStaticStr, PartialEq,
)]
#[sea_orm(value_type = "String")]
#[strum(serialize_all = "snake_case")]
pub enum PreviewFetchState {
    Pending,
    Ready,
    Failed,
}

/// Origin namespace for a status author counted in link usage.
#[derive(
    Clone, Copy, Debug, DeriveValueType, Display, EnumString, Eq, IntoStaticStr, PartialEq,
)]
#[sea_orm(value_type = "String")]
#[strum(serialize_all = "snake_case")]
pub enum PreviewActorOrigin {
    Local,
    Remote,
}

/// Cached remote attachment metadata exposed to API projections.
#[derive(Clone, Debug)]
pub struct RemoteMediaAttachment {
    pub id: Uuid,
    pub remote_status_id: StatusId,
    pub status_order: i32,
    pub remote_url: String,
    pub content_type: Option<String>,
    pub description: Option<String>,
    pub state: RemoteMediaState,
    pub file_path: Option<String>,
    pub preview_file_path: Option<String>,
    pub file_size: Option<i64>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub preview_width: Option<i32>,
    pub preview_height: Option<i32>,
    pub blurhash: Option<String>,
    pub expires_at: Option<OffsetDateTime>,
}

/// Immutable media projection stored with a status revision.
#[derive(Clone, Debug)]
pub struct StatusEditMedia {
    pub id: Uuid,
    pub content_type: Option<String>,
    pub file_path: Option<String>,
    pub preview_file_path: Option<String>,
    pub remote_url: Option<String>,
    pub description: Option<String>,
    pub focus_x: Option<f64>,
    pub focus_y: Option<f64>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub preview_width: Option<i32>,
    pub preview_height: Option<i32>,
    pub blurhash: Option<String>,
}

/// One immutable revision of a locally authored status.
#[derive(Clone, Debug)]
pub struct LocalStatusEdit {
    pub content: String,
    pub spoiler_text: String,
    pub sensitive: bool,
    pub local_mention_ids: Vec<AccountId>,
    pub remote_mention_ids: Vec<AccountId>,
    pub tag_names: Vec<String>,
    pub poll_options: Option<Vec<String>>,
    pub created_at: OffsetDateTime,
    pub media: Vec<StatusEditMedia>,
}

/// One immutable revision of a cached remote status.
#[derive(Clone, Debug)]
pub struct RemoteStatusEdit {
    pub content: String,
    pub spoiler_text: String,
    pub sensitive: bool,
    pub object: JsonValue,
    pub poll_options: Option<Vec<String>>,
    pub created_at: OffsetDateTime,
    pub media: Vec<StatusEditMedia>,
}

/// Outcome of a status edit after row locking and material-change comparison.
#[derive(Clone, Debug)]
pub enum LocalStatusUpdateResult {
    Updated(LocalStatus),
    Unchanged(LocalStatus),
}

/// Outcome of caching a verified remote Create or Update.
#[derive(Clone, Debug)]
pub enum RemoteStatusUpsertResult {
    Created(RemoteStatus),
    Updated(RemoteStatus),
    Unchanged(RemoteStatus),
}

/// A remote custom emoji discovered in a signed actor or Note document.
#[derive(Clone, Debug)]
pub struct NewRemoteCustomEmoji {
    pub shortcode: String,
    pub remote_url: String,
}

/// Cached remote custom emoji metadata.
#[derive(Clone, Debug)]
pub struct RemoteCustomEmoji {
    pub id: Uuid,
    pub shortcode: String,
    pub remote_url: String,
    pub content_type: Option<String>,
    pub state: RemoteMediaState,
    pub file_path: Option<String>,
    pub expires_at: Option<OffsetDateTime>,
}

/// Completed cache metadata for one remote status attachment.
#[derive(Clone, Debug)]
pub struct RemoteMediaCacheWrite {
    pub content_type: String,
    pub file_path: String,
    pub preview_file_path: Option<String>,
    pub file_size: i64,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub preview_width: Option<i32>,
    pub preview_height: Option<i32>,
    pub blurhash: Option<String>,
    pub expires_at: OffsetDateTime,
}

/// The two actor image slots understood by Mastodon-compatible clients.
#[derive(
    Clone, Copy, Debug, DeriveValueType, Display, EnumString, Eq, IntoStaticStr, PartialEq,
)]
#[sea_orm(value_type = "String")]
#[strum(serialize_all = "snake_case")]
pub enum RemoteProfileMediaKind {
    Avatar,
    Header,
}

/// A remote actor image cached independently from status attachments.
#[derive(Clone, Debug)]
pub struct RemoteProfileMedia {
    pub id: Uuid,
    pub remote_actor_id: AccountId,
    pub kind: RemoteProfileMediaKind,
    pub remote_url: String,
    pub content_type: Option<String>,
    pub state: RemoteMediaState,
    pub file_path: Option<String>,
    pub expires_at: Option<OffsetDateTime>,
}

/// The remote actor profile images discovered from its ActivityPub document.
#[derive(Clone, Debug, Default)]
pub struct NewRemoteProfileMedia {
    pub avatar_url: Option<String>,
    pub header_url: Option<String>,
}

/// Open a database connection using SeaORM's PostgreSQL driver.
pub async fn connect(database_url: &str) -> Result<DbConnection> {
    Ok(Database::connect(database_url).await?)
}

/// Verify that the database connection can execute a trivial query.
pub async fn ping(db: &impl ConnectionTrait) -> Result<()> {
    db.query_one(Statement::from_string(
        DatabaseBackend::Postgres,
        "SELECT 1".to_owned(),
    ))
    .await?;

    Ok(())
}

/// Persist one streaming event and notify listeners with only its sequence.
///
/// PostgreSQL delivers the notification at commit, so listeners never observe
/// a sequence before its row is queryable.
pub async fn publish_streaming_event(db: &DbConnection, event: NewStreamingEvent) -> Result<i64> {
    let transaction = db.begin().await?;
    let recipient_ids = event
        .recipient_ids
        .iter()
        .map(|account_id| account_id.0)
        .collect::<Vec<_>>();
    let notification_recipient_ids = event
        .notification_recipient_ids
        .iter()
        .map(|account_id| account_id.0)
        .collect::<Vec<_>>();
    let model = streaming_event::ActiveModel {
        sequence: ActiveValue::NotSet,
        origin_process_id: Set(event.origin_process_id),
        event_kind: Set(event.kind),
        payload: Set(event.payload),
        account_id: Set(event.account_id.0),
        recipient_ids: Set(serde_json::json!(recipient_ids)),
        notification_recipient_ids: Set(serde_json::json!(notification_recipient_ids)),
        visibility: Set(event.visibility),
        status_origin: Set(event.status_origin),
        has_media: Set(event.has_media),
        created_at: ActiveValue::NotSet,
    }
    .insert(&transaction)
    .await?;
    transaction
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT pg_notify('roosty_streaming_events', $1)",
            [model.sequence.to_string().into()],
        ))
        .await?;
    transaction.commit().await?;

    Ok(model.sequence)
}

/// Return the newest retained streaming sequence, or zero for an empty log.
pub async fn latest_streaming_event_sequence(db: &DbConnection) -> Result<i64> {
    Ok(streaming_event::Entity::find()
        .order_by_desc(streaming_event::Column::Sequence)
        .one(db)
        .await?
        .map_or(0, |event| event.sequence))
}

/// Fetch retained streaming events after a cursor in global sequence order.
pub async fn streaming_events_after(
    db: &DbConnection,
    sequence: i64,
) -> Result<Vec<RetainedStreamingEvent>> {
    streaming_event::Entity::find()
        .filter(streaming_event::Column::Sequence.gt(sequence))
        .order_by_asc(streaming_event::Column::Sequence)
        .all(db)
        .await?
        .into_iter()
        .map(retained_streaming_event)
        .collect()
}

/// Delete streaming coordination rows older than the retention cutoff.
pub async fn delete_streaming_events_before(
    db: &DbConnection,
    cutoff: OffsetDateTime,
) -> Result<u64> {
    Ok(streaming_event::Entity::delete_many()
        .filter(streaming_event::Column::CreatedAt.lt(cutoff))
        .exec(db)
        .await?
        .rows_affected)
}

fn retained_streaming_event(model: streaming_event::Model) -> Result<RetainedStreamingEvent> {
    let recipient_ids = serde_json::from_value::<Vec<Uuid>>(model.recipient_ids)
        .map_err(|error| {
            RoostyError::InvalidInput(format!("invalid streaming recipients: {error}"))
        })?
        .into_iter()
        .map(AccountId)
        .collect();
    let notification_recipient_ids =
        serde_json::from_value::<Vec<Uuid>>(model.notification_recipient_ids)
            .map_err(|error| {
                RoostyError::InvalidInput(format!(
                    "invalid streaming notification recipients: {error}"
                ))
            })?
            .into_iter()
            .map(AccountId)
            .collect();
    Ok(RetainedStreamingEvent {
        sequence: model.sequence,
        origin_process_id: model.origin_process_id,
        kind: model.event_kind,
        payload: model.payload,
        account_id: AccountId(model.account_id),
        recipient_ids,
        notification_recipient_ids,
        visibility: model.visibility,
        status_origin: model.status_origin,
        has_media: model.has_media,
    })
}

/// Create the first local administrator account.
///
/// This refuses to run once any local account already exists.
pub async fn create_bootstrap_admin(
    db: &DbConnection,
    username: &str,
    email: &str,
    password_hash: &str,
) -> Result<Uuid> {
    let txn = db.begin().await?;
    let count = local_account::Entity::find().count(&txn).await?;
    if count != 0 {
        return Err(RoostyError::InvalidInput(
            "bootstrap is only allowed before local accounts exist".to_owned(),
        ));
    }

    let account_id = insert_local_account(&txn, username, email, password_hash, true).await?;
    txn.commit().await?;
    Ok(account_id)
}

/// Create a non-admin local account.
pub async fn create_local_account(
    db: &DbConnection,
    username: &str,
    email: &str,
    password_hash: &str,
) -> Result<Uuid> {
    let txn = db.begin().await?;
    let account_id =
        create_local_account_in_transaction(&txn, username, email, password_hash).await?;
    txn.commit().await?;
    Ok(account_id)
}

/// Create a non-admin account inside a caller-owned transaction.
pub async fn create_local_account_in_transaction(
    txn: &DatabaseTransaction,
    username: &str,
    email: &str,
    password_hash: &str,
) -> Result<Uuid> {
    insert_local_account(txn, username, email, password_hash, false).await
}

/// Create an administrator local account after bootstrap.
pub async fn create_admin_account(
    db: &DbConnection,
    username: &str,
    email: &str,
    password_hash: &str,
) -> Result<Uuid> {
    let txn = db.begin().await?;
    let account_id =
        create_admin_account_in_transaction(&txn, username, email, password_hash).await?;
    txn.commit().await?;
    Ok(account_id)
}

/// Create an administrator account inside a caller-owned transaction.
pub async fn create_admin_account_in_transaction(
    txn: &DatabaseTransaction,
    username: &str,
    email: &str,
    password_hash: &str,
) -> Result<Uuid> {
    insert_local_account(txn, username, email, password_hash, true).await
}

/// Insert an account and its required default policy as one transaction unit.
async fn insert_local_account(
    txn: &DatabaseTransaction,
    username: &str,
    email: &str,
    password_hash: &str,
    is_admin: bool,
) -> Result<Uuid> {
    ensure_local_account_available(txn, username, email).await?;

    let account_id = Uuid::now_v7();
    local_account::ActiveModel {
        id: Set(account_id),
        username: Set(username.to_owned()),
        email: Set(email.to_owned()),
        password_hash: Set(password_hash.to_owned()),
        is_admin: Set(is_admin),
        ..Default::default()
    }
    .insert(txn)
    .await?;
    local_notification_policy::ActiveModel {
        account_id: Set(account_id),
        for_not_following: Set(NotificationPolicyAction::Accept),
        for_not_followers: Set(NotificationPolicyAction::Accept),
        for_new_accounts: Set(NotificationPolicyAction::Accept),
        for_private_mentions: Set(NotificationPolicyAction::Filter),
        for_limited_accounts: Set(NotificationPolicyAction::Filter),
        updated_at: Set(OffsetDateTime::now_utc()),
    }
    .insert(txn)
    .await?;

    Ok(account_id)
}

/// Reject account creation when the requested username or email is already in use.
async fn ensure_local_account_available(
    db: &impl ConnectionTrait,
    username: &str,
    email: &str,
) -> Result<()> {
    if local_account::Entity::find()
        .filter(local_account::Column::Username.eq(username))
        .one(db)
        .await?
        .is_some()
    {
        return Err(RoostyError::InvalidInput(
            "username is already in use".to_owned(),
        ));
    }

    if local_account::Entity::find()
        .filter(local_account::Column::Email.eq(email))
        .one(db)
        .await?
        .is_some()
    {
        return Err(RoostyError::InvalidInput(
            "email is already in use".to_owned(),
        ));
    }

    Ok(())
}

/// Local account data used by authentication and account API responses.
#[derive(Clone, Debug)]
pub struct LocalAccount {
    /// Internal account identifier.
    pub id: AccountId,
    /// Local username without a domain.
    pub username: String,
    /// Account email address.
    pub email: String,
    /// Argon2 password hash.
    pub password_hash: String,
    /// Whether this account has administrator privileges.
    pub is_admin: bool,
    /// Profile display name.
    pub display_name: String,
    /// Plain-text profile note.
    pub note: String,
    /// Whether follow requests require approval.
    pub locked: bool,
    /// Whether this account is automated.
    pub bot: bool,
    /// Whether this account can be discovered in profile directories.
    pub discoverable: bool,
    /// Default visibility for authored statuses.
    pub default_visibility: StatusVisibility,
    /// Whether authored statuses are sensitive by default.
    pub default_sensitive: bool,
    /// Default language for authored statuses.
    pub default_language: Option<String>,
    /// Default quote policy for authored statuses.
    pub default_quote_policy: QuoteApprovalPolicy,
    /// Profile metadata fields.
    pub profile_fields: JsonValue,
    /// Optional local avatar path relative to the media root.
    pub avatar_file_path: Option<String>,
    /// Optional local header image path relative to the media root.
    pub header_file_path: Option<String>,
    /// When an operator limited this account locally.
    pub limited_at: Option<OffsetDateTime>,
    /// When an administrator suspended this account.
    pub suspended_at: Option<OffsetDateTime>,
    /// When retained account content was permanently purged.
    pub data_purged_at: Option<OffsetDateTime>,
    /// Timestamp when the local account was created.
    pub created_at: OffsetDateTime,
}

/// Encrypted ActivityPub signing key material for a local actor.
#[derive(Clone, Debug)]
pub struct LocalActorKey {
    /// Actor's public key in PEM SubjectPublicKeyInfo encoding.
    pub public_key_pem: String,
    /// Authenticated-encrypted PKCS#8 private key bytes.
    pub private_key_ciphertext: Vec<u8>,
    /// AES-GCM nonce used to encrypt the private material.
    pub private_key_nonce: Vec<u8>,
}

/// Validated cached data for a remote ActivityPub actor.
#[derive(Clone, Debug)]
pub struct RemoteActor {
    /// UUID-backed identifier exposed through Mastodon account APIs.
    pub id: AccountId,
    /// Canonical HTTPS ActivityPub actor ID.
    pub activitypub_id: String,
    /// Remote username without domain.
    pub username: String,
    /// Remote actor's DNS domain.
    pub domain: String,
    /// Display name from the actor document.
    pub display_name: String,
    /// Profile summary from the actor document.
    pub summary: String,
    /// Original remote ActivityPub Emoji tags retained for API projection.
    pub emojis: JsonValue,
    /// Direct inbox URL.
    pub inbox_url: String,
    /// Optional shared inbox URL.
    pub shared_inbox_url: Option<String>,
    /// Exact followers collection URL validated from the actor document.
    pub followers_url: Option<String>,
    /// Exact same-origin featured collection URL declared by the actor.
    pub featured_url: Option<String>,
    /// Exact same-origin featured-tags collection URL declared by the actor.
    pub featured_tags_url: Option<String>,
    /// Public key identity URL.
    pub public_key_id: String,
    /// Public signing key PEM.
    pub public_key_pem: String,
    /// Cache expiry instant.
    pub expires_at: OffsetDateTime,
    /// Creation timestamp declared by the remote actor document, when available.
    pub profile_created_at: Option<OffsetDateTime>,
    /// Timestamp when this instance first cached the remote actor.
    pub first_seen_at: OffsetDateTime,
    /// When a signed Actor Delete tombstoned this cache entry.
    pub deleted_at: Option<OffsetDateTime>,
    /// Verified replacement actor declared through a signed Move activity.
    pub moved_to_remote_actor_id: Option<AccountId>,
    /// When an operator limited this cached actor locally.
    pub limited_at: Option<OffsetDateTime>,
    /// When an administrator directly suspended this cached actor.
    pub suspended_at: Option<OffsetDateTime>,
    /// When cached actor content was purged after suspension.
    pub data_purged_at: Option<OffsetDateTime>,
    /// Whether the remote actor explicitly opted into profile discovery.
    pub discoverable: Option<bool>,
}

/// One account returned by the unified Mastodon account search.
#[derive(Clone, Debug)]
pub enum AccountSearchResult {
    /// A local account hosted by this instance.
    Local(LocalAccount),
    /// An active actor held in the federation cache.
    Remote(RemoteActor),
}

/// Stable ordering modes exposed by Mastodon's profile directory.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AccountDirectoryOrder {
    /// Accounts with the newest active status first.
    #[default]
    Active,
    /// Accounts with the newest profile creation timestamp first.
    New,
}

/// Inputs for one offset-paginated profile-directory query.
pub struct AccountDirectoryOptions<'a> {
    pub viewer_account_id: Option<AccountId>,
    pub order: AccountDirectoryOrder,
    pub local_only: bool,
    pub limit: u64,
    pub offset: u64,
    pub blocked_remote_domains: &'a [String],
}

/// One directory account with counters loaded by the page query.
#[derive(Clone, Debug)]
pub struct DirectoryAccount {
    pub account: AccountSearchResult,
    pub followers_count: u64,
    pub following_count: u64,
    pub statuses_count: u64,
    pub last_status_at: Option<OffsetDateTime>,
}

/// One profile-directory page plus forward-pagination state.
pub struct AccountDirectoryPage {
    pub items: Vec<DirectoryAccount>,
    pub has_more: bool,
}

/// Inputs for a bounded, viewer-specific account suggestion query.
pub struct AccountSuggestionOptions<'a> {
    pub viewer_account_id: AccountId,
    pub limit: u64,
    pub offset: u64,
    pub blocked_remote_domains: &'a [String],
}

/// A suggested account with counters loaded by the ranking query.
pub type AccountSuggestion = DirectoryAccount;

/// Administrative projection shared by compatible and first-party account views.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminAccount {
    pub id: AccountId,
    pub username: String,
    pub domain: Option<String>,
    pub email: Option<String>,
    pub display_name: String,
    pub is_admin: bool,
    pub limited: bool,
    pub suspended: bool,
    pub data_purged_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
}

/// Persisted Mastodon-compatible domain moderation rule.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FederationDomainBlock {
    pub id: Uuid,
    pub domain: String,
    pub severity: DomainBlockSeverity,
    pub reject_media: bool,
    pub reject_reports: bool,
    pub private_comment: Option<String>,
    pub public_comment: Option<String>,
    pub obfuscate: bool,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// Fields accepted when an administrator creates a domain rule.
pub struct NewFederationDomainBlock {
    pub domain: String,
    pub severity: DomainBlockSeverity,
    pub reject_media: bool,
    pub reject_reports: bool,
    pub private_comment: Option<String>,
    pub public_comment: Option<String>,
    pub obfuscate: bool,
}

/// Optional replacements accepted by Mastodon's domain-block update endpoint.
#[derive(Default)]
pub struct FederationDomainBlockUpdate {
    pub severity: Option<DomainBlockSeverity>,
    pub reject_media: Option<bool>,
    pub reject_reports: Option<bool>,
    pub private_comment: Option<Option<String>>,
    pub public_comment: Option<Option<String>>,
    pub obfuscate: Option<bool>,
}

/// Effective policy after combining every exact or parent-domain rule.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FederationDomainPolicy {
    pub severity: DomainBlockSeverity,
    pub reject_media: bool,
    pub reject_reports: bool,
}

impl FederationDomainPolicy {
    pub fn is_suspended(self) -> bool {
        self.severity == DomainBlockSeverity::Suspend
    }

    pub fn is_limited(self) -> bool {
        self.severity >= DomainBlockSeverity::Silence
    }
}

/// Normalize a DNS domain for durable policy matching.
pub fn normalize_federation_domain(domain: &str) -> Result<String> {
    let domain = domain.trim().trim_end_matches('.');
    match Host::parse(domain) {
        Ok(Host::Domain(domain)) if domain.contains('.') && !domain.contains('*') => {
            Ok(domain.to_ascii_lowercase())
        }
        _ => Err(RoostyError::InvalidInput(
            "domain must be a public DNS name".to_owned(),
        )),
    }
}

/// Return all exact DNS suffixes that can cover one remote domain.
fn federation_domain_suffixes(domain: &str) -> Result<Vec<String>> {
    let domain = normalize_federation_domain(domain)?;
    let labels = domain.split('.').collect::<Vec<_>>();
    Ok((0..labels.len().saturating_sub(1))
        .map(|index| labels[index..].join("."))
        .collect())
}

/// Resolve the effective database-backed policy for a remote domain.
pub async fn federation_domain_policy<C>(db: &C, domain: &str) -> Result<FederationDomainPolicy>
where
    C: ConnectionTrait,
{
    let suffixes = federation_domain_suffixes(domain)?;
    let rules = federation_domain_block::Entity::find()
        .filter(federation_domain_block::Column::Domain.is_in(suffixes))
        .all(db)
        .await?;
    Ok(rules
        .into_iter()
        .fold(FederationDomainPolicy::default(), |mut policy, rule| {
            policy.severity = policy.severity.max(rule.severity);
            policy.reject_media |= rule.reject_media;
            policy.reject_reports |= rule.reject_reports;
            policy
        }))
}

/// Return domains hidden from public discovery by silence or suspension rules.
pub async fn hidden_federation_domains<C>(db: &C) -> Result<Vec<String>>
where
    C: ConnectionTrait,
{
    Ok(federation_domain_block::Entity::find()
        .filter(
            federation_domain_block::Column::Severity
                .is_in([DomainBlockSeverity::Silence, DomainBlockSeverity::Suspend]),
        )
        .all(db)
        .await?
        .into_iter()
        .map(|rule| rule.domain)
        .collect())
}

/// List domain rules in stable UUIDv7 cursor order.
pub async fn list_federation_domain_blocks(
    db: &impl ConnectionTrait,
    limit: u64,
    max_id: Option<Uuid>,
) -> Result<Vec<FederationDomainBlock>> {
    let mut query = federation_domain_block::Entity::find();
    if let Some(max_id) = max_id {
        query = query.filter(federation_domain_block::Column::Id.lt(max_id));
    }
    Ok(query
        .order_by_desc(federation_domain_block::Column::Id)
        .limit(limit.min(201))
        .all(db)
        .await?
        .into_iter()
        .map(federation_domain_block_from_model)
        .collect())
}

pub async fn find_federation_domain_block<C>(
    db: &C,
    id: Uuid,
) -> Result<Option<FederationDomainBlock>>
where
    C: ConnectionTrait,
{
    Ok(federation_domain_block::Entity::find_by_id(id)
        .one(db)
        .await?
        .map(federation_domain_block_from_model))
}

/// Create one rule after validating that the exact domain is not already present.
pub async fn create_federation_domain_block(
    txn: &DatabaseTransaction,
    input: NewFederationDomainBlock,
) -> Result<FederationDomainBlock> {
    let domain = normalize_federation_domain(&input.domain)?;
    if federation_domain_block::Entity::find()
        .filter(federation_domain_block::Column::Domain.eq(&domain))
        .one(txn)
        .await?
        .is_some()
    {
        return Err(RoostyError::InvalidInput(
            "domain is already blocked".to_owned(),
        ));
    }
    let now = OffsetDateTime::now_utc();
    let model = federation_domain_block::ActiveModel {
        id: Set(Uuid::now_v7()),
        domain: Set(domain),
        severity: Set(input.severity),
        reject_media: Set(input.reject_media),
        reject_reports: Set(input.reject_reports),
        private_comment: Set(input.private_comment),
        public_comment: Set(input.public_comment),
        obfuscate: Set(input.obfuscate),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(txn)
    .await?;
    Ok(federation_domain_block_from_model(model))
}

pub async fn update_federation_domain_block(
    txn: &DatabaseTransaction,
    id: Uuid,
    update: FederationDomainBlockUpdate,
) -> Result<FederationDomainBlock> {
    let model = federation_domain_block::Entity::find_by_id(id)
        .one(txn)
        .await?
        .ok_or_else(|| RoostyError::InvalidInput("domain block does not exist".to_owned()))?;
    let mut active = model.into_active_model();
    set_if_some(&mut active.severity, update.severity);
    set_if_some(&mut active.reject_media, update.reject_media);
    set_if_some(&mut active.reject_reports, update.reject_reports);
    set_if_some(&mut active.private_comment, update.private_comment);
    set_if_some(&mut active.public_comment, update.public_comment);
    set_if_some(&mut active.obfuscate, update.obfuscate);
    active.updated_at = Set(OffsetDateTime::now_utc());
    Ok(federation_domain_block_from_model(
        active.update(txn).await?,
    ))
}

pub async fn delete_federation_domain_block(
    txn: &DatabaseTransaction,
    id: Uuid,
) -> Result<Option<FederationDomainBlock>> {
    let Some(model) = federation_domain_block::Entity::find_by_id(id)
        .one(txn)
        .await?
    else {
        return Ok(None);
    };
    let block = federation_domain_block_from_model(model.clone());
    model.delete(txn).await?;
    Ok(Some(block))
}

/// Apply irreversible cache cleanup for an active suspended-domain rule.
pub async fn reconcile_federation_domain_block(txn: &DatabaseTransaction, id: Uuid) -> Result<u64> {
    let Some(block) = federation_domain_block::Entity::find_by_id(id)
        .one(txn)
        .await?
    else {
        return Ok(0);
    };
    if block.severity != DomainBlockSeverity::Suspend {
        return Ok(0);
    }
    let actors = remote_actor::Entity::find()
        .filter(
            remote_actor::Column::Domain
                .eq(&block.domain)
                .or(remote_actor::Column::Domain.ends_with(format!(".{}", block.domain))),
        )
        .all(txn)
        .await?;
    let now = OffsetDateTime::now_utc();
    for actor in &actors {
        purge_remote_actor_cache(txn, AccountId(actor.id), now).await?;
    }
    Ok(actors.len() as u64)
}

/// List local and cached-remote accounts for administrator tooling.
pub async fn list_admin_accounts(
    db: &impl ConnectionTrait,
    query: &str,
    origin: Option<&str>,
    limited: Option<bool>,
    suspended: Option<bool>,
    limit: u64,
    max_id: Option<Uuid>,
) -> Result<Vec<AdminAccount>> {
    let query = query.trim().to_lowercase();
    let fetch_limit = limit.min(101);
    let include_local = origin.is_none_or(|origin| origin == "local");
    let include_remote = origin.is_none_or(|origin| origin == "remote");

    // Fetch at most one page from each entity, then merge by the shared UUIDv7 cursor.
    // This keeps pagination typed and bounded to twice the requested page size.
    let mut accounts = Vec::new();
    if include_local {
        let mut select = local_account::Entity::find();
        if !query.is_empty() {
            let pattern = format!("%{query}%");
            select = select.filter(
                Condition::any()
                    .add(lower_contains(local_account::Column::Username, &pattern))
                    .add(lower_contains(local_account::Column::DisplayName, &pattern))
                    .add(lower_contains(local_account::Column::Email, &pattern)),
            );
        }
        if let Some(limited) = limited {
            select = select.filter(if limited {
                local_account::Column::LimitedAt.is_not_null()
            } else {
                local_account::Column::LimitedAt.is_null()
            });
        }
        if let Some(suspended) = suspended {
            select = select.filter(if suspended {
                local_account::Column::SuspendedAt.is_not_null()
            } else {
                local_account::Column::SuspendedAt.is_null()
            });
        }
        if let Some(max_id) = max_id {
            select = select.filter(local_account::Column::Id.lt(max_id));
        }
        accounts.extend(
            select
                .order_by_desc(local_account::Column::Id)
                .limit(fetch_limit)
                .all(db)
                .await?
                .into_iter()
                .map(|account| AdminAccount {
                    id: AccountId(account.id),
                    username: account.username,
                    domain: None,
                    email: Some(account.email),
                    display_name: account.display_name,
                    is_admin: account.is_admin,
                    limited: account.limited_at.is_some(),
                    suspended: account.suspended_at.is_some(),
                    data_purged_at: account.data_purged_at,
                    created_at: account.created_at,
                }),
        );
    }
    if include_remote {
        let mut select =
            remote_actor::Entity::find().filter(remote_actor::Column::DeletedAt.is_null());
        if !query.is_empty() {
            let pattern = format!("%{query}%");
            select = select.filter(
                Condition::any()
                    .add(lower_contains(remote_actor::Column::Username, &pattern))
                    .add(lower_contains(remote_actor::Column::DisplayName, &pattern))
                    .add(lower_contains(remote_actor::Column::Domain, &pattern)),
            );
        }
        if let Some(limited) = limited {
            select = select.filter(if limited {
                remote_actor::Column::LimitedAt.is_not_null()
            } else {
                remote_actor::Column::LimitedAt.is_null()
            });
        }
        if let Some(suspended) = suspended {
            select = select.filter(if suspended {
                remote_actor::Column::SuspendedAt.is_not_null()
            } else {
                remote_actor::Column::SuspendedAt.is_null()
            });
        }
        if let Some(max_id) = max_id {
            select = select.filter(remote_actor::Column::Id.lt(max_id));
        }
        accounts.extend(
            select
                .order_by_desc(remote_actor::Column::Id)
                .limit(fetch_limit)
                .all(db)
                .await?
                .into_iter()
                .map(|actor| AdminAccount {
                    id: AccountId(actor.id),
                    username: actor.username,
                    domain: Some(actor.domain),
                    email: None,
                    display_name: actor.display_name,
                    is_admin: false,
                    limited: actor.limited_at.is_some(),
                    suspended: actor.suspended_at.is_some(),
                    data_purged_at: actor.data_purged_at,
                    created_at: actor.profile_created_at.unwrap_or(actor.created_at),
                }),
        );
    }
    accounts.sort_unstable_by_key(|account| Reverse(account.id.0));
    accounts.truncate(fetch_limit as usize);
    Ok(accounts)
}

fn lower_contains<C>(column: C, pattern: &str) -> sea_orm::sea_query::SimpleExpr
where
    C: ColumnTrait,
{
    Expr::expr(Func::lower(Expr::col(column))).like(pattern)
}

/// Look up either kind of account for administrator detail and actions.
pub async fn find_admin_account_by_id(
    db: &impl ConnectionTrait,
    account_id: AccountId,
) -> Result<Option<AdminAccount>> {
    if let Some(account) = find_local_account_by_id(db, account_id).await? {
        return Ok(Some(AdminAccount {
            id: account.id,
            username: account.username,
            domain: None,
            email: Some(account.email),
            display_name: account.display_name,
            is_admin: account.is_admin,
            limited: account.limited_at.is_some(),
            suspended: account.suspended_at.is_some(),
            data_purged_at: account.data_purged_at,
            created_at: account.created_at,
        }));
    }
    Ok(find_remote_actor_by_id(db, account_id)
        .await?
        .filter(|actor| actor.deleted_at.is_none())
        .map(|actor| AdminAccount {
            id: actor.id,
            username: actor.username,
            domain: Some(actor.domain),
            email: None,
            display_name: actor.display_name,
            is_admin: false,
            limited: actor.limited_at.is_some(),
            suspended: actor.suspended_at.is_some(),
            data_purged_at: actor.data_purged_at,
            created_at: actor.profile_created_at.unwrap_or(actor.first_seen_at),
        }))
}

/// Count administrators that can still authenticate and recover the instance.
pub async fn count_active_admin_accounts<C>(db: &C) -> Result<u64>
where
    C: ConnectionTrait,
{
    Ok(local_account::Entity::find()
        .filter(local_account::Column::IsAdmin.eq(true))
        .filter(local_account::Column::SuspendedAt.is_null())
        .count(db)
        .await?)
}

#[derive(Clone, Copy, Debug, DeriveValueType, Display, EnumString, Eq, PartialEq)]
#[sea_orm(value_type = "String")]
#[strum(serialize_all = "snake_case")]
enum AccountSearchKind {
    Local,
    Remote,
}

#[derive(Clone, Copy, Debug, DeriveValueType, Display, EnumString, Eq, PartialEq)]
#[sea_orm(value_type = "String")]
#[strum(serialize_all = "snake_case")]
enum StatusSearchKind {
    Local,
    Remote,
}

/// List discoverable profiles with bounded counters and stable offset ordering.
pub async fn account_directory(
    db: &impl ConnectionTrait,
    options: AccountDirectoryOptions<'_>,
) -> Result<AccountDirectoryPage> {
    #[derive(Clone)]
    struct DirectoryRow {
        kind: AccountSearchKind,
        id: AccountId,
        followers_count: u64,
        following_count: u64,
        statuses_count: u64,
        last_status_at: Option<OffsetDateTime>,
    }

    let order = match options.order {
        AccountDirectoryOrder::Active => {
            "sort_status_at DESC NULLS LAST, sort_created_at DESC, id DESC"
        }
        AccountDirectoryOrder::New => "sort_created_at DESC, id DESC",
    };
    let sql = format!(
        r#"
        WITH candidates AS (
            SELECT 'local'::text AS account_kind, account.id,
                   account.last_status_at AS sort_status_at,
                   account.created_at AS sort_created_at
              FROM local_account account
             WHERE account.discoverable
               AND account.limited_at IS NULL
               AND account.suspended_at IS NULL
               AND ($1::uuid IS NULL OR (
                    account.id <> $1
                    AND NOT EXISTS (
                        SELECT 1 FROM local_account_block block
                         WHERE (block.account_id = $1 AND block.target_account_id = account.id)
                            OR (block.account_id = account.id AND block.target_account_id = $1)
                    )
                    AND NOT EXISTS (
                        SELECT 1 FROM local_account_mute mute
                         WHERE mute.account_id = $1
                           AND mute.target_account_id = account.id
                           AND (mute.expires_at IS NULL OR mute.expires_at > now())
                    )
               ))
            UNION ALL
            SELECT 'remote'::text, actor.id, actor.last_status_at,
                   coalesce(actor.profile_created_at, actor.created_at)
              FROM remote_actor actor
             WHERE NOT $2
               AND actor.discoverable IS TRUE
               AND actor.limited_at IS NULL
               AND actor.suspended_at IS NULL
               AND actor.deleted_at IS NULL
               AND actor.moved_to_remote_actor_id IS NULL
               AND NOT EXISTS (
                    SELECT 1
                    FROM jsonb_array_elements_text($3::jsonb) blocked(domain)
                    WHERE actor.domain = blocked.domain
                       OR actor.domain LIKE '%.' || blocked.domain
               )
               AND ($1::uuid IS NULL OR (
                    NOT EXISTS (
                        SELECT 1 FROM local_remote_account_block block
                         WHERE block.local_account_id = $1
                           AND block.remote_actor_id = actor.id
                    )
                    AND NOT EXISTS (
                        SELECT 1 FROM remote_local_account_block block
                         WHERE block.local_account_id = $1
                           AND block.remote_actor_id = actor.id
                    )
                    AND NOT EXISTS (
                        SELECT 1 FROM local_remote_account_mute mute
                         WHERE mute.local_account_id = $1
                           AND mute.remote_actor_id = actor.id
                           AND (mute.expires_at IS NULL OR mute.expires_at > now())
                    )
               ))
        ),
        selected AS (
            SELECT *
              FROM candidates
             ORDER BY {order}
             LIMIT $4 OFFSET $5
        )
        SELECT selected.account_kind, selected.id, selected.sort_status_at,
               CASE selected.account_kind
                 WHEN 'local' THEN
                   (SELECT count(*) FROM local_follow follow
                     WHERE follow.followed_account_id = selected.id)
                   + (SELECT count(*) FROM remote_follow follow
                       WHERE follow.local_account_id = selected.id
                         AND follow.state = 'accepted')
                 ELSE
                   (SELECT count(*) FROM remote_following follow
                     WHERE follow.remote_actor_id = selected.id
                       AND follow.state = 'accepted'
                       AND follow.deactivated_at IS NULL)
               END AS followers_count,
               CASE selected.account_kind
                 WHEN 'local' THEN
                   (SELECT count(*) FROM local_follow follow
                     WHERE follow.follower_account_id = selected.id)
                   + (SELECT count(*) FROM remote_following follow
                       WHERE follow.local_account_id = selected.id
                         AND follow.state = 'accepted'
                         AND follow.deactivated_at IS NULL)
                 ELSE
                   (SELECT count(*) FROM remote_follow follow
                     WHERE follow.remote_actor_id = selected.id
                       AND follow.state = 'accepted')
               END AS following_count,
               CASE selected.account_kind
                 WHEN 'local' THEN
                   (SELECT count(*) FROM local_status status
                     WHERE status.account_id = selected.id
                       AND status.deleted_at IS NULL)
                 ELSE
                   (SELECT count(*) FROM remote_status status
                     WHERE status.remote_actor_id = selected.id
                       AND status.deleted_at IS NULL)
               END AS statuses_count
          FROM selected
         ORDER BY {order}
        "#
    );
    let query_limit = options.limit.saturating_add(1).min(i64::MAX as u64) as i64;
    let rows = db
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            vec![
                options.viewer_account_id.map(|id| id.0).into(),
                options.local_only.into(),
                serde_json::to_string(options.blocked_remote_domains)
                    .map_err(|error| RoostyError::InvalidInput(error.to_string()))?
                    .into(),
                query_limit.into(),
                (options.offset.min(i64::MAX as u64) as i64).into(),
            ],
        ))
        .await?;
    let mut rows = rows
        .into_iter()
        .map(|row| {
            Ok(DirectoryRow {
                kind: row.try_get("", "account_kind")?,
                id: AccountId(row.try_get("", "id")?),
                followers_count: u64::try_from(row.try_get::<i64>("", "followers_count")?)
                    .map_err(|_| DbErr::Type("negative directory follower count".to_owned()))?,
                following_count: u64::try_from(row.try_get::<i64>("", "following_count")?)
                    .map_err(|_| DbErr::Type("negative directory following count".to_owned()))?,
                statuses_count: u64::try_from(row.try_get::<i64>("", "statuses_count")?)
                    .map_err(|_| DbErr::Type("negative directory status count".to_owned()))?,
                last_status_at: row.try_get("", "sort_status_at")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let has_more = rows.len() > options.limit as usize;
    rows.truncate(options.limit as usize);

    let local_ids = rows
        .iter()
        .filter(|row| row.kind == AccountSearchKind::Local)
        .map(|row| row.id)
        .collect();
    let remote_ids = rows
        .iter()
        .filter(|row| row.kind == AccountSearchKind::Remote)
        .map(|row| row.id)
        .collect();
    let local_accounts = local_accounts_by_id(db, local_ids).await?;
    let remote_actors = remote_actors_by_id(db, remote_ids).await?;
    let mut local_accounts = local_accounts
        .into_iter()
        .map(|account| (account.id, account))
        .collect::<HashMap<_, _>>();
    let mut remote_actors = remote_actors
        .into_iter()
        .map(|actor| (actor.id, actor))
        .collect::<HashMap<_, _>>();
    let items = rows
        .into_iter()
        .filter_map(|row| {
            let account = match row.kind {
                AccountSearchKind::Local => {
                    AccountSearchResult::Local(local_accounts.remove(&row.id)?)
                }
                AccountSearchKind::Remote => {
                    AccountSearchResult::Remote(remote_actors.remove(&row.id)?)
                }
            };
            Some(DirectoryAccount {
                account,
                followers_count: row.followers_count,
                following_count: row.following_count,
                statuses_count: row.statuses_count,
                last_status_at: row.last_status_at,
            })
        })
        .collect();
    Ok(AccountDirectoryPage { items, has_more })
}

/// Return ranked follow suggestions from accounts already known to this instance.
pub async fn account_suggestions(
    db: &impl ConnectionTrait,
    options: AccountSuggestionOptions<'_>,
) -> Result<Vec<AccountSuggestion>> {
    #[derive(Clone)]
    struct SuggestionRow {
        kind: AccountSearchKind,
        id: AccountId,
        followers_count: u64,
        following_count: u64,
        statuses_count: u64,
        last_status_at: Option<OffsetDateTime>,
    }

    let rows = db
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
            WITH local_candidates AS (
                SELECT 'local'::text AS account_kind, account.id,
                       account.last_status_at AS sort_status_at,
                       account.created_at AS sort_created_at,
                       (SELECT count(*)
                          FROM local_follow follow
                          JOIN local_account follower
                            ON follower.id = follow.follower_account_id
                           AND follower.suspended_at IS NULL
                         WHERE follow.followed_account_id = account.id) AS local_followers_count
                  FROM local_account account
                 WHERE account.id <> $1
                   AND account.discoverable
                   AND account.limited_at IS NULL
                   AND account.suspended_at IS NULL
                   AND NOT EXISTS (
                       SELECT 1 FROM local_follow follow
                        WHERE follow.follower_account_id = $1
                          AND follow.followed_account_id = account.id
                   )
                   AND NOT EXISTS (
                       SELECT 1 FROM local_account_suggestion_dismissal dismissal
                        WHERE dismissal.account_id = $1
                          AND dismissal.target_account_id = account.id
                   )
                   AND NOT EXISTS (
                       SELECT 1 FROM local_account_block block
                        WHERE (block.account_id = $1 AND block.target_account_id = account.id)
                           OR (block.account_id = account.id AND block.target_account_id = $1)
                   )
                   AND NOT EXISTS (
                       SELECT 1 FROM local_account_mute mute
                        WHERE mute.account_id = $1
                          AND mute.target_account_id = account.id
                          AND (mute.expires_at IS NULL OR mute.expires_at > now())
                   )
                 ORDER BY account.last_status_at DESC NULLS LAST, account.created_at DESC, account.id DESC
                 LIMIT $5
            ),
            remote_candidates AS (
                SELECT 'remote'::text, actor.id, actor.last_status_at,
                       coalesce(actor.profile_created_at, actor.created_at),
                       (SELECT count(*)
                          FROM remote_following follow
                          JOIN local_account follower
                            ON follower.id = follow.local_account_id
                           AND follower.suspended_at IS NULL
                         WHERE follow.remote_actor_id = actor.id
                           AND follow.state = 'accepted'
                           AND follow.deactivated_at IS NULL)
                  FROM remote_actor actor
                 WHERE actor.discoverable IS TRUE
                   AND actor.limited_at IS NULL
                   AND actor.suspended_at IS NULL
                   AND actor.deleted_at IS NULL
                   AND actor.moved_to_remote_actor_id IS NULL
                   AND NOT EXISTS (
                       SELECT 1 FROM remote_following follow
                        WHERE follow.local_account_id = $1
                          AND follow.remote_actor_id = actor.id
                          AND follow.deactivated_at IS NULL
                   )
                   AND NOT EXISTS (
                       SELECT 1 FROM local_account_suggestion_dismissal dismissal
                        WHERE dismissal.account_id = $1
                          AND dismissal.target_remote_actor_id = actor.id
                   )
                   AND NOT EXISTS (
                       SELECT 1
                         FROM jsonb_array_elements_text($2::jsonb) blocked(domain)
                        WHERE actor.domain = blocked.domain
                           OR actor.domain LIKE '%.' || blocked.domain
                   )
                   AND NOT EXISTS (
                       SELECT 1 FROM local_remote_account_block block
                        WHERE block.local_account_id = $1
                          AND block.remote_actor_id = actor.id
                   )
                   AND NOT EXISTS (
                       SELECT 1 FROM remote_local_account_block block
                        WHERE block.local_account_id = $1
                          AND block.remote_actor_id = actor.id
                   )
                   AND NOT EXISTS (
                       SELECT 1 FROM local_remote_account_mute mute
                        WHERE mute.local_account_id = $1
                          AND mute.remote_actor_id = actor.id
                          AND (mute.expires_at IS NULL OR mute.expires_at > now())
                   )
                 ORDER BY actor.last_status_at DESC NULLS LAST,
                          coalesce(actor.profile_created_at, actor.created_at) DESC, actor.id DESC
                 LIMIT $5
            ),
            candidates AS (
                SELECT * FROM local_candidates
                UNION ALL
                SELECT * FROM remote_candidates
            ),
            selected AS (
                SELECT * FROM candidates
                 ORDER BY local_followers_count DESC, sort_status_at DESC NULLS LAST,
                          sort_created_at DESC, id DESC
                 LIMIT $3 OFFSET $4
            )
            SELECT selected.account_kind, selected.id, selected.sort_status_at,
                   CASE selected.account_kind
                     WHEN 'local' THEN
                       (SELECT count(*) FROM local_follow follow
                         WHERE follow.followed_account_id = selected.id)
                       + (SELECT count(*) FROM remote_follow follow
                           WHERE follow.local_account_id = selected.id
                             AND follow.state = 'accepted')
                     ELSE
                       (SELECT count(*) FROM remote_following follow
                         WHERE follow.remote_actor_id = selected.id
                           AND follow.state = 'accepted'
                           AND follow.deactivated_at IS NULL)
                   END AS followers_count,
                   CASE selected.account_kind
                     WHEN 'local' THEN
                       (SELECT count(*) FROM local_follow follow
                         WHERE follow.follower_account_id = selected.id)
                       + (SELECT count(*) FROM remote_following follow
                           WHERE follow.local_account_id = selected.id
                             AND follow.state = 'accepted'
                             AND follow.deactivated_at IS NULL)
                     ELSE
                       (SELECT count(*) FROM remote_follow follow
                         WHERE follow.remote_actor_id = selected.id
                           AND follow.state = 'accepted')
                   END AS following_count,
                   CASE selected.account_kind
                     WHEN 'local' THEN
                       (SELECT count(*) FROM local_status status
                         WHERE status.account_id = selected.id AND status.deleted_at IS NULL)
                     ELSE
                       (SELECT count(*) FROM remote_status status
                         WHERE status.remote_actor_id = selected.id AND status.deleted_at IS NULL)
                   END AS statuses_count
              FROM selected
             ORDER BY selected.local_followers_count DESC,
                      selected.sort_status_at DESC NULLS LAST,
                      selected.sort_created_at DESC, selected.id DESC
            "#,
            vec![
                options.viewer_account_id.0.into(),
                serde_json::to_string(options.blocked_remote_domains)
                    .map_err(|error| RoostyError::InvalidInput(error.to_string()))?
                    .into(),
                (options.limit.min(i64::MAX as u64) as i64).into(),
                (options.offset.min(i64::MAX as u64) as i64).into(),
                1_000_i64.into(),
            ],
        ))
        .await?;
    let rows = rows
        .into_iter()
        .map(|row| {
            Ok(SuggestionRow {
                kind: row.try_get("", "account_kind")?,
                id: AccountId(row.try_get("", "id")?),
                followers_count: u64::try_from(row.try_get::<i64>("", "followers_count")?)
                    .map_err(|_| DbErr::Type("negative suggestion follower count".to_owned()))?,
                following_count: u64::try_from(row.try_get::<i64>("", "following_count")?)
                    .map_err(|_| DbErr::Type("negative suggestion following count".to_owned()))?,
                statuses_count: u64::try_from(row.try_get::<i64>("", "statuses_count")?)
                    .map_err(|_| DbErr::Type("negative suggestion status count".to_owned()))?,
                last_status_at: row.try_get("", "sort_status_at")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let local_ids = rows
        .iter()
        .filter(|row| row.kind == AccountSearchKind::Local)
        .map(|row| row.id)
        .collect();
    let remote_ids = rows
        .iter()
        .filter(|row| row.kind == AccountSearchKind::Remote)
        .map(|row| row.id)
        .collect();
    let mut local_accounts = local_accounts_by_id(db, local_ids)
        .await?
        .into_iter()
        .map(|account| (account.id, account))
        .collect::<HashMap<_, _>>();
    let mut remote_actors = remote_actors_by_id(db, remote_ids)
        .await?
        .into_iter()
        .map(|actor| (actor.id, actor))
        .collect::<HashMap<_, _>>();
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let account = match row.kind {
                AccountSearchKind::Local => {
                    AccountSearchResult::Local(local_accounts.remove(&row.id)?)
                }
                AccountSearchKind::Remote => {
                    AccountSearchResult::Remote(remote_actors.remove(&row.id)?)
                }
            };
            Some(DirectoryAccount {
                account,
                followers_count: row.followers_count,
                following_count: row.following_count,
                statuses_count: row.statuses_count,
                last_status_at: row.last_status_at,
            })
        })
        .collect())
}

/// Idempotently suppress a local or cached-remote account from future suggestions.
pub async fn dismiss_account_suggestion(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    target_id: AccountId,
) -> Result<()> {
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"
        INSERT INTO local_account_suggestion_dismissal (
            id, account_id, target_account_id, target_remote_actor_id, created_at
        )
        SELECT $1, $2, target.local_id, target.remote_id, now()
          FROM (
            SELECT local_target.id AS local_id,
                   CASE WHEN local_target.id IS NULL THEN remote_target.id END AS remote_id
              FROM (SELECT $3::uuid AS id) input
              LEFT JOIN local_account local_target ON local_target.id = input.id
              LEFT JOIN remote_actor remote_target ON remote_target.id = input.id
             WHERE local_target.id IS NOT NULL OR remote_target.id IS NOT NULL
          ) target
        ON CONFLICT DO NOTHING
        "#,
        vec![
            Uuid::now_v7().into(),
            account_id.0.into(),
            target_id.0.into(),
        ],
    ))
    .await?;
    Ok(())
}

/// Inputs controlling unified local and cached-remote account search.
pub struct AccountSearchOptions<'a> {
    /// Authenticated viewer, or a sentinel ID for public v2 search.
    pub viewer_account_id: AccountId,
    /// Normalized account search text.
    pub query: &'a str,
    /// Host used to rank exact local account addresses.
    pub local_domain: &'a str,
    /// Restrict results to accepted follows by the viewer.
    pub following_only: bool,
    /// Include active actors from the federation cache.
    pub include_remote: bool,
    /// Permit every domain except those explicitly blocked.
    pub allow_all_remote_domains: bool,
    /// Exact remote domains allowed by operator policy.
    pub allowed_remote_domains: &'a [String],
    /// Exact remote domains denied by operator policy.
    pub blocked_remote_domains: &'a [String],
    /// Maximum number of combined results.
    pub limit: u64,
    /// Number of combined ranked results to skip.
    pub offset: u64,
}

/// Public or unlisted Note cached from a remote ActivityPub actor.
#[derive(Clone, Debug)]
pub struct RemoteStatus {
    /// UUID-backed internal status identifier.
    pub id: StatusId,
    /// Canonical ActivityPub object ID.
    pub activitypub_id: String,
    /// Cached author.
    pub remote_actor_id: AccountId,
    /// Sanitized-at-render-time remote HTML content.
    pub content: String,
    /// Mastodon-compatible public or unlisted visibility.
    pub visibility: StatusVisibility,
    /// Remote publication timestamp.
    pub published_at: OffsetDateTime,
    /// Remote edit timestamp.
    pub updated_at: OffsetDateTime,
    /// Soft-delete timestamp, if a signed Delete was received.
    pub deleted_at: Option<OffsetDateTime>,
    /// Canonical remote or local object URL named by `inReplyTo`.
    pub in_reply_to: Option<String>,
    /// Resolved local parent, when this instance owns the referenced object.
    pub in_reply_to_local_status_id: Option<StatusId>,
    /// Resolved cached remote parent, when available.
    pub in_reply_to_remote_status_id: Option<StatusId>,
    /// Direct-message conversation containing this cached Note, when applicable.
    pub conversation_id: Option<Uuid>,
    /// Original Note object retained for future projection fields.
    pub object: JsonValue,
    /// Automatic quote audiences advertised through FEP-044f.
    pub quote_automatic_policy: Vec<String>,
    /// Manual quote audiences advertised through FEP-044f.
    pub quote_manual_policy: Vec<String>,
}

/// A status participating in a cached Mastodon thread context.
#[derive(Clone, Debug)]
pub enum StatusContextItem {
    /// A status authored on this instance.
    Local(LocalStatus),
    /// A status received and retained in the federation cache.
    Remote(RemoteStatus),
}

/// Bounded, viewer-scoped inputs for PostgreSQL status search.
pub struct StatusSearchOptions<'a> {
    pub viewer_account_id: AccountId,
    pub query: &'a str,
    pub account_id: Option<AccountId>,
    pub include_remote: bool,
    pub blocked_remote_domains: &'a [String],
    pub limit: u64,
    pub offset: u64,
    pub min_id: Option<StatusId>,
    pub max_id: Option<StatusId>,
}

/// One ordered status-search page with opaque UUID cursor metadata.
pub struct StatusSearchPage {
    pub items: Vec<StatusContextItem>,
    pub first_cursor: Option<Uuid>,
    pub last_cursor: Option<Uuid>,
    pub has_more: bool,
}

impl StatusContextItem {
    /// Return the UUID-backed API identifier shared by both status kinds.
    pub fn id(&self) -> StatusId {
        match self {
            Self::Local(status) => status.id,
            Self::Remote(status) => status.id,
        }
    }
}

/// Typed parent identity used when loading direct replies across status tables.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum StatusContextParent {
    Local(StatusId),
    Remote(StatusId),
}

/// Fields accepted when caching a verified remote Note.
#[derive(Clone, Debug)]
pub struct NewRemoteStatus {
    /// Canonical ActivityPub object ID.
    pub activitypub_id: String,
    /// Verified author.
    pub remote_actor_id: AccountId,
    /// Remote HTML content.
    pub content: String,
    /// Public or unlisted visibility.
    pub visibility: StatusVisibility,
    /// Remote publication timestamp.
    pub published_at: OffsetDateTime,
    /// Remote edit timestamp.
    pub updated_at: OffsetDateTime,
    /// Optional canonical object URL named by the remote Note's `inReplyTo`.
    pub in_reply_to: Option<String>,
    /// Locally resolved parent, if the reference belongs to this instance.
    pub in_reply_to_local_status_id: Option<StatusId>,
    /// Cached remote parent, if it has already been resolved.
    pub in_reply_to_remote_status_id: Option<StatusId>,
    /// Original Note object.
    pub object: JsonValue,
    /// Validated, normalized hashtag names derived from the Note.
    pub tag_names: Vec<String>,
    /// FEP-044f automatic-approval audience IRIs.
    pub quote_automatic_policy: Vec<String>,
    /// FEP-044f manual-approval audience IRIs.
    pub quote_manual_policy: Vec<String>,
}

/// A local actor's relationship to a remote actor.
#[derive(Clone, Debug)]
pub struct RemoteFollowing {
    /// Local follower.
    pub local_account_id: AccountId,
    /// Remote followed actor.
    pub remote_actor_id: AccountId,
    /// Canonical outbound Follow activity ID.
    pub activity_id: String,
    /// `pending` or `accepted`.
    pub state: RemoteFollowState,
    /// Whether boosts by the followed actor should appear in the home timeline.
    pub show_reblogs: bool,
    /// Whether new posts by the followed actor should create notifications.
    pub notify: bool,
}

/// A local or cached remote account returned from a follow collection.
#[derive(Clone, Debug)]
pub enum FollowCollectionAccount {
    /// Local account projection.
    Local(LocalAccount),
    /// Cached remote actor projection.
    Remote(RemoteActor),
}

/// One cursor-addressable account in a mixed follow collection.
#[derive(Clone, Debug)]
pub struct FollowCollectionEntry {
    /// Relationship row identifier used as the collection cursor.
    pub id: Uuid,
    /// Account represented by the relationship.
    pub account: FollowCollectionAccount,
}

/// Insert a pending local-to-remote follow relationship.
pub async fn create_remote_following(
    db: &DbConnection,
    local_account_id: AccountId,
    remote_actor_id: AccountId,
    activity_id: &str,
    show_reblogs: bool,
    notify: bool,
) -> Result<RemoteFollowing> {
    let row = remote_following::ActiveModel {
        id: Set(Uuid::now_v7()),
        local_account_id: Set(local_account_id.0),
        remote_actor_id: Set(remote_actor_id.0),
        activity_id: Set(activity_id.to_owned()),
        state: Set(RemoteFollowState::Pending),
        show_reblogs: Set(show_reblogs),
        notify: Set(notify),
        ..Default::default()
    }
    .insert(db)
    .await?;
    Ok(remote_following_from_model(row))
}

/// Create a pending local-to-remote follow and its durable Follow job.
///
/// The caller owns and commits `txn`, so the relationship and job become
/// visible together with any surrounding handler work.
pub async fn create_remote_following_with_job(
    txn: &sea_orm::DatabaseTransaction,
    local_account_id: AccountId,
    remote_actor_id: AccountId,
    activity_id: &str,
    show_reblogs: bool,
    notify: bool,
    job: NewJob,
) -> Result<RemoteFollowing> {
    lock_local_remote_relation(txn, local_account_id, remote_actor_id).await?;
    if local_remote_accounts_are_blocked(txn, local_account_id, remote_actor_id).await? {
        return Err(AccountRelationshipError::FollowBlocked.into());
    }
    let existing = remote_following::Entity::find()
        .filter(remote_following::Column::LocalAccountId.eq(local_account_id.0))
        .filter(remote_following::Column::RemoteActorId.eq(remote_actor_id.0))
        .one(txn)
        .await?;
    let row = match existing {
        Some(model) if model.deactivated_at.is_none() => {
            let mut active = model.into_active_model();
            active.show_reblogs = Set(show_reblogs);
            active.notify = Set(notify);
            active.updated_at = Set(OffsetDateTime::now_utc());
            active.update(txn).await?
        }
        Some(model) => {
            let mut active = model.into_active_model();
            active.activity_id = Set(activity_id.to_owned());
            active.state = Set(RemoteFollowState::Pending);
            active.show_reblogs = Set(show_reblogs);
            active.notify = Set(notify);
            active.deactivated_at = Set(None);
            active.updated_at = Set(OffsetDateTime::now_utc());
            let row = active.update(txn).await?;
            enqueue_job_in_transaction(txn, job).await?;
            row
        }
        None => {
            let row = remote_following::ActiveModel {
                id: Set(Uuid::now_v7()),
                local_account_id: Set(local_account_id.0),
                remote_actor_id: Set(remote_actor_id.0),
                activity_id: Set(activity_id.to_owned()),
                state: Set(RemoteFollowState::Pending),
                show_reblogs: Set(show_reblogs),
                notify: Set(notify),
                ..Default::default()
            }
            .insert(txn)
            .await?;
            enqueue_job_in_transaction(txn, job).await?;
            row
        }
    };
    Ok(remote_following_from_model(row))
}

/// Find one local-to-remote follow relationship.
pub async fn find_remote_following(
    db: &impl ConnectionTrait,
    local_account_id: AccountId,
    remote_actor_id: AccountId,
) -> Result<Option<RemoteFollowing>> {
    Ok(remote_following::Entity::find()
        .filter(remote_following::Column::LocalAccountId.eq(local_account_id.0))
        .filter(remote_following::Column::RemoteActorId.eq(remote_actor_id.0))
        .filter(remote_following::Column::DeactivatedAt.is_null())
        .one(db)
        .await?
        .map(remote_following_from_model))
}

/// List local accounts whose accepted remote follow targets the supplied actor.
pub async fn accepted_local_followers_of_remote_actor(
    db: &impl ConnectionTrait,
    remote_actor_id: AccountId,
) -> Result<Vec<AccountId>> {
    let follows = remote_following::Entity::find()
        .filter(remote_following::Column::RemoteActorId.eq(remote_actor_id.0))
        .filter(remote_following::Column::State.eq(RemoteFollowState::Accepted))
        .filter(remote_following::Column::DeactivatedAt.is_null())
        .all(db)
        .await?;
    let mut accounts = Vec::with_capacity(follows.len());
    for follow in follows {
        let local = AccountId(follow.local_account_id);
        if !local_remote_accounts_are_blocked(db, local, remote_actor_id).await? {
            accounts.push(local);
        }
    }
    Ok(accounts)
}

/// List accepted local followers that opted into boosts by the remote actor.
pub async fn accepted_local_reblog_followers_of_remote_actor(
    db: &impl ConnectionTrait,
    remote_actor_id: AccountId,
) -> Result<Vec<AccountId>> {
    accepted_local_followers_of_remote_actor_with(db, remote_actor_id, |follow| follow.show_reblogs)
        .await
}

/// List accepted local followers that opted into new-post notifications.
pub async fn accepted_local_notified_followers_of_remote_actor(
    db: &impl ConnectionTrait,
    remote_actor_id: AccountId,
) -> Result<Vec<AccountId>> {
    accepted_local_followers_of_remote_actor_with(db, remote_actor_id, |follow| follow.notify).await
}

async fn accepted_local_followers_of_remote_actor_with(
    db: &impl ConnectionTrait,
    remote_actor_id: AccountId,
    include: impl Fn(&remote_following::Model) -> bool,
) -> Result<Vec<AccountId>> {
    let follows = remote_following::Entity::find()
        .filter(remote_following::Column::RemoteActorId.eq(remote_actor_id.0))
        .filter(remote_following::Column::State.eq(RemoteFollowState::Accepted))
        .filter(remote_following::Column::DeactivatedAt.is_null())
        .all(db)
        .await?;
    let mut accounts = Vec::with_capacity(follows.len());
    for follow in follows.iter().filter(|follow| include(follow)) {
        let local = AccountId(follow.local_account_id);
        if !local_remote_accounts_are_blocked(db, local, remote_actor_id).await? {
            accounts.push(local);
        }
    }
    Ok(accounts)
}

/// Return a page of local and remote accounts following one local account.
pub async fn followers_for_local_account(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    limit: u64,
    cursor: CollectionCursor,
) -> Result<CollectionPage<FollowCollectionEntry>> {
    follow_collection_page(
        db,
        local_follow::Entity::find()
            .filter(local_follow::Column::FollowedAccountId.eq(account_id.0)),
        remote_follow::Entity::find()
            .filter(remote_follow::Column::LocalAccountId.eq(account_id.0))
            .filter(remote_follow::Column::State.eq(RemoteFollowState::Accepted)),
        limit,
        cursor,
        |follow| (follow.id, AccountId(follow.follower_account_id)),
        |follow| (follow.id, AccountId(follow.remote_actor_id)),
    )
    .await
}

/// Return a page of local and accepted remote accounts followed by one local account.
pub async fn following_for_local_account(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    limit: u64,
    cursor: CollectionCursor,
) -> Result<CollectionPage<FollowCollectionEntry>> {
    follow_collection_page(
        db,
        local_follow::Entity::find()
            .filter(local_follow::Column::FollowerAccountId.eq(account_id.0)),
        remote_following::Entity::find()
            .filter(remote_following::Column::LocalAccountId.eq(account_id.0))
            .filter(remote_following::Column::State.eq(RemoteFollowState::Accepted)),
        limit,
        cursor,
        |follow| (follow.id, AccountId(follow.followed_account_id)),
        |follow| (follow.id, AccountId(follow.remote_actor_id)),
    )
    .await
}

/// Merge UUIDv7-ordered local and remote relationship rows into one cursor page.
async fn follow_collection_page<L, R, FL, FR>(
    db: &impl ConnectionTrait,
    local: Select<L>,
    remote: Select<R>,
    limit: u64,
    cursor: CollectionCursor,
    local_id: FL,
    remote_id: FR,
) -> Result<CollectionPage<FollowCollectionEntry>>
where
    L: EntityTrait,
    R: EntityTrait,
    L::Model: Clone,
    R::Model: Clone,
    FL: Fn(L::Model) -> (Uuid, AccountId),
    FR: Fn(R::Model) -> (Uuid, AccountId),
{
    let local = local.all(db).await?;
    let remote = remote.all(db).await?;
    let mut entries = Vec::new();
    for follow in local {
        let (id, account_id) = local_id(follow);
        if collection_cursor_matches(id, cursor)
            && let Some(account) = find_local_account_by_id(db, account_id).await?
        {
            entries.push(FollowCollectionEntry {
                id,
                account: FollowCollectionAccount::Local(account),
            });
        }
    }
    for follow in remote {
        let (id, actor_id) = remote_id(follow);
        if collection_cursor_matches(id, cursor)
            && let Some(actor) = find_remote_actor_by_id(db, actor_id).await?
        {
            entries.push(FollowCollectionEntry {
                id,
                account: FollowCollectionAccount::Remote(actor),
            });
        }
    }
    entries.sort_by_key(|entry| Reverse(entry.id));
    let (items, has_more) = trim_to_page(entries, limit);
    Ok(CollectionPage {
        first_cursor: items.first().map(|entry| entry.id),
        last_cursor: items.last().map(|entry| entry.id),
        items,
        has_more,
    })
}

/// Return locally known accounts following one cached remote actor.
pub async fn followers_for_remote_account(
    db: &impl ConnectionTrait,
    remote_actor_id: AccountId,
    limit: u64,
    cursor: CollectionCursor,
) -> Result<CollectionPage<FollowCollectionEntry>> {
    let rows = remote_following::Entity::find()
        .filter(remote_following::Column::RemoteActorId.eq(remote_actor_id.0))
        .filter(remote_following::Column::State.eq(RemoteFollowState::Accepted))
        .filter(remote_following::Column::DeactivatedAt.is_null())
        .apply_collection_cursor(cursor)
        .order_by_desc(remote_following::Column::Id)
        .limit(page_query_limit(limit))
        .all(db)
        .await?;
    let (rows, has_more) = trim_to_page(rows, limit);
    let relationships = rows
        .into_iter()
        .map(|follow| (follow.id, AccountId(follow.local_account_id)))
        .collect();
    local_relationship_collection_page(db, relationships, has_more).await
}

/// Return locally known accounts followed by one cached remote actor.
pub async fn following_for_remote_account(
    db: &impl ConnectionTrait,
    remote_actor_id: AccountId,
    limit: u64,
    cursor: CollectionCursor,
) -> Result<CollectionPage<FollowCollectionEntry>> {
    let rows = remote_follow::Entity::find()
        .filter(remote_follow::Column::RemoteActorId.eq(remote_actor_id.0))
        .filter(remote_follow::Column::State.eq(RemoteFollowState::Accepted))
        .apply_collection_cursor(cursor)
        .order_by_desc(remote_follow::Column::Id)
        .limit(page_query_limit(limit))
        .all(db)
        .await?;
    let (rows, has_more) = trim_to_page(rows, limit);
    let relationships = rows
        .into_iter()
        .map(|follow| (follow.id, AccountId(follow.local_account_id)))
        .collect();
    local_relationship_collection_page(db, relationships, has_more).await
}

/// Project ordered relationship rows into local account collection entries.
async fn local_relationship_collection_page(
    db: &impl ConnectionTrait,
    relationships: Vec<(Uuid, AccountId)>,
    has_more: bool,
) -> Result<CollectionPage<FollowCollectionEntry>> {
    let first_cursor = relationships.first().map(|(id, _)| *id);
    let last_cursor = relationships.last().map(|(id, _)| *id);
    let account_ids = relationships
        .iter()
        .map(|(_, account_id)| *account_id)
        .collect();
    let mut accounts = local_accounts_by_id(db, account_ids)
        .await?
        .into_iter()
        .map(|account| (account.id, account))
        .collect::<HashMap<_, _>>();
    let items = relationships
        .into_iter()
        .filter_map(|(id, account_id)| {
            accounts
                .remove(&account_id)
                .map(|account| FollowCollectionEntry {
                    id,
                    account: FollowCollectionAccount::Local(account),
                })
        })
        .collect();

    Ok(CollectionPage {
        items,
        first_cursor,
        last_cursor,
        has_more,
    })
}

fn collection_cursor_matches(id: Uuid, cursor: CollectionCursor) -> bool {
    cursor.max_id.is_none_or(|max_id| id < max_id)
        && cursor.since_id.is_none_or(|since_id| id > since_id)
        && cursor.min_id.is_none_or(|min_id| id > min_id)
}

/// Count accepted remote actors followed by this local account.
pub async fn count_remote_following(
    db: &impl ConnectionTrait,
    account_id: AccountId,
) -> Result<u64> {
    Ok(remote_following::Entity::find()
        .filter(remote_following::Column::LocalAccountId.eq(account_id.0))
        .filter(remote_following::Column::State.eq(RemoteFollowState::Accepted))
        .filter(remote_following::Column::DeactivatedAt.is_null())
        .count(db)
        .await?)
}

/// Count accepted local accounts following one cached remote actor.
pub async fn count_remote_actor_followers_known_locally(
    db: &impl ConnectionTrait,
    remote_actor_id: AccountId,
) -> Result<u64> {
    Ok(remote_following::Entity::find()
        .filter(remote_following::Column::RemoteActorId.eq(remote_actor_id.0))
        .filter(remote_following::Column::State.eq(RemoteFollowState::Accepted))
        .filter(remote_following::Column::DeactivatedAt.is_null())
        .count(db)
        .await?)
}

/// Count accepted local accounts followed by one cached remote actor.
pub async fn count_remote_actor_following_known_locally(
    db: &impl ConnectionTrait,
    remote_actor_id: AccountId,
) -> Result<u64> {
    Ok(remote_follow::Entity::find()
        .filter(remote_follow::Column::RemoteActorId.eq(remote_actor_id.0))
        .filter(remote_follow::Column::State.eq(RemoteFollowState::Accepted))
        .count(db)
        .await?)
}

/// Tombstone a remote actor and hide its cached public activity without purging audit data.
pub async fn process_remote_actor_delete(
    txn: &DatabaseTransaction,
    remote_actor_id: AccountId,
) -> Result<Option<RemoteDeleteRepair>> {
    let subscribers = accepted_local_followers_of_remote_actor(txn, remote_actor_id).await?;
    let now = OffsetDateTime::now_utc();
    let Some(actor) = remote_actor::Entity::find_by_id(remote_actor_id.0)
        .one(txn)
        .await?
    else {
        return Ok(None);
    };
    let mut actor = actor.into_active_model();
    actor.deleted_at = Set(Some(now));
    actor.updated_at = Set(now);
    actor.update(txn).await?;
    let statuses = remote_status::Entity::find()
        .filter(remote_status::Column::RemoteActorId.eq(remote_actor_id.0))
        .filter(remote_status::Column::DeletedAt.is_null())
        .all(txn)
        .await?;
    let mut repair = RemoteDeleteRepair::default();
    for status in statuses {
        let status_repair = repair_one_remote_status_delete(txn, status, false).await?;
        repair.projections.extend(status_repair.projections);
        repair
            .conversation_refreshes
            .extend(status_repair.conversation_refreshes);
        repair.deleted_status_count += status_repair.deleted_status_count;
    }
    refresh_remote_actor_last_status_at(txn, remote_actor_id).await?;

    let actor_reblogs = remote_status_reblog::Entity::find()
        .filter(remote_status_reblog::Column::RemoteActorId.eq(remote_actor_id.0))
        .all(txn)
        .await?;
    repair
        .projections
        .extend(actor_reblogs.iter().map(|reblog| DeleteStreamProjection {
            status_id: reblog.id.to_string(),
            actor_id: remote_actor_id,
            home_recipient_ids: subscribers.clone(),
            direct_recipient_ids: Vec::new(),
            visibility: StatusVisibility::Direct,
            status_origin: StreamingStatusOrigin::Remote,
            has_media: false,
        }));
    mark_remote_actor_interactions_dirty(txn, remote_actor_id).await?;
    remote_status_reblog::Entity::delete_many()
        .filter(remote_status_reblog::Column::RemoteActorId.eq(remote_actor_id.0))
        .exec(txn)
        .await?;
    remote_status_favourite::Entity::delete_many()
        .filter(remote_status_favourite::Column::RemoteActorId.eq(remote_actor_id.0))
        .exec(txn)
        .await?;
    local_notification::Entity::delete_many()
        .filter(local_notification::Column::RemoteActorId.eq(remote_actor_id.0))
        .exec(txn)
        .await?;
    remote_follow::Entity::delete_many()
        .filter(remote_follow::Column::RemoteActorId.eq(remote_actor_id.0))
        .exec(txn)
        .await?;
    remote_following::Entity::delete_many()
        .filter(remote_following::Column::RemoteActorId.eq(remote_actor_id.0))
        .exec(txn)
        .await?;
    txn.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "UPDATE job SET completed_at = $2, locked_at = NULL, locked_by = NULL, claim_id = NULL, last_error = 'remote actor deleted' WHERE completed_at IS NULL AND payload->>'remote_actor_id' = $1",
        vec![remote_actor_id.0.to_string().into(), now.into()],
    ))
    .await?;
    let mut consolidated = Vec::<DirectConversationRefresh>::new();
    for refresh in repair.conversation_refreshes.drain(..) {
        if let Some(existing) = consolidated
            .iter_mut()
            .find(|existing| existing.conversation_id == refresh.conversation_id)
        {
            existing
                .updated_account_ids
                .extend(refresh.updated_account_ids);
            existing
                .removed_account_ids
                .extend(refresh.removed_account_ids);
            existing.updated_account_ids.sort_by_key(|id| id.0);
            existing.updated_account_ids.dedup();
            existing.removed_account_ids.sort_by_key(|id| id.0);
            existing.removed_account_ids.dedup();
        } else {
            consolidated.push(refresh);
        }
    }
    repair.conversation_refreshes = consolidated;
    Ok(Some(repair))
}

/// Record a verified ActivityPub account migration without retargeting follows.
pub async fn process_remote_actor_move(
    txn: &DatabaseTransaction,
    remote_actor_id: AccountId,
    target_actor_id: AccountId,
) -> Result<bool> {
    let Some(actor) = remote_actor::Entity::find_by_id(remote_actor_id.0)
        .one(txn)
        .await?
    else {
        return Ok(false);
    };
    let mut actor = actor.into_active_model();
    actor.moved_to_remote_actor_id = Set(Some(target_actor_id.0));
    actor.updated_at = Set(OffsetDateTime::now_utc());
    actor.update(txn).await?;
    Ok(true)
}

/// Find one cached remote Note by its canonical ActivityPub ID.
pub async fn find_remote_status_by_activitypub_id(
    db: &impl ConnectionTrait,
    activitypub_id: &str,
) -> Result<Option<RemoteStatus>> {
    remote_status::Entity::find()
        .filter(remote_status::Column::ActivitypubId.eq(activitypub_id))
        .filter(remote_status::Column::DeletedAt.is_null())
        .one(db)
        .await?
        .map(remote_status_from_model)
        .transpose()
}

/// Find one active cached remote Note by its UUID-backed API identifier.
pub async fn find_remote_status_by_id(
    db: &impl ConnectionTrait,
    status_id: StatusId,
) -> Result<Option<RemoteStatus>> {
    remote_status::Entity::find_by_id(status_id.0)
        .filter(remote_status::Column::DeletedAt.is_null())
        .one(db)
        .await?
        .map(remote_status_from_model)
        .transpose()
}

/// Replace the declared attachment set for a cached remote status.
pub async fn replace_remote_media_attachments(
    txn: &DatabaseTransaction,
    status_id: StatusId,
    attachments: &[NewRemoteMediaAttachment],
) -> Result<()> {
    remote_media_attachment::Entity::delete_many()
        .filter(remote_media_attachment::Column::RemoteStatusId.eq(status_id.0))
        .exec(txn)
        .await?;
    let now = OffsetDateTime::now_utc();
    for (status_order, attachment) in attachments.iter().enumerate() {
        remote_media_attachment::ActiveModel {
            id: Set(Uuid::now_v7()),
            remote_status_id: Set(status_id.0),
            status_order: Set(status_order as i32),
            remote_url: Set(attachment.remote_url.clone()),
            content_type: Set(attachment.content_type.clone()),
            description: Set(attachment.description.clone()),
            state: Set(RemoteMediaState::Pending),
            file_path: Set(None),
            preview_file_path: Set(None),
            file_size: Set(None),
            width: Set(None),
            height: Set(None),
            preview_width: Set(None),
            preview_height: Set(None),
            blurhash: Set(None),
            fetched_at: Set(None),
            expires_at: Set(None),
            last_error: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(txn)
        .await?;
    }
    Ok(())
}

/// List attachments declared by a cached remote status.
pub async fn remote_media_attachments_for_status(
    db: &impl ConnectionTrait,
    status_id: StatusId,
) -> Result<Vec<RemoteMediaAttachment>> {
    remote_media_attachment::Entity::find()
        .filter(remote_media_attachment::Column::RemoteStatusId.eq(status_id.0))
        .order_by_asc(remote_media_attachment::Column::StatusOrder)
        .all(db)
        .await?
        .into_iter()
        .map(remote_media_attachment_from_model)
        .collect::<Result<Vec<_>>>()
}

/// Find one remote media attachment by cache identity.
pub async fn find_remote_media_attachment(
    db: &impl ConnectionTrait,
    id: Uuid,
) -> Result<Option<RemoteMediaAttachment>> {
    remote_media_attachment::Entity::find_by_id(id)
        .one(db)
        .await?
        .map(remote_media_attachment_from_model)
        .transpose()
}

fn remote_media_attachment_from_model(
    model: remote_media_attachment::Model,
) -> Result<RemoteMediaAttachment> {
    Ok(RemoteMediaAttachment {
        id: model.id,
        remote_status_id: StatusId(model.remote_status_id),
        status_order: model.status_order,
        remote_url: model.remote_url,
        content_type: model.content_type,
        description: model.description,
        state: model.state,
        file_path: model.file_path,
        preview_file_path: model.preview_file_path,
        file_size: model.file_size,
        width: model.width,
        height: model.height,
        preview_width: model.preview_width,
        preview_height: model.preview_height,
        blurhash: model.blurhash,
        expires_at: model.expires_at,
    })
}

/// Mark an attachment as being fetched and insert a deduplicated fetch job.
pub async fn queue_remote_media_fetch(
    txn: &DatabaseTransaction,
    attachment_id: Uuid,
    job: NewJob,
) -> Result<()> {
    let attachment = remote_media_attachment::Entity::find_by_id(attachment_id)
        .one(txn)
        .await?
        .ok_or_else(|| {
            RoostyError::InvalidInput("remote media attachment does not exist".to_owned())
        })?;
    let mut active = attachment.into_active_model();
    active.state = Set(RemoteMediaState::Pending);
    active.last_error = Set(None);
    active.updated_at = Set(OffsetDateTime::now_utc());
    active.update(txn).await?;
    enqueue_job_in_transaction(txn, job).await?;
    Ok(())
}

/// Record a completed remote-media cache write.
pub async fn mark_remote_media_ready(
    db: &impl ConnectionTrait,
    id: Uuid,
    cache: RemoteMediaCacheWrite,
) -> Result<()> {
    let Some(model) = remote_media_attachment::Entity::find_by_id(id)
        .one(db)
        .await?
    else {
        return Ok(());
    };
    let mut active = model.into_active_model();
    active.state = Set(RemoteMediaState::Ready);
    active.content_type = Set(Some(cache.content_type));
    active.file_path = Set(Some(cache.file_path));
    active.preview_file_path = Set(cache.preview_file_path);
    active.file_size = Set(Some(cache.file_size));
    active.width = Set(cache.width);
    active.height = Set(cache.height);
    active.preview_width = Set(cache.preview_width);
    active.preview_height = Set(cache.preview_height);
    active.blurhash = Set(cache.blurhash);
    active.fetched_at = Set(Some(OffsetDateTime::now_utc()));
    active.expires_at = Set(Some(cache.expires_at));
    active.last_error = Set(None);
    active.updated_at = Set(OffsetDateTime::now_utc());
    active.update(db).await?;
    Ok(())
}

/// Record a remote-media fetch failure without failing the owning status cache.
pub async fn mark_remote_media_failed(
    db: &impl ConnectionTrait,
    id: Uuid,
    error: &str,
) -> Result<()> {
    let Some(model) = remote_media_attachment::Entity::find_by_id(id)
        .one(db)
        .await?
    else {
        return Ok(());
    };
    let mut active = model.into_active_model();
    active.state = Set(RemoteMediaState::Failed);
    active.last_error = Set(Some(error.to_owned()));
    active.updated_at = Set(OffsetDateTime::now_utc());
    active.update(db).await?;
    Ok(())
}

/// Replace the profile-image URLs advertised by a remote actor.
///
/// Unchanged URLs retain their cache entry; changed URLs are reset to pending so
/// a subsequent fetch cannot serve bytes from the old image.
pub async fn replace_remote_profile_media(
    db: &impl ConnectionTrait,
    remote_actor_id: AccountId,
    media: NewRemoteProfileMedia,
) -> Result<()> {
    replace_remote_profile_media_kind(
        db,
        remote_actor_id,
        RemoteProfileMediaKind::Avatar,
        media.avatar_url,
    )
    .await?;
    replace_remote_profile_media_kind(
        db,
        remote_actor_id,
        RemoteProfileMediaKind::Header,
        media.header_url,
    )
    .await
}

async fn replace_remote_profile_media_kind(
    db: &impl ConnectionTrait,
    remote_actor_id: AccountId,
    kind: RemoteProfileMediaKind,
    remote_url: Option<String>,
) -> Result<()> {
    let existing = remote_profile_media::Entity::find()
        .filter(remote_profile_media::Column::RemoteActorId.eq(remote_actor_id.0))
        .filter(remote_profile_media::Column::Kind.eq(kind.to_string()))
        .one(db)
        .await?;
    match (existing, remote_url) {
        (Some(existing), Some(remote_url)) if existing.remote_url == remote_url => Ok(()),
        (Some(existing), Some(remote_url)) => {
            let mut active = existing.into_active_model();
            active.remote_url = Set(remote_url);
            active.content_type = Set(None);
            active.state = Set(RemoteMediaState::Pending);
            active.file_path = Set(None);
            active.file_size = Set(None);
            active.fetched_at = Set(None);
            active.expires_at = Set(None);
            active.last_error = Set(None);
            active.updated_at = Set(OffsetDateTime::now_utc());
            active.update(db).await?;
            Ok(())
        }
        (Some(existing), None) => {
            existing.delete(db).await?;
            Ok(())
        }
        (None, Some(remote_url)) => {
            let now = OffsetDateTime::now_utc();
            remote_profile_media::ActiveModel {
                id: Set(Uuid::now_v7()),
                remote_actor_id: Set(remote_actor_id.0),
                kind: Set(kind),
                remote_url: Set(remote_url),
                state: Set(RemoteMediaState::Pending),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            }
            .insert(db)
            .await?;
            Ok(())
        }
        (None, None) => Ok(()),
    }
}

/// List cached profile-image metadata for a remote actor.
pub async fn remote_profile_media_for_actor(
    db: &impl ConnectionTrait,
    remote_actor_id: AccountId,
) -> Result<Vec<RemoteProfileMedia>> {
    remote_profile_media::Entity::find()
        .filter(remote_profile_media::Column::RemoteActorId.eq(remote_actor_id.0))
        .all(db)
        .await?
        .into_iter()
        .map(remote_profile_media_from_model)
        .collect()
}

/// Batch profile-image metadata for a bounded collection of remote actors.
pub async fn remote_profile_media_for_actors(
    db: &impl ConnectionTrait,
    remote_actor_ids: &[AccountId],
) -> Result<Vec<RemoteProfileMedia>> {
    if remote_actor_ids.is_empty() {
        return Ok(Vec::new());
    }
    remote_profile_media::Entity::find()
        .filter(
            remote_profile_media::Column::RemoteActorId
                .is_in(remote_actor_ids.iter().map(|id| id.0)),
        )
        .all(db)
        .await?
        .into_iter()
        .map(remote_profile_media_from_model)
        .collect()
}

/// Find a remote profile image by its public cache identity.
pub async fn find_remote_profile_media(
    db: &impl ConnectionTrait,
    id: Uuid,
) -> Result<Option<RemoteProfileMedia>> {
    remote_profile_media::Entity::find_by_id(id)
        .one(db)
        .await?
        .map(remote_profile_media_from_model)
        .transpose()
}

fn remote_profile_media_from_model(
    model: remote_profile_media::Model,
) -> Result<RemoteProfileMedia> {
    Ok(RemoteProfileMedia {
        id: model.id,
        remote_actor_id: AccountId(model.remote_actor_id),
        kind: model.kind,
        remote_url: model.remote_url,
        content_type: model.content_type,
        state: model.state,
        file_path: model.file_path,
        expires_at: model.expires_at,
    })
}

/// Mark a remote profile image as pending and enqueue its fetch transactionally.
pub async fn queue_remote_profile_media_fetch(
    txn: &DatabaseTransaction,
    media_id: Uuid,
    job: NewJob,
) -> Result<()> {
    let media = remote_profile_media::Entity::find_by_id(media_id)
        .one(txn)
        .await?
        .ok_or_else(|| {
            RoostyError::InvalidInput("remote profile media does not exist".to_owned())
        })?;
    let mut active = media.into_active_model();
    active.state = Set(RemoteMediaState::Pending);
    active.last_error = Set(None);
    active.updated_at = Set(OffsetDateTime::now_utc());
    active.update(txn).await?;
    enqueue_job_in_transaction(txn, job).await?;
    Ok(())
}

/// Record a completed remote profile-image cache write.
pub async fn mark_remote_profile_media_ready(
    db: &impl ConnectionTrait,
    id: Uuid,
    content_type: String,
    file_path: String,
    file_size: i64,
    expires_at: OffsetDateTime,
) -> Result<()> {
    let Some(model) = remote_profile_media::Entity::find_by_id(id).one(db).await? else {
        return Ok(());
    };
    let mut active = model.into_active_model();
    active.state = Set(RemoteMediaState::Ready);
    active.content_type = Set(Some(content_type));
    active.file_path = Set(Some(file_path));
    active.file_size = Set(Some(file_size));
    active.fetched_at = Set(Some(OffsetDateTime::now_utc()));
    active.expires_at = Set(Some(expires_at));
    active.last_error = Set(None);
    active.updated_at = Set(OffsetDateTime::now_utc());
    active.update(db).await?;
    Ok(())
}

/// Record a failed remote profile-image fetch.
pub async fn mark_remote_profile_media_failed(
    db: &impl ConnectionTrait,
    id: Uuid,
    error: &str,
) -> Result<()> {
    let Some(model) = remote_profile_media::Entity::find_by_id(id).one(db).await? else {
        return Ok(());
    };
    let mut active = model.into_active_model();
    active.state = Set(RemoteMediaState::Failed);
    active.last_error = Set(Some(error.to_owned()));
    active.updated_at = Set(OffsetDateTime::now_utc());
    active.update(db).await?;
    Ok(())
}

/// Mark the locally initiated Follow identified by its activity ID as accepted.
pub async fn accept_remote_following(
    db: &impl ConnectionTrait,
    remote_actor_id: AccountId,
    activity_id: &str,
) -> Result<bool> {
    let result = db.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres, "UPDATE remote_following SET state = 'accepted', updated_at = now() WHERE remote_actor_id = $1 AND activity_id = $2", vec![remote_actor_id.0.into(), activity_id.to_owned().into()])).await?;
    Ok(result.rows_affected() == 1)
}

/// Remove a rejected local-to-remote Follow by the original activity identity.
pub async fn reject_remote_following(
    db: &impl ConnectionTrait,
    remote_actor_id: AccountId,
    activity_id: &str,
) -> Result<bool> {
    let result = db
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "DELETE FROM remote_following WHERE remote_actor_id = $1 AND activity_id = $2",
            vec![remote_actor_id.0.into(), activity_id.to_owned().into()],
        ))
        .await?;
    Ok(result.rows_affected() == 1)
}

/// Remove a local-to-remote follow relationship and return it for Undo delivery.
pub async fn delete_remote_following(
    db: &DbConnection,
    local_account_id: AccountId,
    remote_actor_id: AccountId,
) -> Result<Option<RemoteFollowing>> {
    let row = remote_following::Entity::find()
        .filter(remote_following::Column::LocalAccountId.eq(local_account_id.0))
        .filter(remote_following::Column::RemoteActorId.eq(remote_actor_id.0))
        .one(db)
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let relationship = remote_following_from_model(row.clone());
    remove_remote_account_from_owned_lists(db, local_account_id, remote_actor_id).await?;
    row.into_active_model().delete(db).await?;
    Ok(Some(relationship))
}

/// Remove a local-to-remote follow and insert its Undo delivery job.
///
/// The caller owns and commits `txn`.
pub async fn delete_remote_following_with_job(
    txn: &sea_orm::DatabaseTransaction,
    local_account_id: AccountId,
    remote_actor_id: AccountId,
    job: NewJob,
) -> Result<Option<RemoteFollowing>> {
    let row = remote_following::Entity::find()
        .filter(remote_following::Column::LocalAccountId.eq(local_account_id.0))
        .filter(remote_following::Column::RemoteActorId.eq(remote_actor_id.0))
        .one(txn)
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let relationship = remote_following_from_model(row.clone());
    remove_remote_account_from_owned_lists(txn, local_account_id, remote_actor_id).await?;
    row.into_active_model().delete(txn).await?;
    enqueue_job_in_transaction(txn, job).await?;
    Ok(Some(relationship))
}

/// Insert or refresh a verified remote Note by its canonical ActivityPub object ID.
pub async fn upsert_remote_status(
    db: &DbConnection,
    status: NewRemoteStatus,
) -> Result<RemoteStatus> {
    let tag_names = status.tag_names.clone();
    let txn = db.begin().await?;
    let status = upsert_remote_status_on(&txn, status).await?;
    replace_remote_status_tags(&txn, status.id, &tag_names).await?;
    Box::pin(replace_status_preview_card(
        &txn,
        PreviewStatusTarget::Remote(status.id),
        &status.content,
        utc_date(status.published_at),
        PreviewActorOrigin::Remote,
        status.remote_actor_id.0,
        true,
    ))
    .await?;
    refresh_status_search_document(&txn, StatusReference::Remote(status.id)).await?;
    txn.commit().await?;
    Ok(status)
}

/// Persist a remote Note through either a pool connection or a transaction.
async fn upsert_remote_status_on<C>(db: &C, status: NewRemoteStatus) -> Result<RemoteStatus>
where
    C: ConnectionTrait,
{
    let quote_automatic_policy = status.quote_automatic_policy.clone();
    let quote_manual_policy = status.quote_manual_policy.clone();
    let existing = remote_status::Entity::find()
        .filter(remote_status::Column::ActivitypubId.eq(&status.activitypub_id))
        .one(db)
        .await?;
    let model = if let Some(existing) = existing {
        if existing.remote_actor_id != status.remote_actor_id.0 {
            return Err(RoostyError::InvalidInput(
                "remote status author does not match cached author".to_owned(),
            ));
        }
        let mut active = existing.into_active_model();
        active.content = Set(status.content);
        active.visibility = Set(status.visibility);
        active.published_at = Set(status.published_at);
        active.updated_at = Set(status.updated_at);
        active.deleted_at = Set(None);
        active.in_reply_to = Set(status.in_reply_to);
        active.in_reply_to_local_status_id = Set(status.in_reply_to_local_status_id.map(|id| id.0));
        active.in_reply_to_remote_status_id =
            Set(status.in_reply_to_remote_status_id.map(|id| id.0));
        active.object = Set(status.object);
        active.quote_automatic_policy = Set(serde_json::to_value(&quote_automatic_policy)
            .map_err(|error| RoostyError::InvalidInput(error.to_string()))?);
        active.quote_manual_policy = Set(serde_json::to_value(&quote_manual_policy)
            .map_err(|error| RoostyError::InvalidInput(error.to_string()))?);
        active.update(db).await?
    } else {
        remote_status::ActiveModel {
            id: Set(Uuid::now_v7()),
            activitypub_id: Set(status.activitypub_id),
            remote_actor_id: Set(status.remote_actor_id.0),
            content: Set(status.content),
            visibility: Set(status.visibility),
            published_at: Set(status.published_at),
            updated_at: Set(status.updated_at),
            deleted_at: Set(None),
            in_reply_to: Set(status.in_reply_to),
            in_reply_to_local_status_id: Set(status.in_reply_to_local_status_id.map(|id| id.0)),
            in_reply_to_remote_status_id: Set(status.in_reply_to_remote_status_id.map(|id| id.0)),
            conversation_id: Set(None),
            object: Set(status.object),
            quote_automatic_policy: Set(serde_json::to_value(quote_automatic_policy)
                .map_err(|error| RoostyError::InvalidInput(error.to_string()))?),
            quote_manual_policy: Set(serde_json::to_value(quote_manual_policy)
                .map_err(|error| RoostyError::InvalidInput(error.to_string()))?),
            ..Default::default()
        }
        .insert(db)
        .await?
    };
    let actor_id = AccountId(model.remote_actor_id);
    refresh_remote_actor_last_status_at(db, actor_id).await?;
    remote_status_from_model(model)
}

/// Record an inbound Create or Update and cache its Note atomically.
pub async fn process_remote_status_upsert(
    txn: &sea_orm::DatabaseTransaction,
    status: NewRemoteStatus,
    attachments: &[NewRemoteMediaAttachment],
) -> Result<RemoteStatusUpsertResult> {
    let tag_names = status.tag_names.clone();
    let existing = remote_status::Entity::find()
        .filter(remote_status::Column::ActivitypubId.eq(&status.activitypub_id))
        .lock_exclusive()
        .one(txn)
        .await?;
    let Some(existing) = existing else {
        let status = upsert_remote_status_on(txn, status).await?;
        replace_remote_media_attachments(txn, status.id, attachments).await?;
        replace_remote_status_tags(txn, status.id, &tag_names).await?;
        Box::pin(replace_status_preview_card(
            txn,
            PreviewStatusTarget::Remote(status.id),
            &status.content,
            utc_date(status.published_at),
            PreviewActorOrigin::Remote,
            status.remote_actor_id.0,
            true,
        ))
        .await?;
        refresh_status_search_document(txn, StatusReference::Remote(status.id)).await?;
        return Ok(RemoteStatusUpsertResult::Created(status));
    };
    if existing.remote_actor_id != status.remote_actor_id.0 {
        return Err(RoostyError::InvalidInput(
            "remote status author does not match cached author".to_owned(),
        ));
    }
    let current_media = remote_media_attachment::Entity::find()
        .filter(remote_media_attachment::Column::RemoteStatusId.eq(existing.id))
        .order_by_asc(remote_media_attachment::Column::StatusOrder)
        .all(txn)
        .await?;
    let media_changed = current_media.len() != attachments.len()
        || current_media
            .iter()
            .zip(attachments)
            .any(|(current, replacement)| {
                current.remote_url != replacement.remote_url
                    || current.content_type != replacement.content_type
                    || current.description != replacement.description
            });
    let projection_changed = existing.content != status.content
        || existing.object.get("summary") != status.object.get("summary")
        || existing.object.get("sensitive") != status.object.get("sensitive")
        || existing.object.get("tag") != status.object.get("tag")
        || existing.object.get("quote") != status.object.get("quote")
        || existing.object.get("quoteUri") != status.object.get("quoteUri")
        || existing.object.get("quoteAuthorization") != status.object.get("quoteAuthorization")
        || existing.quote_automatic_policy
            != serde_json::to_value(&status.quote_automatic_policy)
                .map_err(|error| RoostyError::InvalidInput(error.to_string()))?
        || existing.quote_manual_policy
            != serde_json::to_value(&status.quote_manual_policy)
                .map_err(|error| RoostyError::InvalidInput(error.to_string()))?;
    if status.updated_at <= existing.updated_at || (!projection_changed && !media_changed) {
        return Ok(RemoteStatusUpsertResult::Unchanged(
            remote_status_from_model(existing)?,
        ));
    }
    if remote_status_edit::Entity::find()
        .filter(remote_status_edit::Column::RemoteStatusId.eq(existing.id))
        .one(txn)
        .await?
        .is_none()
    {
        let timestamp = if existing.updated_at == existing.published_at {
            existing.published_at
        } else {
            existing.updated_at
        };
        remote_status_snapshot(txn, &existing, timestamp).await?;
    }
    let revision_timestamp = status.updated_at;
    if existing.deleted_at.is_none() && existing.visibility == StatusVisibility::Public {
        let old_tag_ids = remote_status_tag::Entity::find()
            .filter(remote_status_tag::Column::RemoteStatusId.eq(existing.id))
            .all(txn)
            .await?
            .into_iter()
            .map(|row| row.tag_id)
            .collect::<Vec<_>>();
        adjust_tag_usage(
            txn,
            &old_tag_ids,
            utc_date(existing.published_at),
            "remote",
            existing.remote_actor_id,
            -1,
        )
        .await?;
    }
    let status = upsert_remote_status_on(txn, status).await?;
    mark_trend_dirty(txn, "remote_status", status.id.0).await?;
    replace_remote_media_attachments(txn, status.id, attachments).await?;
    replace_remote_status_tags(txn, status.id, &tag_names).await?;
    Box::pin(replace_status_preview_card(
        txn,
        PreviewStatusTarget::Remote(status.id),
        &status.content,
        utc_date(status.published_at),
        PreviewActorOrigin::Remote,
        status.remote_actor_id.0,
        true,
    ))
    .await?;
    refresh_status_search_document(txn, StatusReference::Remote(status.id)).await?;
    let current = remote_status::Entity::find_by_id(status.id.0)
        .one(txn)
        .await?
        .ok_or_else(|| RoostyError::InvalidInput("remote status disappeared".to_owned()))?;
    remote_status_snapshot(txn, &current, revision_timestamp).await?;
    Ok(RemoteStatusUpsertResult::Updated(status))
}

/// Link unresolved cached replies after their remote parent becomes available.
///
/// Matching the retained canonical URL makes this repair safe when the parent
/// and child are fetched by different workers or Roosty processes.
pub async fn link_unresolved_remote_replies_to_parent(
    db: &impl ConnectionTrait,
    parent: &RemoteStatus,
) -> Result<u64> {
    let result = db
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
            UPDATE remote_status
            SET in_reply_to_remote_status_id = $1
            WHERE in_reply_to = $2
              AND in_reply_to_local_status_id IS NULL
              AND in_reply_to_remote_status_id IS NULL
              AND deleted_at IS NULL
            "#,
            vec![parent.id.0.into(), parent.activitypub_id.clone().into()],
        ))
        .await?;
    Ok(result.rows_affected())
}

/// Replace the indexed hashtags for a cached Note inside its caller-owned transaction.
async fn replace_remote_status_tags(
    txn: &DatabaseTransaction,
    status_id: StatusId,
    tag_names: &[String],
) -> Result<()> {
    let status = remote_status::Entity::find_by_id(status_id.0)
        .one(txn)
        .await?;
    remote_status_tag::Entity::delete_many()
        .filter(remote_status_tag::Column::RemoteStatusId.eq(status_id.0))
        .exec(txn)
        .await?;

    let now = OffsetDateTime::now_utc();
    let mut names = tag_names
        .iter()
        .map(|name| normalize_tag_name(name))
        .filter(|name| {
            !name.is_empty()
                && name
                    .chars()
                    .all(|character| character.is_alphanumeric() || character == '_')
        })
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    let mut new_tag_ids = Vec::with_capacity(names.len());
    for name in names {
        let tag = find_or_create_local_tag(txn, &name, now).await?;
        new_tag_ids.push(tag.id);
        remote_status_tag::ActiveModel {
            remote_status_id: Set(status_id.0),
            tag_id: Set(tag.id),
            created_at: Set(now),
        }
        .insert(txn)
        .await?;
    }
    if let Some(status) = status
        && status.deleted_at.is_none()
        && status.visibility == StatusVisibility::Public
    {
        adjust_tag_usage(
            txn,
            &new_tag_ids,
            utc_date(status.published_at),
            "remote",
            status.remote_actor_id,
            1,
        )
        .await?;
    }
    Ok(())
}

fn remote_object_spoiler_text(object: &JsonValue) -> String {
    object
        .get("summary")
        .and_then(JsonValue::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn remote_object_sensitive(object: &JsonValue) -> bool {
    object
        .get("sensitive")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false)
}

async fn remote_status_snapshot(
    txn: &DatabaseTransaction,
    status: &remote_status::Model,
    created_at: OffsetDateTime,
) -> Result<()> {
    let edit_id = Uuid::now_v7();
    let poll_options = status
        .object
        .get("oneOf")
        .or_else(|| status.object.get("anyOf"))
        .and_then(JsonValue::as_array)
        .map(|options| {
            options
                .iter()
                .filter_map(|option| option.get("name").and_then(JsonValue::as_str))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        });
    remote_status_edit::ActiveModel {
        id: Set(edit_id),
        remote_status_id: Set(status.id),
        content: Set(status.content.clone()),
        spoiler_text: Set(remote_object_spoiler_text(&status.object)),
        sensitive: Set(remote_object_sensitive(&status.object)),
        object: Set(status.object.clone()),
        poll_options: Set(poll_options.map(|options| serde_json::json!(options))),
        created_at: Set(created_at),
    }
    .insert(txn)
    .await?;
    let media = remote_media_attachment::Entity::find()
        .filter(remote_media_attachment::Column::RemoteStatusId.eq(status.id))
        .order_by_asc(remote_media_attachment::Column::StatusOrder)
        .all(txn)
        .await?;
    for (order, item) in media.into_iter().enumerate() {
        remote_status_edit_media::ActiveModel {
            id: Set(Uuid::now_v7()),
            remote_status_edit_id: Set(edit_id),
            source_attachment_id: Set(Some(item.id)),
            status_order: Set(order as i32),
            remote_url: Set(item.remote_url),
            content_type: Set(item.content_type),
            file_path: Set(item.file_path),
            preview_file_path: Set(item.preview_file_path),
            description: Set(item.description),
            width: Set(item.width),
            height: Set(item.height),
            preview_width: Set(item.preview_width),
            preview_height: Set(item.preview_height),
            blurhash: Set(item.blurhash),
        }
        .insert(txn)
        .await?;
    }
    Ok(())
}

/// Soft-delete a remote Note only when its verified author owns it.
pub async fn delete_remote_status(
    db: &impl ConnectionTrait,
    activitypub_id: &str,
    remote_actor_id: AccountId,
) -> Result<bool> {
    let Some(status) = remote_status::Entity::find()
        .filter(remote_status::Column::ActivitypubId.eq(activitypub_id))
        .filter(remote_status::Column::RemoteActorId.eq(remote_actor_id.0))
        .filter(remote_status::Column::DeletedAt.is_null())
        .one(db)
        .await?
    else {
        return Ok(false);
    };
    let status_id = StatusId(status.id);
    remove_status_preview_card(db, PreviewStatusTarget::Remote(status_id)).await?;
    let tag_ids = remote_status_tag::Entity::find()
        .filter(remote_status_tag::Column::RemoteStatusId.eq(status.id))
        .all(db)
        .await?
        .into_iter()
        .map(|row| row.tag_id)
        .collect::<Vec<_>>();
    if status.visibility == StatusVisibility::Public {
        adjust_tag_usage(
            db,
            &tag_ids,
            utc_date(status.published_at),
            "remote",
            status.remote_actor_id,
            -1,
        )
        .await?;
    }
    let actor_id = AccountId(status.remote_actor_id);
    let mut active = status.into_active_model();
    active.deleted_at = Set(Some(OffsetDateTime::now_utc()));
    active.update(db).await?;
    refresh_status_search_document(db, StatusReference::Remote(status_id)).await?;
    refresh_remote_actor_last_status_at(db, actor_id).await?;
    mark_trend_dirty(db, "remote_status", status_id.0).await?;
    Ok(true)
}

/// Record an inbound Delete and soft-delete its cached Note atomically.
pub async fn process_remote_status_delete(
    txn: &sea_orm::DatabaseTransaction,
    remote_actor_id: AccountId,
    activitypub_id: &str,
) -> Result<Option<RemoteDeleteRepair>> {
    let status = remote_status::Entity::find()
        .filter(remote_status::Column::ActivitypubId.eq(activitypub_id))
        .filter(remote_status::Column::RemoteActorId.eq(remote_actor_id.0))
        .filter(remote_status::Column::DeletedAt.is_null())
        .one(txn)
        .await?;
    match status {
        Some(status) => repair_one_remote_status_delete(txn, status, true)
            .await
            .map(Some),
        None => Ok(None),
    }
}

/// Remove all live projections that point at one cached remote Note.
async fn repair_one_remote_status_delete(
    txn: &DatabaseTransaction,
    status: remote_status::Model,
    refresh_directory_activity: bool,
) -> Result<RemoteDeleteRepair> {
    let status_id = StatusId(status.id);
    let author_id = AccountId(status.remote_actor_id);
    let explicit_recipient_ids = remote_status_local_recipient::Entity::find()
        .filter(remote_status_local_recipient::Column::RemoteStatusId.eq(status.id))
        .all(txn)
        .await?
        .into_iter()
        .map(|recipient| AccountId(recipient.account_id))
        .collect::<Vec<_>>();
    let active_mention_ids = active_local_mentions_for_remote_status(txn, status_id).await?;
    let visibility = status.visibility;
    let stream_visibility = if status.in_reply_to.is_some() {
        StatusVisibility::Unlisted
    } else {
        visibility
    };
    let mut home_recipient_ids = match visibility {
        StatusVisibility::Public | StatusVisibility::Unlisted | StatusVisibility::Private => {
            accepted_local_followers_of_remote_actor(txn, author_id).await?
        }
        StatusVisibility::Direct => Vec::new(),
    };
    if visibility == StatusVisibility::Public {
        home_recipient_ids.extend(remote_tag_follower_ids_for_status(txn, status_id).await?);
        home_recipient_ids.sort_by_key(|id| id.0);
        home_recipient_ids.dedup();
    }
    if visibility == StatusVisibility::Private {
        home_recipient_ids.extend(explicit_recipient_ids.iter().copied());
        home_recipient_ids.sort_by_key(|id| id.0);
        home_recipient_ids.dedup();
    }
    if visibility != StatusVisibility::Direct {
        home_recipient_ids.extend(active_mention_ids);
        home_recipient_ids.sort_by_key(|id| id.0);
        home_recipient_ids.dedup();
    }
    let direct_recipient_ids = if visibility == StatusVisibility::Direct {
        explicit_recipient_ids
    } else {
        Vec::new()
    };
    let has_media = remote_media_attachment::Entity::find()
        .filter(remote_media_attachment::Column::RemoteStatusId.eq(status.id))
        .count(txn)
        .await?
        > 0;
    let tag_ids = remote_status_tag::Entity::find()
        .filter(remote_status_tag::Column::RemoteStatusId.eq(status.id))
        .all(txn)
        .await?
        .into_iter()
        .map(|row| row.tag_id)
        .collect::<Vec<_>>();
    if status.visibility == StatusVisibility::Public {
        adjust_tag_usage(
            txn,
            &tag_ids,
            utc_date(status.published_at),
            "remote",
            status.remote_actor_id,
            -1,
        )
        .await?;
    }

    let local_reblogs = local_remote_status_reblog::Entity::find()
        .filter(local_remote_status_reblog::Column::RemoteStatusId.eq(status.id))
        .all(txn)
        .await?;
    let inbound_reblogs = remote_status_reblog::Entity::find()
        .filter(remote_status_reblog::Column::RemoteStatusId.eq(status.id))
        .all(txn)
        .await?;
    let mut projections = vec![DeleteStreamProjection {
        status_id: status.id.to_string(),
        actor_id: author_id,
        home_recipient_ids,
        direct_recipient_ids,
        visibility: stream_visibility,
        status_origin: StreamingStatusOrigin::Remote,
        has_media,
    }];
    projections.extend(local_reblogs.iter().map(|reblog| DeleteStreamProjection {
        status_id: reblog.id.to_string(),
        actor_id: AccountId(reblog.local_account_id),
        home_recipient_ids: vec![AccountId(reblog.local_account_id)],
        direct_recipient_ids: Vec::new(),
        visibility: StatusVisibility::Direct,
        status_origin: StreamingStatusOrigin::Local,
        has_media: false,
    }));
    for reblog in &inbound_reblogs {
        let actor_id = AccountId(reblog.remote_actor_id);
        projections.push(DeleteStreamProjection {
            status_id: reblog.id.to_string(),
            actor_id,
            home_recipient_ids: accepted_local_followers_of_remote_actor(txn, actor_id).await?,
            direct_recipient_ids: Vec::new(),
            visibility: StatusVisibility::Direct,
            status_origin: StreamingStatusOrigin::Remote,
            has_media: false,
        });
    }

    remove_status_preview_card(txn, PreviewStatusTarget::Remote(status_id)).await?;
    let mut active = status.into_active_model();
    active.deleted_at = Set(Some(OffsetDateTime::now_utc()));
    active.update(txn).await?;
    refresh_status_search_document(txn, StatusReference::Remote(status_id)).await?;
    if refresh_directory_activity {
        refresh_remote_actor_last_status_at(txn, author_id).await?;
    }
    mark_trend_dirty(txn, "remote_status", status_id.0).await?;
    remote_status_pin::Entity::delete_many()
        .filter(remote_status_pin::Column::RemoteStatusId.eq(status_id.0))
        .exec(txn)
        .await?;
    local_notification::Entity::delete_many()
        .filter(local_notification::Column::RemoteStatusId.eq(status_id.0))
        .exec(txn)
        .await?;
    local_remote_status_favourite::Entity::delete_many()
        .filter(local_remote_status_favourite::Column::RemoteStatusId.eq(status_id.0))
        .exec(txn)
        .await?;
    local_remote_status_reblog::Entity::delete_many()
        .filter(local_remote_status_reblog::Column::RemoteStatusId.eq(status_id.0))
        .exec(txn)
        .await?;
    remote_status_reblog::Entity::delete_many()
        .filter(remote_status_reblog::Column::RemoteStatusId.eq(status_id.0))
        .exec(txn)
        .await?;
    remote_status::Entity::update_many()
        .col_expr(
            remote_status::Column::InReplyToRemoteStatusId,
            sea_orm::sea_query::Expr::value(Option::<Uuid>::None),
        )
        .filter(remote_status::Column::InReplyToRemoteStatusId.eq(status_id.0))
        .exec(txn)
        .await?;
    local_status::Entity::update_many()
        .col_expr(
            local_status::Column::InReplyToRemoteStatusId,
            sea_orm::sea_query::Expr::value(Option::<Uuid>::None),
        )
        .filter(local_status::Column::InReplyToRemoteStatusId.eq(status_id.0))
        .exec(txn)
        .await?;

    let conversation_refreshes = repair_direct_conversation_after_delete(
        txn,
        remote_status_conversation_id(txn, status_id).await?,
    )
    .await?
    .into_iter()
    .collect();
    Ok(RemoteDeleteRepair {
        projections,
        conversation_refreshes,
        deleted_status_count: 1,
    })
}

/// Find a remote actor by its canonical ActivityPub ID.
pub async fn find_remote_actor_by_activitypub_id(
    db: &impl ConnectionTrait,
    activitypub_id: &str,
) -> Result<Option<RemoteActor>> {
    Ok(remote_actor::Entity::find()
        .filter(remote_actor::Column::ActivitypubId.eq(activitypub_id))
        .one(db)
        .await?
        .map(remote_actor_from_model))
}

/// Find a remote actor by its UUID-backed API identifier.
pub async fn find_remote_actor_by_id<C>(db: &C, actor_id: AccountId) -> Result<Option<RemoteActor>>
where
    C: ConnectionTrait,
{
    Ok(remote_actor::Entity::find_by_id(actor_id.0)
        .one(db)
        .await?
        .map(remote_actor_from_model))
}

/// Find a remote actor by its canonical WebFinger handle.
pub async fn find_remote_actor_by_handle(
    db: &impl ConnectionTrait,
    username: &str,
    domain: &str,
) -> Result<Option<RemoteActor>> {
    Ok(remote_actor::Entity::find()
        .filter(remote_actor::Column::Username.eq(username))
        .filter(remote_actor::Column::Domain.eq(domain))
        .one(db)
        .await?
        .map(remote_actor_from_model))
}

/// Count active cached statuses for a remote actor profile.
pub async fn count_remote_statuses_by_account(
    db: &impl ConnectionTrait,
    actor_id: AccountId,
) -> Result<u64> {
    Ok(remote_status::Entity::find()
        .filter(remote_status::Column::RemoteActorId.eq(actor_id.0))
        .filter(remote_status::Column::DeletedAt.is_null())
        .filter(remote_status::Column::Visibility.is_in(["public", "unlisted"]))
        .count(db)
        .await?)
}

/// Return the newest active cached status date for a remote actor profile.
pub async fn last_remote_status_at(
    db: &impl ConnectionTrait,
    actor_id: AccountId,
) -> Result<Option<OffsetDateTime>> {
    Ok(remote_status::Entity::find()
        .filter(remote_status::Column::RemoteActorId.eq(actor_id.0))
        .filter(remote_status::Column::DeletedAt.is_null())
        .filter(remote_status::Column::Visibility.is_in(["public", "unlisted"]))
        .order_by_desc(remote_status::Column::PublishedAt)
        .one(db)
        .await?
        .map(|status| status.published_at))
}

/// Insert or refresh a remote actor cache entry by canonical actor ID.
pub async fn upsert_remote_actor(
    db: &impl ConnectionTrait,
    actor: &RemoteActor,
) -> Result<RemoteActor> {
    upsert_remote_actor_with_identity(db, actor, true).await
}

/// Refresh an actor document without replacing its separately discovered WebFinger handle.
pub async fn refresh_remote_actor(
    db: &impl ConnectionTrait,
    actor: &RemoteActor,
) -> Result<RemoteActor> {
    upsert_remote_actor_with_identity(db, actor, false).await
}

async fn upsert_remote_actor_with_identity(
    db: &impl ConnectionTrait,
    actor: &RemoteActor,
    replace_identity: bool,
) -> Result<RemoteActor> {
    let now = OffsetDateTime::now_utc();
    let existing = remote_actor::Entity::find()
        .filter(remote_actor::Column::ActivitypubId.eq(&actor.activitypub_id))
        .one(db)
        .await?;
    let model = if let Some(existing) = existing {
        let mut active = existing.into_active_model();
        if replace_identity {
            active.username = Set(actor.username.clone());
            active.domain = Set(actor.domain.clone());
        }
        active.display_name = Set(actor.display_name.clone());
        active.summary = Set(actor.summary.clone());
        active.emojis = Set(actor.emojis.clone());
        active.inbox_url = Set(actor.inbox_url.clone());
        active.shared_inbox_url = Set(actor.shared_inbox_url.clone());
        active.followers_url = Set(actor.followers_url.clone());
        active.featured_url = Set(actor.featured_url.clone());
        active.featured_tags_url = Set(actor.featured_tags_url.clone());
        active.public_key_id = Set(actor.public_key_id.clone());
        active.public_key_pem = Set(actor.public_key_pem.clone());
        active.discoverable = Set(actor.discoverable);
        active.fetched_at = Set(now);
        active.expires_at = Set(actor.expires_at);
        if let Some(profile_created_at) = actor.profile_created_at {
            active.profile_created_at = Set(Some(profile_created_at));
        }
        active.updated_at = Set(now);
        active.update(db).await?
    } else {
        remote_actor::ActiveModel {
            id: Set(actor.id.0),
            activitypub_id: Set(actor.activitypub_id.clone()),
            username: Set(actor.username.clone()),
            domain: Set(actor.domain.clone()),
            display_name: Set(actor.display_name.clone()),
            summary: Set(actor.summary.clone()),
            emojis: Set(actor.emojis.clone()),
            inbox_url: Set(actor.inbox_url.clone()),
            shared_inbox_url: Set(actor.shared_inbox_url.clone()),
            followers_url: Set(actor.followers_url.clone()),
            featured_url: Set(actor.featured_url.clone()),
            featured_tags_url: Set(actor.featured_tags_url.clone()),
            public_key_id: Set(actor.public_key_id.clone()),
            public_key_pem: Set(actor.public_key_pem.clone()),
            discoverable: Set(actor.discoverable),
            fetched_at: Set(now),
            expires_at: Set(actor.expires_at),
            profile_created_at: Set(actor.profile_created_at),
            created_at: Set(actor.first_seen_at),
            ..Default::default()
        }
        .insert(db)
        .await?
    };
    Ok(remote_actor_from_model(model))
}

/// Store remote custom emoji metadata before their image bytes are fetched.
pub async fn upsert_remote_custom_emojis(
    db: &impl ConnectionTrait,
    emojis: &[NewRemoteCustomEmoji],
) -> Result<()> {
    for emoji in emojis {
        let existing = remote_custom_emoji::Entity::find()
            .filter(remote_custom_emoji::Column::RemoteUrl.eq(&emoji.remote_url))
            .one(db)
            .await?;
        if let Some(existing) = existing {
            let mut active = existing.into_active_model();
            active.shortcode = Set(emoji.shortcode.clone());
            active.updated_at = Set(OffsetDateTime::now_utc());
            active.update(db).await?;
        } else {
            let now = OffsetDateTime::now_utc();
            remote_custom_emoji::ActiveModel {
                id: Set(Uuid::now_v7()),
                shortcode: Set(emoji.shortcode.clone()),
                remote_url: Set(emoji.remote_url.clone()),
                state: Set(RemoteMediaState::Pending),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            }
            .insert(db)
            .await?;
        }
    }
    Ok(())
}

/// Look up a cached remote emoji using the URL declared in its ActivityPub tag.
pub async fn find_remote_custom_emoji_by_url(
    db: &impl ConnectionTrait,
    remote_url: &str,
) -> Result<Option<RemoteCustomEmoji>> {
    remote_custom_emoji::Entity::find()
        .filter(remote_custom_emoji::Column::RemoteUrl.eq(remote_url))
        .one(db)
        .await?
        .map(remote_custom_emoji_from_model)
        .transpose()
}

/// Look up one remote emoji cache entry by proxy ID.
pub async fn find_remote_custom_emoji(
    db: &impl ConnectionTrait,
    id: Uuid,
) -> Result<Option<RemoteCustomEmoji>> {
    remote_custom_emoji::Entity::find_by_id(id)
        .one(db)
        .await?
        .map(remote_custom_emoji_from_model)
        .transpose()
}

/// Record a completed remote custom-emoji cache write.
pub async fn mark_remote_custom_emoji_ready(
    db: &impl ConnectionTrait,
    id: Uuid,
    content_type: String,
    file_path: String,
    file_size: i64,
    expires_at: OffsetDateTime,
) -> Result<()> {
    let Some(model) = remote_custom_emoji::Entity::find_by_id(id).one(db).await? else {
        return Ok(());
    };
    let mut active = model.into_active_model();
    active.state = Set(RemoteMediaState::Ready);
    active.content_type = Set(Some(content_type));
    active.file_path = Set(Some(file_path));
    active.file_size = Set(Some(file_size));
    active.fetched_at = Set(Some(OffsetDateTime::now_utc()));
    active.expires_at = Set(Some(expires_at));
    active.last_error = Set(None);
    active.updated_at = Set(OffsetDateTime::now_utc());
    active.update(db).await?;
    Ok(())
}

/// Record a remote custom-emoji fetch failure.
pub async fn mark_remote_custom_emoji_failed(
    db: &impl ConnectionTrait,
    id: Uuid,
    error: &str,
) -> Result<()> {
    let Some(model) = remote_custom_emoji::Entity::find_by_id(id).one(db).await? else {
        return Ok(());
    };
    let mut active = model.into_active_model();
    active.state = Set(RemoteMediaState::Failed);
    active.last_error = Set(Some(error.to_owned()));
    active.updated_at = Set(OffsetDateTime::now_utc());
    active.update(db).await?;
    Ok(())
}

/// Look up the persisted ActivityPub signing key for a local account.
pub async fn find_local_actor_key(
    db: &impl ConnectionTrait,
    account_id: AccountId,
) -> Result<Option<LocalActorKey>> {
    let key = local_actor_key::Entity::find_by_id(account_id.0)
        .one(db)
        .await?;
    Ok(key.map(|key| LocalActorKey {
        public_key_pem: key.public_key_pem,
        private_key_ciphertext: key.private_key_ciphertext,
        private_key_nonce: key.private_key_nonce,
    }))
}

/// Persist a newly generated ActivityPub signing key.
pub async fn create_local_actor_key(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    key: &LocalActorKey,
) -> Result<()> {
    local_actor_key::ActiveModel {
        account_id: Set(account_id.0),
        public_key_pem: Set(key.public_key_pem.clone()),
        private_key_ciphertext: Set(key.private_key_ciphertext.clone()),
        private_key_nonce: Set(key.private_key_nonce.clone()),
        ..Default::default()
    }
    .insert(db)
    .await?;
    Ok(())
}

/// Mutable local account settings accepted from account update APIs.
#[derive(Clone, Debug, Default)]
pub struct LocalAccountSettingsUpdate {
    /// Profile display name.
    pub display_name: Option<String>,
    /// Plain-text profile note.
    pub note: Option<String>,
    /// Whether follow requests require approval.
    pub locked: Option<bool>,
    /// Whether this account is automated.
    pub bot: Option<bool>,
    /// Whether this account can be discovered in profile directories.
    pub discoverable: Option<bool>,
    /// Default visibility for authored statuses.
    pub default_visibility: Option<StatusVisibility>,
    /// Whether authored statuses are sensitive by default.
    pub default_sensitive: Option<bool>,
    /// Default language for authored statuses.
    pub default_language: Option<Option<String>>,
    /// Default quote policy for authored statuses.
    pub default_quote_policy: Option<QuoteApprovalPolicy>,
    /// Profile metadata fields.
    pub profile_fields: Option<JsonValue>,
    /// Optional replacement avatar path relative to the media root.
    pub avatar_file_path: Option<String>,
    /// Optional replacement header path relative to the media root.
    pub header_file_path: Option<String>,
}

/// Local status data returned by status and timeline queries.
#[derive(Clone, Debug)]
pub struct LocalStatus {
    /// Internal status identifier.
    pub id: StatusId,
    /// Authoring local account identifier.
    pub account_id: AccountId,
    /// Plain text status content.
    pub content: String,
    /// Mastodon status visibility value.
    pub visibility: StatusVisibility,
    /// Whether the status is marked sensitive.
    pub sensitive: bool,
    /// Optional content warning text.
    pub spoiler_text: String,
    /// Optional BCP-47 language tag.
    pub language: Option<String>,
    /// Optional local status this status replies to.
    pub in_reply_to_id: Option<StatusId>,
    /// Optional cached remote status this local status replies to.
    pub in_reply_to_remote_status_id: Option<StatusId>,
    /// Optional local direct-message conversation containing this status.
    pub conversation_id: Option<Uuid>,
    /// Creation timestamp.
    pub created_at: OffsetDateTime,
    /// Last update timestamp.
    pub updated_at: OffsetDateTime,
    /// Soft-delete timestamp.
    pub deleted_at: Option<OffsetDateTime>,
    /// Per-status quote consent policy.
    pub quote_approval_policy: QuoteApprovalPolicy,
}

/// Stored local hashtag metadata.
#[derive(Clone, Debug, FromQueryResult)]
pub struct LocalTag {
    /// Internal hashtag identifier.
    pub id: Uuid,
    /// Normalized hashtag name without the leading `#`.
    pub name: String,
    /// Creation timestamp.
    pub created_at: OffsetDateTime,
    /// Last update timestamp.
    pub updated_at: OffsetDateTime,
}

/// One Mastodon tag history bucket.
#[derive(Clone, Debug)]
pub struct LocalTagHistory {
    /// Midnight UTC Unix timestamp for this history bucket.
    pub day: u64,
    /// Number of locally known public status uses on this day.
    pub uses: u64,
    /// Number of distinct locally known accounts using the tag on this day.
    pub accounts: u64,
}

/// A locally known hashtag selected for Mastodon-compatible trend discovery.
#[derive(Clone, Debug)]
pub struct TrendingTag {
    /// Shared hashtag metadata.
    pub tag: LocalTag,
    /// Public local and cached-remote usage during the latest seven UTC days.
    pub history: Vec<LocalTagHistory>,
}

/// A trend-ranked status and its cached interaction totals.
#[derive(Clone, Debug)]
pub struct TrendingStatus {
    /// Local or cached-remote status selected for discovery.
    pub item: PublicTimelineItem,
    /// Known favourites from local and remote actors.
    pub favourites_count: u64,
    /// Known boosts from local and remote actors.
    pub reblogs_count: u64,
}

/// A strongly typed target in the shared trend bookkeeping tables.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrendTarget {
    LocalStatus(StatusId),
    RemoteStatus(StatusId),
    Tag(Uuid),
}

/// Cached Mastodon preview-card metadata for one normalized article URL.
#[derive(Clone, Debug)]
pub struct PreviewCard {
    pub id: Uuid,
    pub url: String,
    pub title: String,
    pub description: String,
    pub author_name: String,
    pub author_url: String,
    pub provider_name: String,
    pub provider_url: String,
    pub image_file_path: Option<String>,
    pub image_width: u32,
    pub image_height: u32,
    pub blurhash: Option<String>,
    pub published_at: Option<OffsetDateTime>,
}

/// Metadata written after a bounded, SSRF-safe preview fetch.
#[derive(Clone, Debug)]
pub struct PreviewCardUpdate {
    pub title: String,
    pub description: String,
    pub author_name: String,
    pub author_url: String,
    pub provider_name: String,
    pub provider_url: String,
    pub image_file_path: Option<String>,
    pub image_width: u32,
    pub image_height: u32,
    pub blurhash: Option<String>,
    pub published_at: Option<OffsetDateTime>,
}

/// One trend-ranked preview card and its seven complete UTC usage buckets.
#[derive(Clone, Debug)]
pub struct TrendingLink {
    pub card: PreviewCard,
    pub history: Vec<LocalTagHistory>,
}

impl TrendTarget {
    fn persisted(self) -> (&'static str, Uuid) {
        match self {
            Self::LocalStatus(id) => ("local_status", id.0),
            Self::RemoteStatus(id) => ("remote_status", id.0),
            Self::Tag(id) => ("tag", id),
        }
    }
}

/// Atomically adjust a status interaction counter and coalesce a scoring request.
async fn adjust_status_trend<C>(
    db: &C,
    target: TrendTarget,
    favourite_delta: i64,
    reblog_delta: i64,
) -> Result<()>
where
    C: ConnectionTrait,
{
    let (kind, target_id, status_table, id_column, published_column, local_id, remote_id) =
        match target {
            TrendTarget::LocalStatus(id) => (
                "local_status",
                id.0,
                "local_status",
                "id",
                "created_at",
                Some(id.0),
                None,
            ),
            TrendTarget::RemoteStatus(id) => (
                "remote_status",
                id.0,
                "remote_status",
                "id",
                "published_at",
                None,
                Some(id.0),
            ),
            TrendTarget::Tag(_) => {
                return Err(RoostyError::InvalidInput(
                    "tag is not a status trend target".to_owned(),
                ));
            }
        };
    let sql = format!(
        r#"INSERT INTO status_trend_metric (
               local_status_id, remote_status_id, favourites_count, reblogs_count, published_at
           )
           SELECT $2, $3, greatest($4, 0), greatest($5, 0), {published_column}
           FROM {status_table} WHERE {id_column} = $1
           ON CONFLICT (local_status_id, remote_status_id) DO UPDATE SET
             favourites_count = greatest(
                 status_trend_metric.favourites_count + $4, 0),
             reblogs_count = greatest(
                 status_trend_metric.reblogs_count + $5, 0),
             published_at = EXCLUDED.published_at,
             updated_at = now()"#
    );
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        sql,
        vec![
            target_id.into(),
            local_id.into(),
            remote_id.into(),
            favourite_delta.into(),
            reblog_delta.into(),
        ],
    ))
    .await?;
    mark_trend_dirty(db, kind, target_id).await
}

async fn mark_trend_dirty<C>(db: &C, kind: &str, target_id: Uuid) -> Result<()>
where
    C: ConnectionTrait,
{
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"INSERT INTO trend_dirty(kind, target_id, touched_at)
           VALUES ($1, $2, now())
           ON CONFLICT (kind, target_id)
           DO UPDATE SET touched_at = EXCLUDED.touched_at"#,
        vec![kind.to_owned().into(), target_id.into()],
    ))
    .await?;
    Ok(())
}

/// Normalize an HTTP(S) article URL for shared preview-card identity.
pub fn normalize_preview_url(value: &str) -> Option<String> {
    let mut url = Url::parse(value.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.host_str().is_none()
        || url.host_str()?.parse::<IpAddr>().is_ok()
    {
        return None;
    }
    url.set_fragment(None);
    if matches!(
        (url.scheme(), url.port()),
        ("http", Some(80)) | ("https", Some(443))
    ) {
        url.set_port(None).ok()?;
    }
    Some(url.to_string())
}

fn first_local_preview_url(content: &str) -> Option<String> {
    let mut finder = LinkFinder::new();
    finder.kinds(&[LinkKind::Url]);
    finder.url_must_have_scheme(true);
    finder
        .links(content)
        .find_map(|link| normalize_preview_url(link.as_str()))
}

fn first_remote_preview_url(content: &str) -> Option<String> {
    let document = Html::parse_fragment(content);
    let selector = Selector::parse("a[href]").ok()?;
    document
        .select(&selector)
        .filter(|element| {
            let class = element.value().attr("class").unwrap_or_default();
            let rel = element.value().attr("rel").unwrap_or_default();
            !class
                .split_ascii_whitespace()
                .any(|value| matches!(value, "mention" | "hashtag"))
                && !rel.split_ascii_whitespace().any(|value| value == "tag")
        })
        .filter_map(|element| element.value().attr("href"))
        .find_map(normalize_preview_url)
}

#[derive(Clone, Copy)]
enum PreviewStatusTarget {
    Local(StatusId),
    Remote(StatusId),
}

impl PreviewStatusTarget {
    fn ids(self) -> (Option<Uuid>, Option<Uuid>) {
        match self {
            Self::Local(id) => (Some(id.0), None),
            Self::Remote(id) => (None, Some(id.0)),
        }
    }
}

fn normalized_search_document(parts: impl IntoIterator<Item = String>) -> String {
    parts
        .into_iter()
        .flat_map(|part| {
            part.split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn remote_content_text(content: &str) -> String {
    Html::parse_fragment(content)
        .root_element()
        .text()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Rebuild one compact search document after its canonical status projection changes.
async fn refresh_status_search_document<C>(db: &C, target: StatusReference) -> Result<()>
where
    C: ConnectionTrait,
{
    let (local_status_id, remote_status_id, parts, updated_at) = match target {
        StatusReference::Local(status_id) => {
            let Some(status) = local_status::Entity::find_by_id(status_id.0)
                .one(db)
                .await?
            else {
                return Ok(());
            };
            if status.deleted_at.is_some() {
                status_search_document::Entity::delete_many()
                    .filter(status_search_document::Column::LocalStatusId.eq(status_id.0))
                    .exec(db)
                    .await?;
                return Ok(());
            }
            let media = local_media_attachment::Entity::find()
                .filter(local_media_attachment::Column::StatusId.eq(status_id.0))
                .all(db)
                .await?;
            let mut parts = vec![status.spoiler_text, status.content];
            parts.extend(media.into_iter().filter_map(|item| item.description));
            (Some(status_id.0), None, parts, status.updated_at)
        }
        StatusReference::Remote(status_id) => {
            let Some(status) = remote_status::Entity::find_by_id(status_id.0)
                .one(db)
                .await?
            else {
                return Ok(());
            };
            if status.deleted_at.is_some() {
                status_search_document::Entity::delete_many()
                    .filter(status_search_document::Column::RemoteStatusId.eq(status_id.0))
                    .exec(db)
                    .await?;
                return Ok(());
            }
            let media = remote_media_attachment::Entity::find()
                .filter(remote_media_attachment::Column::RemoteStatusId.eq(status_id.0))
                .all(db)
                .await?;
            let mut parts = vec![
                status
                    .object
                    .get("summary")
                    .and_then(JsonValue::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                remote_content_text(&status.content),
            ];
            parts.extend(media.into_iter().filter_map(|item| item.description));
            (None, Some(status_id.0), parts, status.updated_at)
        }
    };
    let mut query = status_search_document::Entity::find();
    query = match local_status_id {
        Some(id) => query.filter(status_search_document::Column::LocalStatusId.eq(id)),
        None => query.filter(status_search_document::Column::LocalStatusId.is_null()),
    };
    query = match remote_status_id {
        Some(id) => query.filter(status_search_document::Column::RemoteStatusId.eq(id)),
        None => query.filter(status_search_document::Column::RemoteStatusId.is_null()),
    };
    let document = normalized_search_document(parts);
    if let Some(existing) = query.one(db).await? {
        let mut active = existing.into_active_model();
        active.document = Set(document);
        active.updated_at = Set(updated_at);
        active.update(db).await?;
    } else {
        status_search_document::ActiveModel {
            id: Set(Uuid::now_v7()),
            local_status_id: Set(local_status_id),
            remote_status_id: Set(remote_status_id),
            document: Set(document),
            updated_at: Set(updated_at),
        }
        .insert(db)
        .await?;
    }
    Ok(())
}

/// Replace one status's preview association and coalesce scoring/fetch work atomically.
async fn replace_status_preview_card(
    txn: &DatabaseTransaction,
    target: PreviewStatusTarget,
    content: &str,
    usage_day: Date,
    actor_origin: PreviewActorOrigin,
    actor_id: Uuid,
    remote_html: bool,
) -> Result<()> {
    let (local_status_id, remote_status_id) = target.ids();
    let mut association_query = status_preview_card::Entity::find();
    association_query = match local_status_id {
        Some(id) => association_query.filter(status_preview_card::Column::LocalStatusId.eq(id)),
        None => association_query.filter(status_preview_card::Column::LocalStatusId.is_null()),
    };
    association_query = match remote_status_id {
        Some(id) => association_query.filter(status_preview_card::Column::RemoteStatusId.eq(id)),
        None => association_query.filter(status_preview_card::Column::RemoteStatusId.is_null()),
    };
    let existing = association_query.one(txn).await?;
    let url = if remote_html {
        first_remote_preview_url(content)
    } else {
        first_local_preview_url(content)
    };
    let scanned_at = OffsetDateTime::now_utc();
    let mut scan_query = status_preview_scan::Entity::find();
    scan_query = match local_status_id {
        Some(id) => scan_query.filter(status_preview_scan::Column::LocalStatusId.eq(id)),
        None => scan_query.filter(status_preview_scan::Column::LocalStatusId.is_null()),
    };
    scan_query = match remote_status_id {
        Some(id) => scan_query.filter(status_preview_scan::Column::RemoteStatusId.eq(id)),
        None => scan_query.filter(status_preview_scan::Column::RemoteStatusId.is_null()),
    };
    if let Some(scan) = scan_query.one(txn).await? {
        let mut active = scan.into_active_model();
        active.scanned_at = Set(scanned_at);
        active.update(txn).await?;
    } else {
        status_preview_scan::ActiveModel {
            id: Set(Uuid::now_v7()),
            local_status_id: Set(local_status_id),
            remote_status_id: Set(remote_status_id),
            scanned_at: Set(scanned_at),
        }
        .insert(txn)
        .await?;
    }
    if let Some(existing) = &existing {
        mark_trend_dirty(txn, "link", existing.preview_card_id).await?;
    }
    let Some(url) = url else {
        if let Some(existing) = existing {
            existing.delete(txn).await?;
        }
        return Ok(());
    };
    let card = preview_card::Entity::insert(preview_card::ActiveModel {
        id: Set(Uuid::now_v7()),
        url: Set(url),
        ..Default::default()
    })
    .on_conflict(
        OnConflict::column(preview_card::Column::Url)
            .update_column(preview_card::Column::Url)
            .to_owned(),
    )
    .exec_with_returning(txn)
    .await?;
    let card_id = card.id;
    let needs_fetch = card.fetch_state == PreviewFetchState::Pending
        || card
            .fetched_at
            .is_none_or(|fetched_at| fetched_at <= OffsetDateTime::now_utc() - Duration::days(7));
    if let Some(existing) = existing {
        let mut active = existing.into_active_model();
        active.preview_card_id = Set(card_id);
        active.usage_day = Set(usage_day);
        active.actor_origin = Set(actor_origin);
        active.actor_id = Set(actor_id);
        active.update(txn).await?;
    } else {
        status_preview_card::ActiveModel {
            id: Set(Uuid::now_v7()),
            local_status_id: Set(local_status_id),
            remote_status_id: Set(remote_status_id),
            preview_card_id: Set(card_id),
            usage_day: Set(usage_day),
            actor_origin: Set(actor_origin),
            actor_id: Set(actor_id),
            created_at: Set(OffsetDateTime::now_utc()),
        }
        .insert(txn)
        .await?;
    }
    mark_trend_dirty(txn, "link", card_id).await?;
    if needs_fetch {
        enqueue_job_in_transaction(
            txn,
            NewJob {
                kind: JobKind::PreviewCardFetch,
                payload: serde_json::json!({"preview_card_id": card_id}),
                deduplication_key: Some(format!("preview-card:{card_id}")),
                run_after: OffsetDateTime::now_utc(),
            },
        )
        .await?;
    }
    Ok(())
}

async fn remove_status_preview_card<C>(db: &C, target: PreviewStatusTarget) -> Result<()>
where
    C: ConnectionTrait,
{
    let (local_status_id, remote_status_id) = target.ids();
    let mut query = status_preview_card::Entity::find();
    query = match local_status_id {
        Some(id) => query.filter(status_preview_card::Column::LocalStatusId.eq(id)),
        None => query.filter(status_preview_card::Column::LocalStatusId.is_null()),
    };
    query = match remote_status_id {
        Some(id) => query.filter(status_preview_card::Column::RemoteStatusId.eq(id)),
        None => query.filter(status_preview_card::Column::RemoteStatusId.is_null()),
    };
    if let Some(row) = query.one(db).await? {
        mark_trend_dirty(db, "link", row.preview_card_id).await?;
        row.delete(db).await?;
    }
    Ok(())
}

/// Update compact per-actor hashtag usage inside the canonical status transaction.
async fn adjust_tag_usage<C>(
    txn: &C,
    tag_ids: &[Uuid],
    usage_day: Date,
    actor_origin: &str,
    actor_id: Uuid,
    delta: i64,
) -> Result<()>
where
    C: ConnectionTrait,
{
    let mut tag_ids = tag_ids.to_vec();
    tag_ids.sort_unstable();
    tag_ids.dedup();
    for tag_id in tag_ids {
        // Lock the aggregate first. The following statement receives a fresh
        // READ COMMITTED snapshot after any competing writer releases it.
        txn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"INSERT INTO tag_daily_usage(
                   tag_id, usage_day, uses, accounts, updated_at)
               VALUES ($1, $2, 0, 0, now())
               ON CONFLICT (tag_id, usage_day)
               DO UPDATE SET updated_at = tag_daily_usage.updated_at"#,
            vec![tag_id.into(), usage_day.into()],
        ))
        .await?;
        let statement = if delta > 0 {
            Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                r#"INSERT INTO tag_daily_actor_usage(
                       tag_id, usage_day, actor_origin, actor_id, uses)
                   VALUES ($1, $2, $3, $4, $5)
                   ON CONFLICT (tag_id, usage_day, actor_origin, actor_id)
                   DO UPDATE SET uses = tag_daily_actor_usage.uses + EXCLUDED.uses"#,
                vec![
                    tag_id.into(),
                    usage_day.into(),
                    actor_origin.to_owned().into(),
                    actor_id.into(),
                    delta.into(),
                ],
            )
        } else {
            Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                r#"WITH reduced AS (
                       UPDATE tag_daily_actor_usage SET uses = uses - 1
                       WHERE tag_id = $1 AND usage_day = $2 AND actor_origin = $3
                         AND actor_id = $4 AND uses > 1
                       RETURNING 1
                   )
                   DELETE FROM tag_daily_actor_usage
                   WHERE tag_id = $1 AND usage_day = $2 AND actor_origin = $3
                     AND actor_id = $4 AND uses = 1
                     AND NOT EXISTS (SELECT 1 FROM reduced)"#,
                vec![
                    tag_id.into(),
                    usage_day.into(),
                    actor_origin.to_owned().into(),
                    actor_id.into(),
                ],
            )
        };
        txn.execute(statement).await?;
        txn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"INSERT INTO tag_daily_usage(
                   tag_id, usage_day, uses, accounts, updated_at)
               SELECT $1, $2, coalesce(sum(uses), 0), count(*), now()
               FROM tag_daily_actor_usage
               WHERE tag_id = $1 AND usage_day = $2
               ON CONFLICT (tag_id, usage_day) DO UPDATE SET
                 uses = EXCLUDED.uses, accounts = EXCLUDED.accounts,
                 updated_at = EXCLUDED.updated_at"#,
            vec![tag_id.into(), usage_day.into()],
        ))
        .await?;
        txn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"DELETE FROM tag_daily_usage
               WHERE tag_id = $1 AND usage_day = $2
                 AND uses = 0 AND accounts = 0"#,
            vec![tag_id.into(), usage_day.into()],
        ))
        .await?;
        mark_trend_dirty(txn, "tag", tag_id).await?;
    }
    Ok(())
}

fn utc_date(timestamp: OffsetDateTime) -> time::Date {
    timestamp.to_offset(time::UtcOffset::UTC).date()
}

async fn mark_account_status_trends_dirty<C>(
    db: &C,
    origin: &str,
    account_id: AccountId,
) -> Result<()>
where
    C: ConnectionTrait,
{
    let (kind, table, account_column) = match origin {
        "local" => ("local_status", "local_status", "account_id"),
        "remote" => ("remote_status", "remote_status", "remote_actor_id"),
        _ => {
            return Err(RoostyError::InvalidInput(
                "invalid trend account origin".to_owned(),
            ));
        }
    };
    let sql = format!(
        r#"INSERT INTO trend_dirty(kind, target_id, touched_at)
           SELECT '{kind}', id, now() FROM {table} WHERE {account_column} = $1
           ON CONFLICT (kind, target_id)
           DO UPDATE SET touched_at = EXCLUDED.touched_at"#
    );
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        sql,
        vec![account_id.0.into()],
    ))
    .await?;
    let status_column = if origin == "local" {
        "local_status_id"
    } else {
        "remote_status_id"
    };
    let link_sql = format!(
        r#"INSERT INTO trend_dirty(kind, target_id, touched_at)
           SELECT 'link', association.preview_card_id, now()
           FROM status_preview_card association
           JOIN {table} status ON status.id = association.{status_column}
           WHERE status.{account_column} = $1
           ON CONFLICT (kind, target_id)
           DO UPDATE SET touched_at = EXCLUDED.touched_at"#
    );
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        link_sql,
        vec![account_id.0.into()],
    ))
    .await?;
    Ok(())
}

async fn mark_remote_actor_interactions_dirty<C>(db: &C, actor_id: AccountId) -> Result<()>
where
    C: ConnectionTrait,
{
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"INSERT INTO trend_dirty(kind, target_id, touched_at)
           SELECT kind, target_id, now() FROM (
             SELECT 'local_status' kind, local_status_id target_id
             FROM remote_status_favourite WHERE remote_actor_id = $1
             UNION
             SELECT CASE WHEN local_status_id IS NULL
                         THEN 'remote_status' ELSE 'local_status' END,
                    coalesce(local_status_id, remote_status_id)
             FROM remote_status_reblog WHERE remote_actor_id = $1
           ) targets
           ON CONFLICT (kind, target_id)
           DO UPDATE SET touched_at = EXCLUDED.touched_at"#,
        vec![actor_id.0.into()],
    ))
    .await?;
    Ok(())
}

#[derive(Debug, FromQueryResult)]
struct TrendingTagRow {
    id: Uuid,
    name: String,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
    day: i64,
    uses: i64,
    accounts: i64,
}

/// A featured hashtag projected with account-scoped visible-status statistics.
#[derive(Clone, Debug, FromQueryResult)]
pub struct FeaturedTag {
    /// Identifier of the featured relationship, not the shared tag.
    pub id: Uuid,
    /// Normalized tag name without `#`.
    pub name: String,
    /// Remote profile link, when supplied by a remote actor.
    pub href: Option<String>,
    /// Number of locally known public or unlisted statuses by this account.
    pub statuses_count: i64,
    /// Most recent locally known use of the tag.
    pub last_status_at: Option<OffsetDateTime>,
    /// Relationship creation timestamp.
    pub created_at: OffsetDateTime,
}

/// Result of an idempotent local featured-tag creation attempt.
#[derive(Clone, Debug)]
pub enum FeatureTagResult {
    /// The relationship was created or already existed.
    Featured {
        /// Current relationship projection.
        tag: FeaturedTag,
        /// Whether this call created the relationship.
        created: bool,
    },
    /// The local account has reached its configured maximum.
    LimitReached,
}

/// Validated remote featured-tag data ready for atomic reconciliation.
#[derive(Clone, Debug)]
pub struct RemoteFeaturedTagInput {
    /// Normalized name without the leading `#`.
    pub name: String,
    /// Original display spelling received from the remote actor.
    pub display_name: String,
    /// Validated public profile hashtag link.
    pub href: String,
}

/// Data accepted when creating a local status.
#[derive(Clone, Debug)]
pub struct NewLocalStatus {
    /// Stable identifier, supplied for retry-safe scheduled publication.
    pub id: Option<StatusId>,
    /// Authoring local account identifier.
    pub account_id: AccountId,
    /// Plain text status content.
    pub content: String,
    /// Mastodon status visibility value.
    pub visibility: StatusVisibility,
    /// Whether the status is marked sensitive.
    pub sensitive: bool,
    /// Optional content warning text.
    pub spoiler_text: String,
    /// Optional BCP-47 language tag.
    pub language: Option<String>,
    /// Optional local status this status replies to.
    pub in_reply_to_id: Option<StatusId>,
    /// Optional cached remote parent status.
    pub in_reply_to_remote_status_id: Option<StatusId>,
    /// Per-status quote consent policy.
    pub quote_approval_policy: QuoteApprovalPolicy,
}

/// Stored posting intent waiting for durable publication.
#[derive(Clone, Debug)]
pub struct ScheduledStatus {
    pub id: Uuid,
    pub account_id: AccountId,
    pub publication_status_id: StatusId,
    pub content: String,
    pub visibility: StatusVisibility,
    pub sensitive: bool,
    pub spoiler_text: String,
    pub language: Option<String>,
    pub in_reply_to_id: Option<StatusId>,
    pub in_reply_to_remote_status_id: Option<StatusId>,
    pub quoted_status_id: Option<StatusId>,
    pub quote_approval_policy: QuoteApprovalPolicy,
    pub scheduled_at: OffsetDateTime,
}

/// Validated scheduled posting intent and reserved attachment order.
#[derive(Clone, Debug)]
pub struct NewScheduledStatus {
    pub id: Option<Uuid>,
    pub account_id: AccountId,
    pub content: String,
    pub visibility: StatusVisibility,
    pub sensitive: bool,
    pub spoiler_text: String,
    pub language: Option<String>,
    pub in_reply_to_id: Option<StatusId>,
    pub in_reply_to_remote_status_id: Option<StatusId>,
    pub quoted_status_id: Option<StatusId>,
    pub quote_approval_policy: QuoteApprovalPolicy,
    pub scheduled_at: OffsetDateTime,
    pub media_ids: Vec<Uuid>,
    pub poll: Option<NewStatusPoll>,
}

/// Existing result protected by an account-scoped status idempotency key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExistingStatusCreation {
    Status(StatusId),
    ScheduledStatus(Uuid),
}

/// Persisted result variants for status-creation idempotency.
#[derive(
    Clone, Copy, Debug, DeriveValueType, Display, EnumString, Eq, IntoStaticStr, PartialEq,
)]
#[sea_orm(value_type = "String")]
#[strum(serialize_all = "snake_case")]
pub enum StatusCreationResultKind {
    Status,
    ScheduledStatus,
}

/// PostgreSQL transaction retaining an advisory lock until a creation result is recorded.
pub struct StatusCreationGuard {
    txn: DatabaseTransaction,
    account_id: AccountId,
    key_hash: String,
}

/// Result of acquiring a status-creation idempotency key.
pub enum StatusCreationReservation {
    New(StatusCreationGuard),
    Existing(ExistingStatusCreation),
}

impl StatusCreationGuard {
    /// Record the successful result and release the cross-process advisory lock.
    pub async fn complete(self, result: ExistingStatusCreation) -> Result<()> {
        let (kind, id) = match result {
            ExistingStatusCreation::Status(id) => (StatusCreationResultKind::Status, id.0),
            ExistingStatusCreation::ScheduledStatus(id) => {
                (StatusCreationResultKind::ScheduledStatus, id)
            }
        };
        status_creation_idempotency::ActiveModel {
            account_id: Set(self.account_id.0),
            key_hash: Set(self.key_hash),
            result_kind: Set(kind),
            result_id: Set(id),
            expires_at: Set(OffsetDateTime::now_utc() + Duration::hours(1)),
            created_at: Set(OffsetDateTime::now_utc()),
        }
        .insert(&self.txn)
        .await?;
        self.txn.commit().await?;
        Ok(())
    }
}

/// Serialize status creation for an account/key pair across all server processes.
pub async fn begin_status_creation(
    db: &DbConnection,
    account_id: AccountId,
    key: &str,
) -> Result<StatusCreationReservation> {
    let key_hash = URL_SAFE_NO_PAD.encode(Sha256::digest(key.as_bytes()));
    let txn = db.begin().await?;
    txn.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        vec![format!("{}:{key_hash}", account_id.0).into()],
    ))
    .await?;
    status_creation_idempotency::Entity::delete_many()
        .filter(status_creation_idempotency::Column::AccountId.eq(account_id.0))
        .filter(status_creation_idempotency::Column::KeyHash.eq(&key_hash))
        .filter(status_creation_idempotency::Column::ExpiresAt.lt(OffsetDateTime::now_utc()))
        .exec(&txn)
        .await?;
    let existing =
        status_creation_idempotency::Entity::find_by_id((account_id.0, key_hash.clone()))
            .one(&txn)
            .await?;
    if let Some(existing) = existing {
        let result = match existing.result_kind {
            StatusCreationResultKind::Status => {
                ExistingStatusCreation::Status(StatusId(existing.result_id))
            }
            StatusCreationResultKind::ScheduledStatus => {
                ExistingStatusCreation::ScheduledStatus(existing.result_id)
            }
        };
        txn.rollback().await?;
        return Ok(StatusCreationReservation::Existing(result));
    }
    Ok(StatusCreationReservation::New(StatusCreationGuard {
        txn,
        account_id,
        key_hash,
    }))
}

/// Outcome when an account's configurable scheduling capacity is checked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScheduleStatusResult {
    Created,
    TotalLimitReached,
    DailyLimitReached,
}

/// Stored local direct-message conversation.
#[derive(Clone, Debug)]
pub struct LocalConversation {
    /// Internal conversation identifier.
    pub id: Uuid,
    /// Most recent status in the conversation, when still available.
    pub last_status_id: Option<StatusId>,
    /// Most recent cached remote status in the conversation, when applicable.
    pub last_remote_status_id: Option<StatusId>,
    /// Creation timestamp.
    pub created_at: OffsetDateTime,
    /// Last update timestamp.
    pub updated_at: OffsetDateTime,
}

/// A local account's view of one direct-message conversation.
#[derive(Clone, Debug)]
pub struct LocalConversationAccount {
    /// Per-account conversation identifier exposed through Mastodon APIs.
    pub id: Uuid,
    /// Cursor identifier used for conversation pagination.
    pub cursor_id: Uuid,
    /// Shared local conversation identifier.
    pub conversation_id: Uuid,
    /// Local account that owns this conversation view.
    pub account_id: AccountId,
    /// Whether the conversation has unread activity for this account.
    pub unread: bool,
    /// Soft-hide timestamp for this account's conversation view.
    pub hidden_at: Option<OffsetDateTime>,
    /// Most recent visible local direct status for this account.
    pub last_status_id: Option<StatusId>,
    /// Most recent visible remote direct status for this account.
    pub last_remote_status_id: Option<StatusId>,
    /// Creation timestamp.
    pub created_at: OffsetDateTime,
    /// Last update timestamp.
    pub updated_at: OffsetDateTime,
}

/// Local conversation row with the authenticated account's view state.
#[derive(Clone, Debug)]
pub struct LocalConversationView {
    /// Shared conversation row.
    pub conversation: LocalConversation,
    /// Authenticated account's conversation row.
    pub account: LocalConversationAccount,
}

/// Recipient views changed while refreshing a direct conversation after an edit or deletion.
#[derive(Clone, Debug)]
pub struct DirectConversationRefresh {
    /// Shared conversation whose recipient views were refreshed.
    pub conversation_id: Uuid,
    /// Accounts whose remaining view points at a different latest status.
    pub updated_account_ids: Vec<AccountId>,
    /// Accounts whose view no longer has any visible status and was removed.
    pub removed_account_ids: Vec<AccountId>,
}

/// One status-like streaming projection removed by federation delete repair.
#[derive(Clone, Debug)]
pub struct DeleteStreamProjection {
    pub status_id: String,
    pub actor_id: AccountId,
    pub home_recipient_ids: Vec<AccountId>,
    pub direct_recipient_ids: Vec<AccountId>,
    pub visibility: StatusVisibility,
    pub status_origin: StreamingStatusOrigin,
    pub has_media: bool,
}

/// Durable state repaired by one signed remote status or actor deletion.
#[derive(Clone, Debug, Default)]
pub struct RemoteDeleteRepair {
    pub projections: Vec<DeleteStreamProjection>,
    pub conversation_refreshes: Vec<DirectConversationRefresh>,
    pub deleted_status_count: usize,
}

/// A remote participant retained for a direct conversation.
#[derive(Clone, Debug)]
pub struct RemoteConversationParticipant {
    /// Canonical actor ID, retained even when the actor cache entry is unavailable.
    pub activitypub_id: String,
    /// Cached remote actor, when known locally.
    pub remote_actor_id: Option<AccountId>,
    /// Mention text declared by the originating Note.
    pub mention_name: Option<String>,
}

/// Exact audience projected for one account's visible direct status.
#[derive(Clone, Debug, Default)]
pub struct DirectStatusParticipants {
    pub local_accounts: Vec<LocalAccount>,
    pub remote_accounts: Vec<RemoteConversationParticipant>,
}

/// Mutable local status fields accepted by Mastodon status edit APIs.
#[derive(Clone, Debug, Default)]
pub struct LocalStatusUpdate {
    /// Optional replacement plain text content.
    pub content: Option<String>,
    /// Optional replacement sensitivity flag.
    pub sensitive: Option<bool>,
    /// Optional replacement content warning text.
    pub spoiler_text: Option<String>,
    /// Optional replacement language tag.
    pub language: Option<Option<String>>,
    /// Replacement poll state; absence removes an existing poll.
    pub poll: Option<NewStatusPoll>,
}

/// Stored local media attachment metadata.
#[derive(Clone, Debug)]
pub struct LocalMediaAttachment {
    /// Internal media identifier exposed through Mastodon media APIs.
    pub id: Uuid,
    /// Local account that uploaded the media.
    pub account_id: AccountId,
    /// Local status this media is attached to, when already posted.
    pub status_id: Option<StatusId>,
    /// Scheduled status reserving this media before publication.
    pub scheduled_status_id: Option<Uuid>,
    /// Position of this attachment on the status.
    pub status_order: i32,
    /// Original uploaded MIME type.
    pub content_type: String,
    /// Original filename supplied by the client.
    pub original_filename: String,
    /// Path relative to the configured media root.
    pub file_path: String,
    /// Preview path relative to the configured media root.
    pub preview_file_path: Option<String>,
    /// Stored file size in bytes.
    pub file_size: i64,
    /// Optional accessible media description.
    pub description: Option<String>,
    /// Optional horizontal focal point.
    pub focus_x: Option<f64>,
    /// Optional vertical focal point.
    pub focus_y: Option<f64>,
    /// Optional image width.
    pub width: Option<i32>,
    /// Optional image height.
    pub height: Option<i32>,
    /// Optional preview image width.
    pub preview_width: Option<i32>,
    /// Optional preview image height.
    pub preview_height: Option<i32>,
    /// Optional blurhash generated from the preview image.
    pub blurhash: Option<String>,
}

/// New local media metadata ready to persist after storing the file.
#[derive(Clone, Debug)]
pub struct NewLocalMediaAttachment {
    /// Local account that uploaded the media.
    pub account_id: AccountId,
    /// Original uploaded MIME type.
    pub content_type: String,
    /// Original filename supplied by the client.
    pub original_filename: String,
    /// Path relative to the configured media root.
    pub file_path: String,
    /// Preview path relative to the configured media root.
    pub preview_file_path: Option<String>,
    /// Stored file size in bytes.
    pub file_size: i64,
    /// Optional accessible media description.
    pub description: Option<String>,
    /// Optional horizontal focal point.
    pub focus_x: Option<f64>,
    /// Optional vertical focal point.
    pub focus_y: Option<f64>,
    /// Optional image width.
    pub width: Option<i32>,
    /// Optional image height.
    pub height: Option<i32>,
    /// Optional preview image width.
    pub preview_width: Option<i32>,
    /// Optional preview image height.
    pub preview_height: Option<i32>,
    /// Optional blurhash generated from the preview image.
    pub blurhash: Option<String>,
}

/// Mutable media fields accepted before media is attached to a status.
#[derive(Clone, Debug, Default)]
pub struct LocalMediaAttachmentUpdate {
    /// Optional accessible media description.
    pub description: Option<Option<String>>,
    /// Optional focal point update.
    pub focus: Option<(f64, f64)>,
    /// Optional replacement preview metadata.
    pub preview: Option<LocalMediaPreviewUpdate>,
}

/// Replacement preview metadata for an unattached media attachment.
#[derive(Clone, Debug)]
pub struct LocalMediaPreviewUpdate {
    /// Preview path relative to the configured media root.
    pub preview_file_path: String,
    /// Preview image width.
    pub preview_width: i32,
    /// Preview image height.
    pub preview_height: i32,
    /// Blurhash generated from the preview image.
    pub blurhash: String,
}

/// Mutable media metadata accepted while editing an owned local status.
#[derive(Clone, Debug)]
pub struct LocalStatusMediaAttributeUpdate {
    /// Media attachment identifier.
    pub media_id: Uuid,
    /// Optional replacement accessible media description.
    pub description: Option<Option<String>>,
    /// Optional replacement focal point.
    pub focus: Option<(f64, f64)>,
}

/// Cursor filters accepted by local timeline queries.
#[derive(Clone, Copy, Debug, Default)]
pub struct TimelineCursor {
    /// Return statuses older than this id.
    pub max_id: Option<StatusId>,
    /// Return statuses newer than this id.
    pub since_id: Option<StatusId>,
    /// Return statuses immediately newer than this id.
    pub min_id: Option<StatusId>,
}

/// Cursor filters accepted by Mastodon collection queries.
#[derive(Clone, Copy, Debug, Default)]
pub struct CollectionCursor {
    /// Return collection rows older than this internal id.
    pub max_id: Option<Uuid>,
    /// Return collection rows newer than this internal id.
    pub since_id: Option<Uuid>,
    /// Return collection rows immediately newer than this internal id.
    pub min_id: Option<Uuid>,
}

/// Page of Mastodon collection items and opaque cursor metadata.
#[derive(Clone, Debug)]
pub struct CollectionPage<T> {
    /// Items returned to the API caller.
    pub items: Vec<T>,
    /// Cursor for the first row in the page.
    pub first_cursor: Option<Uuid>,
    /// Cursor for the last row in the page.
    pub last_cursor: Option<Uuid>,
    /// Whether one more row was found past the requested limit.
    pub has_more: bool,
}

/// Page of Mastodon timeline items and UUID cursor metadata.
#[derive(Clone, Debug)]
pub struct TimelinePage<T> {
    /// Items returned to the API caller.
    pub items: Vec<T>,
    /// Cursor for the first row in the page.
    pub first_cursor: Option<Uuid>,
    /// Cursor for the last row in the page.
    pub last_cursor: Option<Uuid>,
    /// Whether one more row was found past the requested limit.
    pub has_more: bool,
}

/// Result of an idempotent local status pin attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PinStatusResult {
    /// A new pin was stored.
    Pinned,
    /// The status was already pinned by its author.
    AlreadyPinned,
    /// An existing pin was removed.
    Unpinned,
    /// The owned status was not pinned.
    AlreadyUnpinned,
    /// No active local status has this identifier.
    NotFound,
    /// The authenticated account does not own the status.
    NotOwned,
    /// Only public and unlisted statuses can currently be pinned.
    UnsupportedVisibility,
    /// The account already has the configured maximum number of pins.
    LimitReached,
}

/// Filters supported by Mastodon account status timeline requests.
#[derive(Clone, Debug, Default)]
pub struct AccountStatusTimelineOptions {
    /// Exclude statuses that reply to another local status.
    pub exclude_replies: bool,
    /// Return only statuses with at least one media attachment.
    pub only_media: bool,
    /// Return only statuses carrying the normalized hashtag.
    pub tagged: Option<String>,
}

/// Filters supported by Mastodon's mixed hashtag timeline request.
#[derive(Clone, Debug, Default)]
pub struct TagTimelineOptions {
    /// Return statuses that include at least one of these additional tags.
    pub any: Vec<String>,
    /// Return statuses that include every one of these additional tags.
    pub all: Vec<String>,
    /// Exclude statuses that include any of these tags.
    pub none: Vec<String>,
    /// Return only statuses with at least one media attachment.
    pub only_media: bool,
    /// Restrict results to local, remote, or both origins.
    pub origin: PublicTimelineOrigin,
    /// Optional authenticated viewer used for moderation filtering.
    pub viewer: Option<AccountId>,
    /// Remote domains eligible for cached public projection.
    pub allowed_remote_domains: Vec<String>,
    /// Remote domains excluded from cached public projection.
    pub blocked_remote_domains: Vec<String>,
}

/// Supported local Mastodon notification kinds.
#[derive(
    Clone,
    Copy,
    Debug,
    DeriveValueType,
    Display,
    EnumString,
    Eq,
    IntoStaticStr,
    PartialEq,
    Serialize,
    Deserialize,
)]
#[sea_orm(value_type = "String")]
#[strum(serialize_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum LocalNotificationType {
    Mention,
    Favourite,
    Follow,
    FollowRequest,
    Reblog,
    Status,
    Update,
    Quote,
    QuotedUpdate,
    Poll,
    #[strum(serialize = "admin.report")]
    #[serde(rename = "admin.report")]
    AdminReport,
}

/// Stored local boost relationship between an account and a status.
#[derive(Clone, Debug)]
pub struct LocalStatusReblog {
    /// Opaque boost identifier used as the Mastodon status id for boost entries.
    pub id: Uuid,
    /// Account that boosted the status.
    pub account_id: AccountId,
    /// Status that was boosted.
    pub status_id: StatusId,
    /// Creation timestamp for the boost.
    pub created_at: OffsetDateTime,
}

/// Stored local boost of a cached remote Note, including its ActivityPub identity.
#[derive(Clone, Debug)]
pub struct LocalRemoteStatusReblog {
    /// Opaque boost identifier used as the Mastodon status id for boost entries.
    pub id: Uuid,
    /// Account that boosted the cached Note.
    pub local_account_id: AccountId,
    /// Cached Note that was boosted.
    pub remote_status_id: StatusId,
    /// Canonical locally authored Announce activity ID.
    pub activity_id: String,
    /// Creation timestamp for the boost.
    pub created_at: OffsetDateTime,
}

/// Target of an inbound remote Announce activity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteStatusReblogTarget {
    /// A local status was boosted.
    Local(StatusId),
    /// A cached remote Note was boosted.
    Remote(StatusId),
}

/// Stored remote Announce activity.
#[derive(Clone, Debug)]
pub struct RemoteStatusReblog {
    /// Opaque local identifier for the timeline boost entry.
    pub id: Uuid,
    /// Remote actor that announced the status.
    pub remote_actor_id: AccountId,
    /// Local or cached remote target of the Announce.
    pub target: RemoteStatusReblogTarget,
    /// Canonical remote Announce activity ID.
    pub activity_id: String,
    /// Creation timestamp for the boost.
    pub created_at: OffsetDateTime,
}

/// A home timeline row, either an authored status or a boost entry.
#[derive(Clone, Debug)]
pub enum HomeTimelineItem {
    /// Authored local status.
    Status(LocalStatus),
    /// Local boost of an authored status.
    Reblog(LocalStatusReblog),
    /// Cached status from an accepted remote follow.
    RemoteStatus(RemoteStatus),
    /// Local boost of a cached remote status.
    LocalRemoteReblog(LocalRemoteStatusReblog),
    /// Cached remote actor's boost of a local or cached remote status.
    RemoteReblog(RemoteStatusReblog),
}

/// A status displayed in the federated public timeline.
#[derive(Clone, Debug)]
pub enum PublicTimelineItem {
    /// A status authored on this instance.
    Local(LocalStatus),
    /// A public status cached from another instance.
    Remote(RemoteStatus),
}

/// Origin filter for the Mastodon-compatible public timeline.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PublicTimelineOrigin {
    /// Include both local and cached remote statuses.
    #[default]
    Federated,
    /// Include only statuses authored on this instance.
    Local,
    /// Include only cached remote statuses.
    Remote,
}

/// Filters applied while reading the public timeline.
#[derive(Clone, Debug, Default)]
pub struct PublicTimelineOptions {
    pub origin: PublicTimelineOrigin,
    pub only_media: bool,
    pub viewer: Option<AccountId>,
    pub allowed_remote_domains: Vec<String>,
    pub blocked_remote_domains: Vec<String>,
}

/// Local or cached-remote status whose interaction actors are being listed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatusInteractionTarget {
    /// Status authored by a local account.
    Local(StatusId),
    /// Status cached from a remote actor.
    Remote(StatusId),
}

/// Local or remote actor that interacted with a status.
#[derive(Clone, Debug)]
pub enum StatusInteractionAccount {
    /// Local interaction actor.
    Local(LocalAccount),
    /// Remote interaction actor.
    Remote(RemoteActor),
}

/// A local or cached remote status returned from the authenticated favourites collection.
#[derive(Clone, Debug)]
pub enum FavouriteStatus {
    /// A locally authored status.
    Local(LocalStatus),
    /// A cached remote Note.
    Remote(RemoteStatus),
}

/// Stored favourite of a cached remote Note, including its outbound activity identity.
#[derive(Clone, Debug)]
pub struct LocalRemoteStatusFavourite {
    /// Local account that favourited the cached Note.
    pub local_account_id: AccountId,
    /// Cached Note that was favourited.
    pub remote_status_id: StatusId,
    /// Canonical locally authored Like activity ID.
    pub activity_id: String,
}

impl LocalNotificationType {
    /// Return the Mastodon wire value for this notification type.
    pub fn as_str(self) -> &'static str {
        self.into()
    }
}

/// Stored local notification event.
#[derive(Clone, Debug)]
pub struct LocalNotification {
    /// Opaque Mastodon notification identifier.
    pub id: Uuid,
    /// Account receiving the notification.
    pub account_id: AccountId,
    /// Mastodon notification type.
    pub notification_type: LocalNotificationType,
    /// Account that caused the notification.
    pub actor_account_id: Option<AccountId>,
    /// Optional remote actor that caused the notification.
    pub remote_actor_id: Option<AccountId>,
    /// Related local status for mention and favourite notifications.
    pub status_id: Option<StatusId>,
    /// Related cached remote status for a remote mention notification.
    pub remote_status_id: Option<StatusId>,
    /// Related moderation report for `admin.report` notifications.
    pub report_id: Option<Uuid>,
    /// Persisted rolling group identity for groupable notification types.
    pub group_id: Option<Uuid>,
    /// Whether the recipient's policy hid this notification.
    pub filtered: bool,
    /// Sender-scoped request collecting this filtered notification.
    pub notification_request_id: Option<Uuid>,
    /// Creation timestamp.
    pub created_at: OffsetDateTime,
    /// Soft-dismiss timestamp.
    pub dismissed_at: Option<OffsetDateTime>,
}

/// Persisted Mastodon notification policy for one local account.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct NotificationPolicy {
    pub for_not_following: NotificationPolicyAction,
    pub for_not_followers: NotificationPolicyAction,
    pub for_new_accounts: NotificationPolicyAction,
    pub for_private_mentions: NotificationPolicyAction,
    pub for_limited_accounts: NotificationPolicyAction,
}

/// Partial policy update accepted by Mastodon's PATCH endpoint.
#[derive(Clone, Copy, Debug, Default)]
pub struct NotificationPolicyUpdate {
    pub for_not_following: Option<NotificationPolicyAction>,
    pub for_not_followers: Option<NotificationPolicyAction>,
    pub for_new_accounts: Option<NotificationPolicyAction>,
    pub for_private_mentions: Option<NotificationPolicyAction>,
    pub for_limited_accounts: Option<NotificationPolicyAction>,
}

/// Local or cached-remote sender of a notification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotificationActor {
    Local(AccountId),
    Remote(AccountId),
}

/// One sender-scoped request returned by the Mastodon notification API.
#[derive(Clone, Debug)]
pub struct NotificationRequest {
    pub id: Uuid,
    pub account_id: AccountId,
    pub actor: NotificationActor,
    pub last_status_id: Option<StatusId>,
    pub last_remote_status_id: Option<StatusId>,
    pub notifications_count: u64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(FromQueryResult)]
struct NotificationRequestRow {
    id: Uuid,
    account_id: Uuid,
    actor_account_id: Option<Uuid>,
    remote_actor_id: Option<Uuid>,
    last_status_id: Option<Uuid>,
    last_remote_status_id: Option<Uuid>,
    notifications_count: i64,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

impl LocalNotification {
    /// Return the opaque Mastodon group key for individual and streaming projections.
    pub fn group_key(&self) -> String {
        self.group_id.map_or_else(
            || format!("ungrouped-{}", self.id),
            |group_id| format!("{}-{group_id}", self.notification_type.as_str()),
        )
    }
}

/// Persisted inbound remote follow request or accepted remote follower.
#[derive(Clone, Debug)]
pub struct RemoteFollow {
    pub id: Uuid,
    pub remote_actor_id: AccountId,
    pub local_account_id: AccountId,
    pub activity_id: String,
    pub activity: JsonValue,
    pub state: RemoteFollowState,
}

/// Durable delivery work to create together with an automatically accepted Follow.
#[derive(Clone, Debug)]
pub struct RemoteFollowResponseJob {
    /// Worker job kind.
    pub kind: JobKind,
    /// Serialized delivery payload.
    pub payload: JsonValue,
    /// Active-job deduplication key.
    pub deduplication_key: String,
}

/// Known durable job kinds dispatched by Roosty's worker.
#[derive(
    Clone, Copy, Debug, DeriveValueType, Display, EnumString, Eq, IntoStaticStr, PartialEq,
)]
#[sea_orm(value_type = "String")]
#[strum(serialize_all = "snake_case")]
pub enum JobKind {
    FederationFollowResponse,
    FederationStatusDelivery,
    FederationQuoteDelivery,
    FederationFollowDelivery,
    FederationFavouriteDelivery,
    FederationReblogDelivery,
    FederationActorUpdateDelivery,
    FederationModerationDelivery,
    FederationRemoteMediaFetch,
    FederationFeaturedRefresh,
    FederationFeaturedTagsRefresh,
    FederationThreadResolve,
    FederationRepliesFetch,
    FederationReplyFetch,
    WebPushDelivery,
    NotificationRequestMerge,
    NotificationRequestCleanup,
    AccountPurge,
    DomainModerationReconcile,
    ScheduledStatusPublish,
    PollExpiration,
    PollUpdate,
    FederationPollVoteDelivery,
    TrendMaintenance,
    PreviewCardFetch,
    PreviewCardBackfill,
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        self.into()
    }
}

/// State shared by inbound and outbound remote follow relationships.
#[derive(
    Clone, Copy, Debug, DeriveValueType, Display, EnumString, Eq, IntoStaticStr, PartialEq,
)]
#[sea_orm(value_type = "String")]
#[strum(serialize_all = "snake_case")]
pub enum RemoteFollowState {
    Pending,
    Accepted,
}

#[derive(FromQueryResult)]
struct RemoteFollowRow {
    id: Uuid,
    remote_actor_id: Uuid,
    local_account_id: Uuid,
    activity_id: String,
    activity: JsonValue,
    state: RemoteFollowState,
}

/// Store or refresh an inbound remote Follow request.
pub async fn upsert_remote_follow(
    db: &DbConnection,
    remote_actor_id: AccountId,
    local_account_id: AccountId,
    activity_id: &str,
    activity: JsonValue,
    state: RemoteFollowState,
) -> Result<RemoteFollow> {
    let row = RemoteFollowRow::find_by_statement(Statement::from_sql_and_values(DatabaseBackend::Postgres, r#"
        INSERT INTO remote_follow (id, remote_actor_id, local_account_id, activity_id, activity, state)
        VALUES ($1, $2, $3, $4, $5, $6)
        ON CONFLICT (remote_actor_id, local_account_id) DO UPDATE
        SET activity_id = EXCLUDED.activity_id, activity = EXCLUDED.activity, state = EXCLUDED.state, updated_at = now()
        RETURNING id, remote_actor_id, local_account_id, activity_id, activity, state
    "#, vec![Uuid::now_v7().into(), remote_actor_id.0.into(), local_account_id.0.into(), activity_id.to_owned().into(), activity.into(), state.into()])).one(db).await?
        .ok_or_else(|| RoostyError::InvalidInput("remote follow could not be saved".to_owned()))?;
    Ok(remote_follow_from_row(row))
}

/// Persist one newly validated automatic remote Follow and its durable Accept job atomically.
pub async fn upsert_processed_remote_follow_with_response_job(
    txn: &sea_orm::DatabaseTransaction,
    remote_actor_id: AccountId,
    local_account_id: AccountId,
    activity_id: &str,
    activity: JsonValue,
    response_job: RemoteFollowResponseJob,
) -> Result<bool> {
    lock_local_remote_relation(txn, local_account_id, remote_actor_id).await?;
    if local_remote_accounts_are_blocked(txn, local_account_id, remote_actor_id).await? {
        return Ok(false);
    }
    remote_follow::Entity::insert(remote_follow::ActiveModel {
        id: Set(Uuid::now_v7()),
        remote_actor_id: Set(remote_actor_id.0),
        local_account_id: Set(local_account_id.0),
        activity_id: Set(activity_id.to_owned()),
        activity: Set(activity),
        state: Set(RemoteFollowState::Accepted),
        created_at: Set(OffsetDateTime::now_utc()),
        updated_at: Set(OffsetDateTime::now_utc()),
    })
    .on_conflict(
        OnConflict::columns([
            remote_follow::Column::RemoteActorId,
            remote_follow::Column::LocalAccountId,
        ])
        .update_columns([
            remote_follow::Column::ActivityId,
            remote_follow::Column::Activity,
            remote_follow::Column::State,
            remote_follow::Column::UpdatedAt,
        ])
        .to_owned(),
    )
    .exec(txn)
    .await?;
    insert_response_job(txn, response_job).await?;
    Ok(true)
}

/// Persist an inbox idempotency marker and a pending remote Follow together.
pub async fn upsert_processed_pending_remote_follow(
    txn: &sea_orm::DatabaseTransaction,
    remote_actor_id: AccountId,
    local_account_id: AccountId,
    activity_id: &str,
    activity: JsonValue,
) -> Result<bool> {
    lock_local_remote_relation(txn, local_account_id, remote_actor_id).await?;
    if local_remote_accounts_are_blocked(txn, local_account_id, remote_actor_id).await? {
        return Ok(false);
    }
    remote_follow::Entity::insert(remote_follow::ActiveModel {
        id: Set(Uuid::now_v7()),
        remote_actor_id: Set(remote_actor_id.0),
        local_account_id: Set(local_account_id.0),
        activity_id: Set(activity_id.to_owned()),
        activity: Set(activity),
        state: Set(RemoteFollowState::Pending),
        created_at: Set(OffsetDateTime::now_utc()),
        updated_at: Set(OffsetDateTime::now_utc()),
    })
    .on_conflict(
        OnConflict::columns([
            remote_follow::Column::RemoteActorId,
            remote_follow::Column::LocalAccountId,
        ])
        .update_columns([
            remote_follow::Column::ActivityId,
            remote_follow::Column::Activity,
            remote_follow::Column::State,
            remote_follow::Column::UpdatedAt,
        ])
        .to_owned(),
    )
    .exec(txn)
    .await?;
    Ok(true)
}

/// Insert a deduplicated follow-response job within a caller-owned transaction.
async fn insert_response_job(
    txn: &sea_orm::DatabaseTransaction,
    response_job: RemoteFollowResponseJob,
) -> Result<()> {
    let _ = job::Entity::insert(job::ActiveModel {
        id: Set(Uuid::now_v7()),
        kind: Set(response_job.kind),
        payload: Set(response_job.payload),
        deduplication_key: Set(Some(response_job.deduplication_key)),
        run_after: Set(OffsetDateTime::now_utc()),
        attempts: Set(0),
        locked_at: Set(None),
        locked_by: Set(None),
        claim_id: Set(None),
        last_error: Set(None),
        created_at: Set(OffsetDateTime::now_utc()),
        completed_at: Set(None),
        permanently_failed_at: Set(None),
    })
    .on_conflict_do_nothing()
    .exec(txn)
    .await?;
    Ok(())
}

/// Accept a pending remote Follow and create its Accept delivery job atomically.
pub async fn accept_remote_follow_with_response_job(
    txn: &sea_orm::DatabaseTransaction,
    local_account_id: AccountId,
    remote_actor_id: AccountId,
    activity_id: &str,
    response_job: RemoteFollowResponseJob,
) -> Result<bool> {
    let follow = remote_follow::Entity::find()
        .filter(remote_follow::Column::LocalAccountId.eq(local_account_id.0))
        .filter(remote_follow::Column::RemoteActorId.eq(remote_actor_id.0))
        .filter(remote_follow::Column::ActivityId.eq(activity_id))
        .filter(remote_follow::Column::State.eq(RemoteFollowState::Pending))
        .one(txn)
        .await?;
    let Some(follow) = follow else {
        return Ok(false);
    };
    let mut follow = follow.into_active_model();
    follow.state = Set(RemoteFollowState::Accepted);
    follow.updated_at = Set(OffsetDateTime::now_utc());
    follow.update(txn).await?;
    insert_response_job(txn, response_job).await?;
    Ok(true)
}

/// Remove an incoming remote follow by its original activity identity.
pub async fn delete_remote_follow_by_activity(
    db: &DbConnection,
    remote_actor_id: AccountId,
    activity_id: &str,
) -> Result<()> {
    remote_follow::Entity::delete_many()
        .filter(remote_follow::Column::RemoteActorId.eq(remote_actor_id.0))
        .filter(remote_follow::Column::ActivityId.eq(activity_id))
        .exec(db)
        .await?;
    Ok(())
}

/// Record an inbound Undo(Follow) and remove its original relationship atomically.
pub async fn process_remote_undo_follow(
    txn: &sea_orm::DatabaseTransaction,
    remote_actor_id: AccountId,
    original_activity_id: &str,
) -> Result<bool> {
    remote_follow::Entity::delete_many()
        .filter(remote_follow::Column::RemoteActorId.eq(remote_actor_id.0))
        .filter(remote_follow::Column::ActivityId.eq(original_activity_id))
        .exec(txn)
        .await?;
    Ok(true)
}

/// Reject a pending remote Follow and create its Reject delivery job atomically.
pub async fn delete_remote_follow_with_response_job(
    txn: &sea_orm::DatabaseTransaction,
    local_account_id: AccountId,
    remote_actor_id: AccountId,
    activity_id: &str,
    response_job: RemoteFollowResponseJob,
) -> Result<bool> {
    let follow = remote_follow::Entity::find()
        .filter(remote_follow::Column::LocalAccountId.eq(local_account_id.0))
        .filter(remote_follow::Column::RemoteActorId.eq(remote_actor_id.0))
        .filter(remote_follow::Column::ActivityId.eq(activity_id))
        .filter(remote_follow::Column::State.eq(RemoteFollowState::Pending))
        .one(txn)
        .await?;
    let Some(follow) = follow else {
        return Ok(false);
    };
    follow.into_active_model().delete(txn).await?;
    insert_response_job(txn, response_job).await?;
    Ok(true)
}

/// List pending remote follows for internal approval and rejection lookup.
pub async fn pending_remote_follows(
    db: &impl ConnectionTrait,
    local_account_id: AccountId,
) -> Result<Vec<RemoteFollow>> {
    Ok(remote_follow::Entity::find()
        .filter(remote_follow::Column::LocalAccountId.eq(local_account_id.0))
        .filter(remote_follow::Column::State.eq(RemoteFollowState::Pending))
        .order_by_desc(remote_follow::Column::Id)
        .all(db)
        .await?
        .into_iter()
        .map(remote_follow_from_model)
        .collect())
}

/// List pending remote follow-request actors for a local account with Mastodon cursor pagination.
pub async fn pending_remote_follow_requests(
    db: &impl ConnectionTrait,
    local_account_id: AccountId,
    limit: u64,
    cursor: CollectionCursor,
) -> Result<CollectionPage<RemoteActor>> {
    let rows = remote_follow::Entity::find()
        .filter(remote_follow::Column::LocalAccountId.eq(local_account_id.0))
        .filter(remote_follow::Column::State.eq(RemoteFollowState::Pending))
        .apply_collection_cursor(cursor)
        .order_by_desc(remote_follow::Column::Id)
        .limit(page_query_limit(limit))
        .all(db)
        .await?;
    let (rows, has_more) = trim_to_page(rows, limit);
    let first_cursor = rows.first().map(|row| row.id);
    let last_cursor = rows.last().map(|row| row.id);
    let mut actors = Vec::with_capacity(rows.len());
    for row in rows {
        let actor = remote_actor::Entity::find_by_id(row.remote_actor_id)
            .one(db)
            .await?
            .ok_or_else(|| {
                RoostyError::InvalidInput("remote follow actor is missing".to_owned())
            })?;
        actors.push(remote_actor_from_model(actor));
    }

    Ok(CollectionPage {
        items: actors,
        first_cursor,
        last_cursor,
        has_more,
    })
}

/// Return whether an accepted remote actor follows a local account.
pub async fn remote_actor_follows_local_account(
    db: &impl ConnectionTrait,
    remote_actor_id: AccountId,
    local_account_id: AccountId,
) -> Result<bool> {
    Ok(db.query_one(Statement::from_sql_and_values(DatabaseBackend::Postgres, "SELECT 1 FROM remote_follow WHERE remote_actor_id = $1 AND local_account_id = $2 AND state = 'accepted'", vec![remote_actor_id.0.into(), local_account_id.0.into()])).await?.is_some())
}

/// Return whether a remote actor may interact with a local private status.
pub async fn local_private_status_visible_to_remote_actor(
    db: &impl ConnectionTrait,
    status: &LocalStatus,
    remote_actor_id: AccountId,
) -> Result<bool> {
    if status.visibility != StatusVisibility::Private {
        return Ok(matches!(
            status.visibility,
            StatusVisibility::Public | StatusVisibility::Unlisted
        ));
    }
    if remote_follow::Entity::find()
        .filter(remote_follow::Column::RemoteActorId.eq(remote_actor_id.0))
        .filter(remote_follow::Column::LocalAccountId.eq(status.account_id.0))
        .filter(remote_follow::Column::State.eq(RemoteFollowState::Accepted))
        .one(db)
        .await?
        .is_some()
    {
        return Ok(true);
    }
    Ok(local_status_remote_mention::Entity::find()
        .filter(local_status_remote_mention::Column::StatusId.eq(status.id.0))
        .filter(local_status_remote_mention::Column::RemoteActorId.eq(remote_actor_id.0))
        .one(db)
        .await?
        .is_some())
}

/// Classify and durably register a canonical inbound activity.
///
/// The insert and conflict read run on the caller's transaction, so concurrent
/// deliveries serialize on the activity primary key. Legacy rows with no digest
/// remain duplicate markers for the same signer.
pub async fn register_inbox_activity(
    db: &impl ConnectionTrait,
    metadata: InboxActivityMetadata<'_>,
) -> Result<InboxReplayResult> {
    let inserted =
        processed_inbox_activity::Entity::insert(processed_inbox_activity::ActiveModel {
            activity_id: Set(metadata.activity_id.to_owned()),
            remote_actor_id: Set(metadata.remote_actor_id.0),
            payload_digest: Set(Some(metadata.payload_digest.to_vec())),
            activity_type: Set(Some(metadata.activity_type)),
            outcome: Set(Some(metadata.outcome.to_owned())),
            processed_at: Set(OffsetDateTime::now_utc()),
        })
        .on_conflict_do_nothing()
        .exec(db)
        .await?;
    if matches!(inserted, TryInsertResult::Inserted(_)) {
        return Ok(InboxReplayResult::New);
    }

    classify_inbox_activity(db, metadata)
        .await?
        .ok_or_else(|| RoostyError::InvalidInput("inbox replay marker disappeared".to_owned()))
}

/// Classify an existing replay marker without creating a new one.
pub async fn classify_inbox_activity(
    db: &impl ConnectionTrait,
    metadata: InboxActivityMetadata<'_>,
) -> Result<Option<InboxReplayResult>> {
    let Some(existing) = processed_inbox_activity::Entity::find_by_id(metadata.activity_id)
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    if existing.remote_actor_id != metadata.remote_actor_id.0 {
        return Ok(Some(InboxReplayResult::Conflict));
    }
    let Some(existing_digest) = existing.payload_digest else {
        return Ok(Some(InboxReplayResult::Duplicate));
    };
    if existing_digest == metadata.payload_digest
        && existing.activity_type == Some(metadata.activity_type)
    {
        Ok(Some(InboxReplayResult::Duplicate))
    } else {
        Ok(Some(InboxReplayResult::Conflict))
    }
}

/// Record a legacy inbox marker, returning false for a pre-existing identity.
pub async fn record_processed_inbox_activity(
    db: &impl ConnectionTrait,
    activity_id: &str,
    remote_actor_id: AccountId,
) -> Result<bool> {
    let result = db.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres, "INSERT INTO processed_inbox_activity (activity_id, remote_actor_id) VALUES ($1, $2) ON CONFLICT DO NOTHING", vec![activity_id.to_owned().into(), remote_actor_id.0.into()])).await?;
    Ok(result.rows_affected() == 1)
}

/// Create a follow notification attributable to a remote actor.
pub async fn notify_remote_actor_follow(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    remote_actor_id: AccountId,
    notification_type: LocalNotificationType,
) -> Result<Option<LocalNotification>> {
    if !remote_account_allows_notification(db, account_id, remote_actor_id).await? {
        return Ok(None);
    }
    if !matches!(
        notification_type,
        LocalNotificationType::Follow | LocalNotificationType::FollowRequest
    ) {
        return Err(RoostyError::InvalidInput(
            "remote actor notification type is invalid".to_owned(),
        ));
    }
    if local_notification::Entity::find()
        .filter(local_notification::Column::AccountId.eq(account_id.0))
        .filter(local_notification::Column::NotificationType.eq(notification_type))
        .filter(local_notification::Column::RemoteActorId.eq(Some(remote_actor_id.0)))
        .filter(local_notification::Column::StatusId.is_null())
        .filter(local_notification::Column::RemoteStatusId.is_null())
        .one(db)
        .await?
        .is_some()
    {
        return Ok(None);
    }
    let action =
        remote_notification_policy_action(db, account_id, remote_actor_id, notification_type, None)
            .await?;
    if action == NotificationPolicyAction::Drop {
        return Ok(None);
    }
    let request_id = if action == NotificationPolicyAction::Filter {
        active_notification_request_id(db, account_id, NotificationActor::Remote(remote_actor_id))
            .await?
    } else {
        None
    };
    let model = local_notification::ActiveModel {
        id: Set(Uuid::now_v7()),
        account_id: Set(account_id.0),
        notification_type: Set(notification_type),
        actor_account_id: Set(None),
        remote_actor_id: Set(Some(remote_actor_id.0)),
        status_id: Set(None),
        remote_status_id: Set(None),
        group_id: Set(None),
        filtered: Set(action == NotificationPolicyAction::Filter),
        notification_request_id: Set(request_id),
        report_id: Set(None),
        created_at: Set(OffsetDateTime::now_utc()),
        dismissed_at: Set(None),
    }
    .insert(db)
    .await?;
    Ok(Some(local_notification_from_model(model)))
}

/// Create an idempotent favourite notification caused by a remote actor.
pub async fn notify_remote_actor_favourite<C>(
    db: &C,
    account_id: AccountId,
    remote_actor_id: AccountId,
    status_id: StatusId,
) -> Result<Option<LocalNotification>>
where
    C: ConnectionTrait,
{
    if let Some(existing) = local_notification::Entity::find()
        .filter(local_notification::Column::AccountId.eq(account_id.0))
        .filter(local_notification::Column::NotificationType.eq(LocalNotificationType::Favourite))
        .filter(local_notification::Column::RemoteActorId.eq(Some(remote_actor_id.0)))
        .filter(local_notification::Column::StatusId.eq(Some(status_id.0)))
        .one(db)
        .await?
    {
        return Ok(Some(local_notification_from_model(existing)));
    }
    if !remote_account_allows_notification(db, account_id, remote_actor_id).await? {
        return Ok(None);
    }
    let action = remote_notification_policy_action(
        db,
        account_id,
        remote_actor_id,
        LocalNotificationType::Favourite,
        None,
    )
    .await?;
    if action == NotificationPolicyAction::Drop {
        return Ok(None);
    }
    let request_id = if action == NotificationPolicyAction::Filter {
        active_notification_request_id(db, account_id, NotificationActor::Remote(remote_actor_id))
            .await?
    } else {
        None
    };
    let model = local_notification::ActiveModel {
        id: Set(Uuid::now_v7()),
        account_id: Set(account_id.0),
        notification_type: Set(LocalNotificationType::Favourite),
        actor_account_id: Set(None),
        remote_actor_id: Set(Some(remote_actor_id.0)),
        status_id: Set(Some(status_id.0)),
        remote_status_id: Set(None),
        group_id: Set(None),
        filtered: Set(action == NotificationPolicyAction::Filter),
        notification_request_id: Set(request_id),
        report_id: Set(None),
        created_at: Set(OffsetDateTime::now_utc()),
        dismissed_at: Set(None),
    }
    .insert(db)
    .await?;
    Ok(Some(local_notification_from_model(model)))
}

/// Create an idempotent boost notification caused by a remote actor.
pub async fn notify_remote_actor_reblog(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    remote_actor_id: AccountId,
    status_id: StatusId,
) -> Result<Option<LocalNotification>> {
    if !remote_account_allows_notification(db, account_id, remote_actor_id).await? {
        return Ok(None);
    }
    if local_notification::Entity::find()
        .filter(local_notification::Column::AccountId.eq(account_id.0))
        .filter(local_notification::Column::NotificationType.eq(LocalNotificationType::Reblog))
        .filter(local_notification::Column::RemoteActorId.eq(Some(remote_actor_id.0)))
        .filter(local_notification::Column::StatusId.eq(Some(status_id.0)))
        .one(db)
        .await?
        .is_some()
    {
        return Ok(None);
    }
    let action = remote_notification_policy_action(
        db,
        account_id,
        remote_actor_id,
        LocalNotificationType::Reblog,
        None,
    )
    .await?;
    if action == NotificationPolicyAction::Drop {
        return Ok(None);
    }
    let request_id = if action == NotificationPolicyAction::Filter {
        active_notification_request_id(db, account_id, NotificationActor::Remote(remote_actor_id))
            .await?
    } else {
        None
    };
    let model = local_notification::ActiveModel {
        id: Set(Uuid::now_v7()),
        account_id: Set(account_id.0),
        notification_type: Set(LocalNotificationType::Reblog),
        actor_account_id: Set(None),
        remote_actor_id: Set(Some(remote_actor_id.0)),
        status_id: Set(Some(status_id.0)),
        remote_status_id: Set(None),
        group_id: Set(None),
        filtered: Set(action == NotificationPolicyAction::Filter),
        notification_request_id: Set(request_id),
        report_id: Set(None),
        created_at: Set(OffsetDateTime::now_utc()),
        dismissed_at: Set(None),
    }
    .insert(db)
    .await?;
    Ok(Some(local_notification_from_model(model)))
}

/// Create an idempotent mention notification caused by a cached remote Note.
pub async fn notify_remote_status_mention<C>(
    db: &C,
    account_id: AccountId,
    remote_actor_id: AccountId,
    remote_status_id: StatusId,
) -> Result<Option<LocalNotification>>
where
    C: ConnectionTrait,
{
    if !remote_account_allows_notification(db, account_id, remote_actor_id).await? {
        return Ok(None);
    }
    if let Some(existing) = local_notification::Entity::find()
        .filter(local_notification::Column::AccountId.eq(account_id.0))
        .filter(local_notification::Column::NotificationType.eq(LocalNotificationType::Mention))
        .filter(local_notification::Column::RemoteActorId.eq(Some(remote_actor_id.0)))
        .filter(local_notification::Column::RemoteStatusId.eq(Some(remote_status_id.0)))
        .one(db)
        .await?
    {
        return Ok((!existing.filtered).then(|| local_notification_from_model(existing)));
    }
    let action = remote_notification_policy_action(
        db,
        account_id,
        remote_actor_id,
        LocalNotificationType::Mention,
        Some(remote_status_id),
    )
    .await?;
    if action == NotificationPolicyAction::Drop {
        return Ok(None);
    }
    let request_id = if action == NotificationPolicyAction::Filter {
        Some(
            upsert_notification_request(
                db,
                account_id,
                NotificationActor::Remote(remote_actor_id),
                remote_status_id,
            )
            .await?,
        )
    } else {
        None
    };
    let model = local_notification::ActiveModel {
        id: Set(Uuid::now_v7()),
        account_id: Set(account_id.0),
        notification_type: Set(LocalNotificationType::Mention),
        actor_account_id: Set(None),
        remote_actor_id: Set(Some(remote_actor_id.0)),
        status_id: Set(None),
        remote_status_id: Set(Some(remote_status_id.0)),
        group_id: Set(None),
        filtered: Set(request_id.is_some()),
        notification_request_id: Set(request_id),
        report_id: Set(None),
        created_at: Set(OffsetDateTime::now_utc()),
        dismissed_at: Set(None),
    }
    .insert(db)
    .await?;
    Ok(request_id
        .is_none()
        .then(|| local_notification_from_model(model)))
}

/// Replace Mastodon `update` notifications for local accounts that boosted a local status.
pub async fn replace_local_status_update_notifications(
    db: &impl ConnectionTrait,
    status_id: StatusId,
    author_id: AccountId,
) -> Result<Vec<LocalNotification>> {
    let reblogs = local_status_reblog::Entity::find()
        .filter(local_status_reblog::Column::StatusId.eq(status_id.0))
        .all(db)
        .await?;
    let mut notifications = Vec::new();
    for reblog in reblogs {
        let account_id = AccountId(reblog.account_id);
        if account_id == author_id
            || !local_account_allows_notification(db, account_id, author_id).await?
        {
            continue;
        }
        local_notification::Entity::delete_many()
            .filter(local_notification::Column::AccountId.eq(account_id.0))
            .filter(local_notification::Column::NotificationType.eq(LocalNotificationType::Update))
            .filter(local_notification::Column::StatusId.eq(status_id.0))
            .exec(db)
            .await?;
        let model = local_notification::ActiveModel {
            id: Set(Uuid::now_v7()),
            account_id: Set(account_id.0),
            notification_type: Set(LocalNotificationType::Update),
            actor_account_id: Set(Some(author_id.0)),
            remote_actor_id: Set(None),
            status_id: Set(Some(status_id.0)),
            remote_status_id: Set(None),
            group_id: Set(None),
            filtered: Set(false),
            notification_request_id: Set(None),
            report_id: Set(None),
            created_at: Set(OffsetDateTime::now_utc()),
            dismissed_at: Set(None),
        }
        .insert(db)
        .await?;
        notifications.push(local_notification_from_model(model));
    }
    Ok(notifications)
}

/// Replace Mastodon `update` notifications for local accounts that boosted a remote status.
pub async fn replace_remote_status_update_notifications(
    db: &impl ConnectionTrait,
    status_id: StatusId,
    remote_actor_id: AccountId,
) -> Result<Vec<LocalNotification>> {
    let reblogs = local_remote_status_reblog::Entity::find()
        .filter(local_remote_status_reblog::Column::RemoteStatusId.eq(status_id.0))
        .all(db)
        .await?;
    let mut notifications = Vec::new();
    for reblog in reblogs {
        let account_id = AccountId(reblog.local_account_id);
        if !remote_account_allows_notification(db, account_id, remote_actor_id).await? {
            continue;
        }
        local_notification::Entity::delete_many()
            .filter(local_notification::Column::AccountId.eq(account_id.0))
            .filter(local_notification::Column::NotificationType.eq(LocalNotificationType::Update))
            .filter(local_notification::Column::RemoteStatusId.eq(status_id.0))
            .exec(db)
            .await?;
        let model = local_notification::ActiveModel {
            id: Set(Uuid::now_v7()),
            account_id: Set(account_id.0),
            notification_type: Set(LocalNotificationType::Update),
            actor_account_id: Set(None),
            remote_actor_id: Set(Some(remote_actor_id.0)),
            status_id: Set(None),
            remote_status_id: Set(Some(status_id.0)),
            group_id: Set(None),
            filtered: Set(false),
            notification_request_id: Set(None),
            report_id: Set(None),
            created_at: Set(OffsetDateTime::now_utc()),
            dismissed_at: Set(None),
        }
        .insert(db)
        .await?;
        notifications.push(local_notification_from_model(model));
    }
    Ok(notifications)
}

/// Create an idempotent new-post notification caused by a cached remote Note.
pub async fn notify_remote_status<C>(
    db: &C,
    account_id: AccountId,
    remote_actor_id: AccountId,
    remote_status_id: StatusId,
) -> Result<Option<LocalNotification>>
where
    C: ConnectionTrait,
{
    if !remote_account_allows_notification(db, account_id, remote_actor_id).await? {
        return Ok(None);
    }
    if local_notification::Entity::find()
        .filter(local_notification::Column::AccountId.eq(account_id.0))
        .filter(local_notification::Column::NotificationType.eq(LocalNotificationType::Status))
        .filter(local_notification::Column::RemoteActorId.eq(Some(remote_actor_id.0)))
        .filter(local_notification::Column::RemoteStatusId.eq(Some(remote_status_id.0)))
        .one(db)
        .await?
        .is_some()
    {
        return Ok(None);
    }
    let model = local_notification::ActiveModel {
        id: Set(Uuid::now_v7()),
        account_id: Set(account_id.0),
        notification_type: Set(LocalNotificationType::Status),
        actor_account_id: Set(None),
        remote_actor_id: Set(Some(remote_actor_id.0)),
        status_id: Set(None),
        remote_status_id: Set(Some(remote_status_id.0)),
        group_id: Set(None),
        filtered: Set(false),
        notification_request_id: Set(None),
        report_id: Set(None),
        created_at: Set(OffsetDateTime::now_utc()),
        dismissed_at: Set(None),
    }
    .insert(db)
    .await?;
    Ok(Some(local_notification_from_model(model)))
}

/// Timelines that support persisted Mastodon read markers.
#[derive(
    Clone, Copy, Debug, DeriveValueType, Display, EnumString, Eq, IntoStaticStr, PartialEq,
)]
#[sea_orm(value_type = "String")]
#[strum(serialize_all = "snake_case")]
pub enum LocalTimeline {
    Home,
    Notifications,
}

impl TryFromU64 for LocalTimeline {
    fn try_from_u64(_: u64) -> std::result::Result<Self, DbErr> {
        Err(DbErr::ConvertFromU64("LocalTimeline"))
    }
}

impl LocalTimeline {
    pub fn as_str(self) -> &'static str {
        self.into()
    }
}

/// Persisted read position for one account timeline.
#[derive(Clone, Debug)]
pub struct LocalTimelineMarker {
    /// Timeline whose read position this marker records.
    pub timeline: LocalTimeline,
    /// Opaque identifier of the last item the account read.
    pub last_read_id: Uuid,
    /// Monotonically increasing revision of this marker.
    pub version: i64,
    /// Time of the most recent marker update.
    pub updated_at: OffsetDateTime,
}

/// Filters accepted by local notification collection queries.
#[derive(Clone, Debug, Default)]
pub struct NotificationFilter {
    /// Only include these notification types when present.
    pub include_types: Vec<LocalNotificationType>,
    /// Exclude these notification types.
    pub exclude_types: Vec<LocalNotificationType>,
    /// Only include notifications caused by this account.
    pub account_id: Option<AccountId>,
    /// Include notifications held in notification requests.
    pub include_filtered: bool,
}

/// One Mastodon-compatible grouped notification projection.
#[derive(Clone, Debug)]
pub struct NotificationGroup {
    pub group_key: String,
    pub notifications_count: u64,
    pub notification_type: LocalNotificationType,
    pub most_recent_notification_id: Uuid,
    pub page_min_id: Uuid,
    pub page_max_id: Uuid,
    pub latest_page_notification_at: OffsetDateTime,
    pub sample_account_ids: Vec<AccountId>,
    pub status_id: Option<StatusId>,
    pub remote_status: bool,
}

#[derive(Clone, Debug)]
pub struct NotificationGroupPage {
    pub items: Vec<NotificationGroup>,
    pub first_cursor: Option<Uuid>,
    pub last_cursor: Option<Uuid>,
    pub has_more: bool,
}

#[derive(FromQueryResult)]
struct NotificationGroupRow {
    group_key: String,
    notification_type: LocalNotificationType,
    notifications_count: i64,
    most_recent_notification_id: Uuid,
    page_min_id: Uuid,
    page_max_id: Uuid,
    latest_page_notification_at: OffsetDateTime,
    sample_account_ids: JsonValue,
    status_id: Option<Uuid>,
    remote_status: bool,
}

/// Stored local follow relationship between two accounts.
#[derive(Clone, Debug)]
pub struct LocalFollow {
    /// Account that follows another account.
    pub follower_account_id: AccountId,
    /// Account being followed.
    pub followed_account_id: AccountId,
    /// Whether boosts should appear in the follower's home timeline.
    pub show_reblogs: bool,
    /// Whether the follower wants notifications for new posts.
    pub notify: bool,
}

/// Stored local account mute relationship.
#[derive(Clone, Debug)]
pub struct LocalAccountMute {
    /// Account that muted another local account.
    pub account_id: AccountId,
    /// Account that is muted.
    pub target_account_id: AccountId,
    /// Whether the mute suppresses notifications as well as statuses.
    pub notifications: bool,
    /// Optional timestamp when the mute stops applying.
    pub expires_at: Option<OffsetDateTime>,
}

/// Stored local moderation relationship targeting a cached remote actor.
#[derive(Clone, Debug)]
pub struct LocalRemoteAccountBlock {
    pub local_account_id: AccountId,
    pub remote_actor_id: AccountId,
    /// Stable ID of the outbound ActivityPub `Block` activity.
    pub activity_id: String,
}

/// Stored local-only mute targeting a cached remote actor.
#[derive(Clone, Debug)]
pub struct LocalRemoteAccountMute {
    pub local_account_id: AccountId,
    pub remote_actor_id: AccountId,
    pub notifications: bool,
    pub expires_at: Option<OffsetDateTime>,
}

/// OAuth client application metadata.
#[derive(Clone, Debug)]
pub struct OAuthApplication {
    /// Internal application identifier.
    pub id: Uuid,
    /// Public OAuth client id.
    pub client_id: String,
    /// Hashed OAuth client secret.
    pub client_secret_hash: String,
    /// Human-readable client name.
    pub name: String,
    /// Registered redirect URI, or newline-separated redirect URI list.
    pub redirect_uri: String,
    /// Space-separated OAuth scopes registered by the client.
    pub scopes: String,
    /// Optional client website.
    pub website: Option<String>,
}

/// Newly issued OAuth access token material.
#[derive(Clone, Debug)]
pub struct OAuthAccessToken {
    /// Raw bearer token returned once to the OAuth client.
    pub token: String,
    /// OAuth token type.
    pub token_type: OAuthTokenType,
    /// Space-separated scopes granted to the token.
    pub scope: String,
    /// Unix timestamp for token issuance.
    pub created_at: i64,
}

/// OAuth bearer token type returned by Mastodon-compatible token endpoints.
#[derive(Clone, Copy, Debug, Serialize)]
pub enum OAuthTokenType {
    Bearer,
}

/// Validated access-token grant used by APIs that must retain token identity.
#[derive(Clone, Debug)]
pub struct AccessTokenGrant {
    pub id: Uuid,
    pub account: LocalAccount,
    pub scopes: String,
}

/// Subscription delivery policy accepted by Mastodon-compatible clients.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    DeriveValueType,
    Display,
    EnumString,
    Eq,
    IntoStaticStr,
    PartialEq,
    Serialize,
    Deserialize,
)]
#[sea_orm(value_type = "String")]
#[strum(serialize_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum PushPolicy {
    #[default]
    All,
    Followed,
    Follower,
    None,
}

/// Stored Web Push content-encoding selection.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PushSubscriptionEncoding {
    #[default]
    Legacy,
    Standard,
}

/// Closed set of notification switches supported by Roosty.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct PushAlerts {
    #[serde(default)]
    pub mention: bool,
    #[serde(default)]
    pub favourite: bool,
    #[serde(default)]
    pub follow: bool,
    #[serde(default)]
    pub follow_request: bool,
    #[serde(default)]
    pub reblog: bool,
    #[serde(default)]
    pub status: bool,
    #[serde(default)]
    pub update: bool,
    #[serde(default)]
    pub quote: bool,
    #[serde(default)]
    pub quoted_update: bool,
    #[serde(default)]
    pub poll: bool,
    #[serde(default, rename = "admin.report")]
    pub admin_report: bool,
}

impl PushAlerts {
    pub fn enabled(&self, notification_type: LocalNotificationType) -> bool {
        match notification_type {
            LocalNotificationType::Mention => self.mention,
            LocalNotificationType::Favourite => self.favourite,
            LocalNotificationType::Follow => self.follow,
            LocalNotificationType::FollowRequest => self.follow_request,
            LocalNotificationType::Reblog => self.reblog,
            LocalNotificationType::Status => self.status,
            LocalNotificationType::Update => self.update,
            LocalNotificationType::Quote => self.quote,
            LocalNotificationType::QuotedUpdate => self.quoted_update,
            LocalNotificationType::Poll => self.poll,
            LocalNotificationType::AdminReport => self.admin_report,
        }
    }
}

/// Notify every active local administrator about a newly accepted report.
pub async fn notify_administrators_of_report(
    txn: &DatabaseTransaction,
    report: &ModerationReport,
) -> Result<Vec<LocalNotification>> {
    let administrators = local_account::Entity::find()
        .filter(local_account::Column::IsAdmin.eq(true))
        .filter(local_account::Column::SuspendedAt.is_null())
        .all(txn)
        .await?;
    let (actor_account_id, remote_actor_id) = match report.source {
        ReportAccount::Local(id) => (Some(id.0), None),
        ReportAccount::Remote(id) => (None, Some(id.0)),
    };
    let mut notifications = Vec::with_capacity(administrators.len());
    for administrator in administrators {
        let model = local_notification::ActiveModel {
            id: Set(Uuid::now_v7()),
            account_id: Set(administrator.id),
            notification_type: Set(LocalNotificationType::AdminReport),
            actor_account_id: Set(actor_account_id),
            remote_actor_id: Set(remote_actor_id),
            status_id: Set(None),
            remote_status_id: Set(None),
            group_id: Set(None),
            filtered: Set(false),
            notification_request_id: Set(None),
            report_id: Set(Some(report.id)),
            created_at: Set(OffsetDateTime::now_utc()),
            dismissed_at: Set(None),
        }
        .insert(txn)
        .await?;
        notifications.push(local_notification_from_model(model));
    }
    Ok(notifications)
}

/// Persisted Web Push subscription and encrypted Mastodon payload credential.
#[derive(Clone, Debug)]
pub struct PushSubscription {
    pub id: Uuid,
    pub access_token_id: Uuid,
    pub account_id: AccountId,
    pub endpoint: String,
    pub p256dh: Vec<u8>,
    pub auth: Vec<u8>,
    pub encoding: PushSubscriptionEncoding,
    pub policy: PushPolicy,
    pub alerts: PushAlerts,
    pub access_token_ciphertext: Vec<u8>,
    pub access_token_nonce: Vec<u8>,
}

/// Values that replace one access token's Web Push subscription atomically.
#[derive(Clone, Debug)]
pub struct NewPushSubscription {
    pub access_token_id: Uuid,
    pub account_id: AccountId,
    pub endpoint: String,
    pub p256dh: Vec<u8>,
    pub auth: Vec<u8>,
    pub encoding: PushSubscriptionEncoding,
    pub policy: PushPolicy,
    pub alerts: PushAlerts,
    pub access_token_ciphertext: Vec<u8>,
    pub access_token_nonce: Vec<u8>,
}

/// Find a local account by username or email for password login.
pub async fn find_local_account_by_login(
    db: &impl ConnectionTrait,
    login: &str,
) -> Result<Option<LocalAccount>> {
    let account = local_account::Entity::find()
        .filter(
            local_account::Column::Username
                .eq(login)
                .or(local_account::Column::Email.eq(login)),
        )
        .one(db)
        .await?;

    account.map(local_account_from_model).transpose()
}

/// Find a local account by internal id.
pub async fn find_local_account_by_id<C>(
    db: &C,
    account_id: AccountId,
) -> Result<Option<LocalAccount>>
where
    C: ConnectionTrait,
{
    let account = local_account::Entity::find_by_id(account_id.0)
        .one(db)
        .await?;

    account.map(local_account_from_model).transpose()
}

/// Find a local account by its exact local username.
pub async fn find_local_account_by_username(
    db: &impl ConnectionTrait,
    username: &str,
) -> Result<Option<LocalAccount>> {
    let account = local_account::Entity::find()
        .filter(local_account::Column::Username.eq(username))
        .one(db)
        .await?;

    account.map(local_account_from_model).transpose()
}

/// Set or clear the notification/discovery limit on a local account.
pub async fn set_local_account_limited(
    db: &DbConnection,
    username: &str,
    limited: bool,
) -> Result<Option<LocalAccount>> {
    let txn = db.begin().await?;
    let Some(account) = local_account::Entity::find()
        .filter(local_account::Column::Username.eq(username))
        .lock_exclusive()
        .one(&txn)
        .await?
    else {
        txn.commit().await?;
        return Ok(None);
    };
    let mut active = account.into_active_model();
    active.limited_at = Set(limited.then(OffsetDateTime::now_utc));
    let account = local_account_from_model(active.update(&txn).await?)?;
    mark_account_status_trends_dirty(&txn, "local", account.id).await?;
    txn.commit().await?;
    Ok(Some(account))
}

/// Set or clear the notification/discovery limit on a cached remote actor.
pub async fn set_remote_actor_limited(
    db: &DbConnection,
    username: &str,
    domain: &str,
    limited: bool,
) -> Result<Option<RemoteActor>> {
    let txn = db.begin().await?;
    let Some(actor) = remote_actor::Entity::find()
        .filter(remote_actor::Column::Username.eq(username))
        .filter(remote_actor::Column::Domain.eq(domain))
        .lock_exclusive()
        .one(&txn)
        .await?
    else {
        txn.commit().await?;
        return Ok(None);
    };
    let mut active = actor.into_active_model();
    active.limited_at = Set(limited.then(OffsetDateTime::now_utc));
    let actor = remote_actor_from_model(active.update(&txn).await?);
    mark_account_status_trends_dirty(&txn, "remote", actor.id).await?;
    txn.commit().await?;
    Ok(Some(actor))
}

/// Set a local account's limit state inside a caller-owned transaction.
pub async fn set_local_account_limited_by_id(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    limited: bool,
) -> Result<Option<LocalAccount>> {
    let Some(account) = local_account::Entity::find_by_id(account_id.0)
        .lock_exclusive()
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    let mut active = account.into_active_model();
    active.limited_at = Set(limited.then(OffsetDateTime::now_utc));
    let account = local_account_from_model(active.update(db).await?)?;
    mark_account_status_trends_dirty(db, "local", account.id).await?;
    Ok(Some(account))
}

/// Set a cached remote actor's limit state inside a caller-owned transaction.
pub async fn set_remote_actor_limited_by_id(
    db: &impl ConnectionTrait,
    actor_id: AccountId,
    limited: bool,
) -> Result<Option<RemoteActor>> {
    let Some(actor) = remote_actor::Entity::find_by_id(actor_id.0)
        .lock_exclusive()
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    let mut active = actor.into_active_model();
    active.limited_at = Set(limited.then(OffsetDateTime::now_utc));
    let actor = remote_actor_from_model(active.update(db).await?);
    mark_account_status_trends_dirty(db, "remote", actor.id).await?;
    Ok(Some(actor))
}

/// Set or clear direct administrator suspension and enqueue irreversible cleanup work.
pub async fn set_account_suspended_by_id(
    txn: &DatabaseTransaction,
    account_id: AccountId,
    suspended: bool,
) -> Result<Option<AdminAccount>> {
    let now = OffsetDateTime::now_utc();
    if let Some(account) = local_account::Entity::find_by_id(account_id.0)
        .lock_exclusive()
        .one(txn)
        .await?
    {
        let was_suspended = account.suspended_at.is_some();
        let mut active = account.into_active_model();
        active.suspended_at = Set(suspended.then_some(now));
        let updated = active.update(txn).await?;
        if suspended && !was_suspended {
            sever_local_account_relationships(txn, account_id, now).await?;
            enqueue_job_on_connection(
                txn,
                NewJob {
                    kind: JobKind::AccountPurge,
                    payload: serde_json::json!({
                        "account_id": account_id.0,
                        "origin": "local"
                    }),
                    deduplication_key: Some(format!("account-purge:{}", account_id.0)),
                    run_after: now + Duration::days(30),
                },
            )
            .await?;
        } else if !suspended {
            cancel_account_purge(txn, account_id, now).await?;
        }
        mark_account_status_trends_dirty(txn, "local", account_id).await?;
        return Ok(Some(AdminAccount {
            id: account_id,
            username: updated.username,
            domain: None,
            email: Some(updated.email),
            display_name: updated.display_name,
            is_admin: updated.is_admin,
            limited: updated.limited_at.is_some(),
            suspended: updated.suspended_at.is_some(),
            data_purged_at: updated.data_purged_at,
            created_at: updated.created_at,
        }));
    }
    if let Some(actor) = remote_actor::Entity::find_by_id(account_id.0)
        .lock_exclusive()
        .one(txn)
        .await?
    {
        let mut active = actor.into_active_model();
        active.suspended_at = Set(suspended.then_some(now));
        if suspended {
            active.data_purged_at = Set(Some(now));
        }
        let updated = active.update(txn).await?;
        if suspended {
            purge_remote_actor_cache(txn, account_id, now).await?;
        }
        mark_account_status_trends_dirty(txn, "remote", account_id).await?;
        return Ok(Some(AdminAccount {
            id: account_id,
            username: updated.username,
            domain: Some(updated.domain),
            email: None,
            display_name: updated.display_name,
            is_admin: false,
            limited: updated.limited_at.is_some(),
            suspended: updated.suspended_at.is_some(),
            data_purged_at: updated.data_purged_at,
            created_at: updated.profile_created_at.unwrap_or(updated.created_at),
        }));
    }
    Ok(None)
}

async fn sever_local_account_relationships(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    now: OffsetDateTime,
) -> Result<()> {
    for statement in [
        "DELETE FROM local_follow WHERE follower_account_id = $1 OR followed_account_id = $1",
        "DELETE FROM remote_follow WHERE local_account_id = $1",
        "DELETE FROM remote_following WHERE local_account_id = $1",
        "DELETE FROM local_account_block WHERE account_id = $1 OR target_account_id = $1",
        "DELETE FROM local_account_mute WHERE account_id = $1 OR target_account_id = $1",
        "DELETE FROM local_remote_account_block WHERE local_account_id = $1",
        "DELETE FROM local_remote_account_mute WHERE local_account_id = $1",
        "DELETE FROM remote_local_account_block WHERE local_account_id = $1",
        "DELETE FROM local_list_local_member WHERE account_id = $1 OR list_id IN (SELECT id FROM local_list WHERE account_id = $1)",
        "DELETE FROM local_list_remote_member WHERE list_id IN (SELECT id FROM local_list WHERE account_id = $1)",
        "DELETE FROM local_list WHERE account_id = $1",
        "DELETE FROM scheduled_status WHERE account_id = $1",
    ] {
        db.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            statement,
            vec![account_id.0.into()],
        ))
        .await?;
    }
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "UPDATE oauth_access_token SET revoked_at = $2 WHERE account_id = $1 AND revoked_at IS NULL",
        vec![account_id.0.into(), now.into()],
    ))
    .await?;
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "UPDATE local_notification SET dismissed_at = $2 WHERE (account_id = $1 OR actor_account_id = $1) AND dismissed_at IS NULL",
        vec![account_id.0.into(), now.into()],
    ))
    .await?;
    Ok(())
}

async fn purge_remote_actor_cache(
    db: &impl ConnectionTrait,
    actor_id: AccountId,
    now: OffsetDateTime,
) -> Result<()> {
    remote_follow::Entity::delete_many()
        .filter(remote_follow::Column::RemoteActorId.eq(actor_id.0))
        .exec(db)
        .await?;
    remote_following::Entity::delete_many()
        .filter(remote_following::Column::RemoteActorId.eq(actor_id.0))
        .exec(db)
        .await?;
    local_list_remote_member::Entity::delete_many()
        .filter(local_list_remote_member::Column::RemoteActorId.eq(actor_id.0))
        .exec(db)
        .await?;
    local_remote_account_block::Entity::delete_many()
        .filter(local_remote_account_block::Column::RemoteActorId.eq(actor_id.0))
        .exec(db)
        .await?;
    local_remote_account_mute::Entity::delete_many()
        .filter(local_remote_account_mute::Column::RemoteActorId.eq(actor_id.0))
        .exec(db)
        .await?;
    remote_local_account_block::Entity::delete_many()
        .filter(remote_local_account_block::Column::RemoteActorId.eq(actor_id.0))
        .exec(db)
        .await?;
    remote_status::Entity::update_many()
        .col_expr(remote_status::Column::DeletedAt, Expr::value(Some(now)))
        .filter(remote_status::Column::RemoteActorId.eq(actor_id.0))
        .exec(db)
        .await?;
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "UPDATE local_notification SET dismissed_at = $2 WHERE remote_actor_id = $1 AND dismissed_at IS NULL",
        vec![actor_id.0.into(), now.into()],
    ))
    .await?;
    Ok(())
}

async fn cancel_account_purge(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    now: OffsetDateTime,
) -> Result<()> {
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"UPDATE job SET completed_at = $2, last_error = 'cancelled after unsuspend'
           WHERE kind = 'account_purge' AND completed_at IS NULL
             AND payload->>'account_id' = $1"#,
        vec![account_id.0.to_string().into(), now.into()],
    ))
    .await?;
    Ok(())
}

/// Permanently purge retained data if the local account remains suspended.
pub async fn purge_suspended_local_account(
    txn: &DatabaseTransaction,
    account_id: AccountId,
) -> Result<Vec<String>> {
    let Some(account) = local_account::Entity::find_by_id(account_id.0)
        .lock_exclusive()
        .one(txn)
        .await?
    else {
        return Ok(Vec::new());
    };
    if account.suspended_at.is_none() || account.data_purged_at.is_some() {
        return Ok(Vec::new());
    }
    let attachments = local_media_attachment::Entity::find()
        .filter(local_media_attachment::Column::AccountId.eq(account_id.0))
        .all(txn)
        .await?;
    let mut paths = attachments
        .iter()
        .flat_map(|attachment| {
            [
                Some(attachment.file_path.clone()),
                attachment.preview_file_path.clone(),
            ]
        })
        .flatten()
        .collect::<Vec<_>>();
    paths.extend(account.avatar_file_path.clone());
    paths.extend(account.header_file_path.clone());
    local_status::Entity::delete_many()
        .filter(local_status::Column::AccountId.eq(account_id.0))
        .exec(txn)
        .await?;
    local_media_attachment::Entity::delete_many()
        .filter(local_media_attachment::Column::AccountId.eq(account_id.0))
        .exec(txn)
        .await?;
    let mut active = account.into_active_model();
    active.display_name = Set(String::new());
    active.note = Set(String::new());
    active.profile_fields = Set(serde_json::json!([]));
    active.avatar_file_path = Set(None);
    active.header_file_path = Set(None);
    active.data_purged_at = Set(Some(OffsetDateTime::now_utc()));
    active.update(txn).await?;
    Ok(paths)
}

/// Search local accounts by username or display name for Mastodon autocomplete.
pub async fn search_local_accounts(
    db: &DbConnection,
    viewer_account_id: AccountId,
    query: &str,
    limit: u64,
    offset: u64,
) -> Result<Vec<LocalAccount>> {
    if query.trim().is_empty() || limit == 0 {
        return Ok(Vec::new());
    }

    let hidden_account_ids = blocked_local_account_ids_for_account(db, viewer_account_id).await?;
    let mut accounts = local_account::Entity::find()
        .filter(local_account::Column::LimitedAt.is_null())
        .filter(local_account::Column::SuspendedAt.is_null())
        .filter(
            local_account::Column::Username
                .contains(query)
                .or(local_account::Column::DisplayName.contains(query)),
        );
    if !hidden_account_ids.is_empty() {
        accounts = accounts.filter(
            local_account::Column::Id.is_not_in(hidden_account_ids.into_iter().map(|id| id.0)),
        );
    }
    let accounts = accounts
        .order_by_asc(local_account::Column::Username)
        .limit(limit)
        .offset(offset)
        .all(db)
        .await?;

    accounts.into_iter().map(local_account_from_model).collect()
}

/// Search local and cached remote accounts with one stable Mastodon-compatible ranking.
pub async fn search_accounts(
    db: &impl ConnectionTrait,
    options: AccountSearchOptions<'_>,
) -> Result<Vec<AccountSearchResult>> {
    if options.query.trim().is_empty() || options.limit == 0 {
        return Ok(Vec::new());
    }
    let rows = db
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
            WITH candidates AS (
                SELECT 'local'::text AS account_kind, account.id,
                       lower(account.username) AS username,
                       lower(account.display_name) AS display_name,
                       lower(account.username || '@' || $3) AS address,
                       EXISTS (
                           SELECT 1 FROM local_follow follow
                           WHERE follow.follower_account_id = $1
                             AND follow.followed_account_id = account.id
                       ) AS followed
                FROM local_account account
                WHERE account.limited_at IS NULL
                  AND account.suspended_at IS NULL
                  AND NOT EXISTS (
                    SELECT 1 FROM local_account_block block
                    WHERE block.account_id = $1 AND block.target_account_id = account.id
                )
                UNION ALL
                SELECT 'remote'::text AS account_kind, actor.id,
                       lower(actor.username), lower(actor.display_name),
                       lower(actor.username || '@' || actor.domain),
                       EXISTS (
                           SELECT 1 FROM remote_following follow
                           WHERE follow.local_account_id = $1
                             AND follow.remote_actor_id = actor.id
                             AND follow.state = 'accepted'
                             AND follow.deactivated_at IS NULL
                       ) AS followed
                FROM remote_actor actor
                WHERE $4
                  AND actor.limited_at IS NULL
                  AND actor.suspended_at IS NULL
                  AND ($8 OR actor.domain IN (
                    SELECT jsonb_array_elements_text($9::jsonb)
                  ))
                  AND NOT EXISTS (
                    SELECT 1
                    FROM jsonb_array_elements_text($10::jsonb) blocked(domain)
                    WHERE actor.domain = blocked.domain
                       OR actor.domain LIKE '%.' || blocked.domain
                  )
                  AND actor.deleted_at IS NULL
                  AND NOT EXISTS (
                    SELECT 1 FROM local_remote_account_block block
                    WHERE block.local_account_id = $1 AND block.remote_actor_id = actor.id
                  )
            )
            SELECT account_kind, id
            FROM candidates
            WHERE ($5 = false OR followed)
              AND (username LIKE '%' || lower($2) || '%'
                   OR display_name LIKE '%' || lower($2) || '%'
                   OR address LIKE '%' || lower($2) || '%')
            ORDER BY
              CASE WHEN address = lower($2) THEN 0 ELSE 1 END,
              followed DESC,
              CASE
                WHEN username = lower($2) THEN 0
                WHEN username LIKE lower($2) || '%' THEN 1
                WHEN display_name LIKE lower($2) || '%' THEN 2
                ELSE 3
              END,
              id ASC
            LIMIT $6 OFFSET $7
            "#,
            vec![
                options.viewer_account_id.0.into(),
                options.query.to_owned().into(),
                options.local_domain.to_ascii_lowercase().into(),
                options.include_remote.into(),
                options.following_only.into(),
                (options.limit as i64).into(),
                (options.offset as i64).into(),
                options.allow_all_remote_domains.into(),
                serde_json::to_string(options.allowed_remote_domains)
                    .map_err(|error| RoostyError::InvalidInput(error.to_string()))?
                    .into(),
                serde_json::to_string(options.blocked_remote_domains)
                    .map_err(|error| RoostyError::InvalidInput(error.to_string()))?
                    .into(),
            ],
        ))
        .await?;
    let mut results = Vec::with_capacity(rows.len());
    for row in rows {
        let kind: AccountSearchKind = row.try_get("", "account_kind")?;
        let id = AccountId(row.try_get("", "id")?);
        match kind {
            AccountSearchKind::Local => {
                if let Some(account) = find_local_account_by_id(db, id).await? {
                    results.push(AccountSearchResult::Local(account));
                }
            }
            AccountSearchKind::Remote => {
                if let Some(actor) = find_remote_actor_by_id(db, id).await?
                    && actor.deleted_at.is_none()
                {
                    results.push(AccountSearchResult::Remote(actor));
                }
            }
        }
    }
    Ok(results)
}

/// Search status documents while enforcing Mastodon's viewer interaction scope.
pub async fn search_statuses(
    db: &impl ConnectionTrait,
    options: StatusSearchOptions<'_>,
) -> Result<StatusSearchPage> {
    #[derive(FromQueryResult)]
    struct Row {
        status_kind: StatusSearchKind,
        status_id: Uuid,
    }

    if options.query.chars().count() < 3 || options.limit == 0 {
        return Ok(StatusSearchPage {
            items: Vec::new(),
            first_cursor: None,
            last_cursor: None,
            has_more: false,
        });
    }
    let escaped_query = options
        .query
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    let blocked_domains = serde_json::to_string(options.blocked_remote_domains)
        .map_err(|error| RoostyError::InvalidInput(error.to_string()))?;
    let rows = Row::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"
        WITH candidates AS (
            SELECT 'local'::text AS status_kind, status.id AS status_id
            FROM status_search_document document
            JOIN local_status status ON status.id = document.local_status_id
            JOIN local_account author ON author.id = status.account_id
            WHERE document.document ILIKE '%' || $2 || '%' ESCAPE '\'
              AND status.deleted_at IS NULL
              AND (author.limited_at IS NULL OR author.id = $1)
              AND author.suspended_at IS NULL
              AND ($3::uuid IS NULL OR author.id = $3)
              AND (
                author.id = $1
                OR EXISTS (
                    SELECT 1 FROM local_status_local_mention mention
                    WHERE mention.status_id = status.id
                      AND mention.account_id = $1 AND mention.active
                )
                OR EXISTS (
                    SELECT 1 FROM local_status_favourite favourite
                    WHERE favourite.status_id = status.id
                      AND favourite.account_id = $1
                )
                OR EXISTS (
                    SELECT 1 FROM local_status_reblog reblog
                    WHERE reblog.status_id = status.id
                      AND reblog.account_id = $1
                )
                OR EXISTS (
                    SELECT 1 FROM local_status_bookmark bookmark
                    WHERE bookmark.status_id = status.id
                      AND bookmark.account_id = $1
                )
              )
              AND (
                status.visibility IN ('public', 'unlisted')
                OR author.id = $1
                OR (
                    status.visibility = 'private'
                    AND (
                        EXISTS (
                            SELECT 1 FROM local_follow follow
                            WHERE follow.follower_account_id = $1
                              AND follow.followed_account_id = author.id
                        )
                        OR EXISTS (
                            SELECT 1 FROM local_status_local_recipient recipient
                            WHERE recipient.status_id = status.id
                              AND recipient.account_id = $1
                        )
                    )
                )
                OR (
                    status.visibility = 'direct'
                    AND EXISTS (
                        SELECT 1 FROM local_status_local_recipient recipient
                        WHERE recipient.status_id = status.id
                          AND recipient.account_id = $1
                    )
                )
              )
              AND (
                author.id = $1
                OR NOT EXISTS (
                    SELECT 1 FROM local_account_block block
                    WHERE (block.account_id = $1 AND block.target_account_id = author.id)
                       OR (block.account_id = author.id AND block.target_account_id = $1)
                )
              )
              AND (
                author.id = $1
                OR NOT EXISTS (
                    SELECT 1 FROM local_account_mute mute
                    WHERE mute.account_id = $1
                      AND mute.target_account_id = author.id
                      AND (mute.expires_at IS NULL OR mute.expires_at > now())
                )
              )
            UNION ALL
            SELECT 'remote'::text, status.id
            FROM status_search_document document
            JOIN remote_status status ON status.id = document.remote_status_id
            JOIN remote_actor author ON author.id = status.remote_actor_id
            WHERE $4
              AND document.document ILIKE '%' || $2 || '%' ESCAPE '\'
              AND status.deleted_at IS NULL
              AND author.deleted_at IS NULL
              AND author.limited_at IS NULL
              AND author.suspended_at IS NULL
              AND ($3::uuid IS NULL OR author.id = $3)
              AND NOT EXISTS (
                SELECT 1
                FROM jsonb_array_elements_text($5::jsonb) blocked(domain)
                WHERE author.domain = blocked.domain
                   OR author.domain LIKE '%.' || blocked.domain
              )
              AND (
                EXISTS (
                    SELECT 1 FROM remote_status_local_mention mention
                    WHERE mention.remote_status_id = status.id
                      AND mention.account_id = $1 AND mention.active
                )
                OR EXISTS (
                    SELECT 1 FROM local_remote_status_favourite favourite
                    WHERE favourite.remote_status_id = status.id
                      AND favourite.local_account_id = $1
                )
                OR EXISTS (
                    SELECT 1 FROM local_remote_status_reblog reblog
                    WHERE reblog.remote_status_id = status.id
                      AND reblog.local_account_id = $1
                )
              )
              AND (
                status.visibility IN ('public', 'unlisted')
                OR (
                    status.visibility = 'private'
                    AND (
                        EXISTS (
                            SELECT 1 FROM remote_following follow
                            WHERE follow.local_account_id = $1
                              AND follow.remote_actor_id = author.id
                              AND follow.state = 'accepted'
                              AND follow.deactivated_at IS NULL
                        )
                        OR EXISTS (
                            SELECT 1 FROM remote_status_local_recipient recipient
                            WHERE recipient.remote_status_id = status.id
                              AND recipient.account_id = $1
                        )
                    )
                )
                OR (
                    status.visibility = 'direct'
                    AND EXISTS (
                        SELECT 1 FROM remote_status_local_recipient recipient
                        WHERE recipient.remote_status_id = status.id
                          AND recipient.account_id = $1
                    )
                )
              )
              AND NOT EXISTS (
                SELECT 1 FROM local_remote_account_block block
                WHERE block.local_account_id = $1
                  AND block.remote_actor_id = author.id
              )
              AND NOT EXISTS (
                SELECT 1 FROM local_remote_account_mute mute
                WHERE mute.local_account_id = $1
                  AND mute.remote_actor_id = author.id
                  AND (mute.expires_at IS NULL OR mute.expires_at > now())
              )
        )
        SELECT status_kind, status_id
        FROM candidates
        WHERE ($6::uuid IS NULL OR status_id > $6)
          AND ($7::uuid IS NULL OR status_id < $7)
        ORDER BY status_id DESC
        LIMIT $8 OFFSET $9
        "#,
        vec![
            options.viewer_account_id.0.into(),
            escaped_query.into(),
            options.account_id.map(|id| id.0).into(),
            options.include_remote.into(),
            blocked_domains.into(),
            options.min_id.map(|id| id.0).into(),
            options.max_id.map(|id| id.0).into(),
            (page_query_limit(options.limit) as i64).into(),
            (options.offset as i64).into(),
        ],
    ))
    .all(db)
    .await?;

    let has_more = rows.len() > options.limit as usize;
    let mut rows = rows;
    if has_more {
        rows.truncate(options.limit as usize);
    }
    let first_cursor = rows.first().map(|row| row.status_id);
    let last_cursor = rows.last().map(|row| row.status_id);
    let local_ids = rows
        .iter()
        .filter(|row| row.status_kind == StatusSearchKind::Local)
        .map(|row| row.status_id)
        .collect::<Vec<_>>();
    let remote_ids = rows
        .iter()
        .filter(|row| row.status_kind == StatusSearchKind::Remote)
        .map(|row| row.status_id)
        .collect::<Vec<_>>();
    let local = local_status::Entity::find()
        .filter(local_status::Column::Id.is_in(local_ids))
        .all(db)
        .await?;
    let remote = remote_status::Entity::find()
        .filter(remote_status::Column::Id.is_in(remote_ids))
        .all(db)
        .await?;

    let mut local = local
        .into_iter()
        .map(|model| local_status_from_model(model).map(|status| (status.id.0, status)))
        .collect::<Result<HashMap<_, _>>>()?;
    let mut remote = remote
        .into_iter()
        .map(|model| remote_status_from_model(model).map(|status| (status.id.0, status)))
        .collect::<Result<HashMap<_, _>>>()?;
    let items = rows
        .into_iter()
        .filter_map(|row| match row.status_kind {
            StatusSearchKind::Local => local.remove(&row.status_id).map(StatusContextItem::Local),
            StatusSearchKind::Remote => {
                remote.remove(&row.status_id).map(StatusContextItem::Remote)
            }
        })
        .collect();
    Ok(StatusSearchPage {
        items,
        first_cursor,
        last_cursor,
        has_more,
    })
}

/// Count local accounts following this account.
pub async fn count_local_followers(
    db: &impl ConnectionTrait,
    account_id: AccountId,
) -> Result<u64> {
    Ok(local_follow::Entity::find()
        .filter(local_follow::Column::FollowedAccountId.eq(account_id.0))
        .count(db)
        .await?)
}

/// Count accepted remote actors following this local account.
pub async fn count_remote_followers(
    db: &impl ConnectionTrait,
    account_id: AccountId,
) -> Result<u64> {
    Ok(db.query_one(Statement::from_sql_and_values(DatabaseBackend::Postgres, "SELECT count(*) AS count FROM remote_follow WHERE local_account_id = $1 AND state = 'accepted'", vec![account_id.0.into()])).await?.map(|row| row.try_get::<i64>("", "count")).transpose()?.unwrap_or(0) as u64)
}

/// List accepted remote followers that must receive activities from a local actor.
pub async fn accepted_remote_followers(
    db: &impl ConnectionTrait,
    account_id: AccountId,
) -> Result<Vec<RemoteActor>> {
    let actor_ids = accepted_remote_follower_ids(db, account_id)
        .await?
        .into_iter()
        .map(|id| id.0)
        .collect::<Vec<_>>();
    if actor_ids.is_empty() {
        return Ok(Vec::new());
    }
    Ok(remote_actor::Entity::find()
        .filter(remote_actor::Column::Id.is_in(actor_ids))
        .all(db)
        .await?
        .into_iter()
        .map(remote_actor_from_model)
        .collect())
}

/// List accepted remote follower identifiers through a caller-owned transaction.
pub async fn accepted_remote_follower_ids(
    db: &impl ConnectionTrait,
    account_id: AccountId,
) -> Result<Vec<AccountId>> {
    Ok(remote_follow::Entity::find()
        .filter(remote_follow::Column::LocalAccountId.eq(account_id.0))
        .filter(remote_follow::Column::State.eq(RemoteFollowState::Accepted))
        .all(db)
        .await?
        .into_iter()
        .map(|follow| AccountId(follow.remote_actor_id))
        .collect())
}

/// Count local accounts this account follows.
pub async fn count_local_following(
    db: &impl ConnectionTrait,
    account_id: AccountId,
) -> Result<u64> {
    Ok(local_follow::Entity::find()
        .filter(local_follow::Column::FollowerAccountId.eq(account_id.0))
        .count(db)
        .await?)
}

/// Return whether one local account follows another.
pub async fn local_follow_relationship(
    db: &impl ConnectionTrait,
    follower_account_id: AccountId,
    followed_account_id: AccountId,
) -> Result<Option<LocalFollow>> {
    let follow = local_follow::Entity::find_by_id((follower_account_id.0, followed_account_id.0))
        .one(db)
        .await?;

    Ok(follow.map(local_follow_from_model))
}

/// List local follower ids for streaming delivery.
pub async fn local_follower_ids_for_account(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    include_reblog_muted: bool,
) -> Result<Vec<AccountId>> {
    let mut query = local_follow::Entity::find()
        .filter(local_follow::Column::FollowedAccountId.eq(account_id.0));
    if !include_reblog_muted {
        query = query.filter(local_follow::Column::ShowReblogs.eq(true));
    }
    let follows = query.all(db).await?;

    Ok(follows
        .into_iter()
        .map(|follow| AccountId(follow.follower_account_id))
        .collect())
}

/// List local followers that opted into new-post notifications.
pub async fn local_notified_follower_ids_for_account(
    db: &impl ConnectionTrait,
    account_id: AccountId,
) -> Result<Vec<AccountId>> {
    let follows = local_follow::Entity::find()
        .filter(local_follow::Column::FollowedAccountId.eq(account_id.0))
        .filter(local_follow::Column::Notify.eq(true))
        .all(db)
        .await?;
    Ok(follows
        .into_iter()
        .map(|follow| AccountId(follow.follower_account_id))
        .collect())
}

/// Follow a local account, updating follow options when it already exists.
pub async fn follow_local_account(
    db: &impl ConnectionTrait,
    follower_account_id: AccountId,
    followed_account_id: AccountId,
    show_reblogs: bool,
    notify: bool,
) -> Result<LocalFollow> {
    if follower_account_id == followed_account_id {
        return Err(AccountRelationshipError::SelfFollow.into());
    }
    if find_local_account_by_id(db, followed_account_id)
        .await?
        .is_none()
    {
        return Err(AccountRelationshipError::FollowTargetNotFound.into());
    }
    if local_accounts_are_blocked(db, follower_account_id, followed_account_id).await? {
        return Err(AccountRelationshipError::FollowBlocked.into());
    }

    let now = OffsetDateTime::now_utc();
    let follow =
        match local_follow::Entity::find_by_id((follower_account_id.0, followed_account_id.0))
            .one(db)
            .await?
        {
            Some(model) => {
                let mut active = model.into_active_model();
                active.show_reblogs = Set(show_reblogs);
                active.notify = Set(notify);
                active.updated_at = Set(now);
                active.update(db).await?
            }
            None => {
                local_follow::ActiveModel {
                    id: Set(Uuid::now_v7()),
                    follower_account_id: Set(follower_account_id.0),
                    followed_account_id: Set(followed_account_id.0),
                    show_reblogs: Set(show_reblogs),
                    notify: Set(notify),
                    created_at: Set(now),
                    updated_at: Set(now),
                }
                .insert(db)
                .await?
            }
        };

    Ok(local_follow_from_model(follow))
}

/// Remove a local follow relationship when it exists.
pub async fn unfollow_local_account(
    db: &impl ConnectionTrait,
    follower_account_id: AccountId,
    followed_account_id: AccountId,
) -> Result<()> {
    remove_local_account_from_owned_lists(db, follower_account_id, followed_account_id).await?;
    if let Some(model) =
        local_follow::Entity::find_by_id((follower_account_id.0, followed_account_id.0))
            .one(db)
            .await?
    {
        model.into_active_model().delete(db).await?;
    }

    Ok(())
}

/// Block a local account and sever any follow relationships between the accounts.
pub async fn block_local_account(
    txn: &sea_orm::DatabaseTransaction,
    account_id: AccountId,
    target_account_id: AccountId,
) -> Result<()> {
    ensure_local_relation_target(txn, account_id, target_account_id).await?;

    if local_account_block::Entity::find_by_id((account_id.0, target_account_id.0))
        .one(txn)
        .await?
        .is_none()
    {
        let now = OffsetDateTime::now_utc();
        local_account_block::ActiveModel {
            id: Set(Uuid::now_v7()),
            account_id: Set(account_id.0),
            target_account_id: Set(target_account_id.0),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(txn)
        .await?;
    }

    local_follow::Entity::delete_many()
        .filter(local_follow::Column::FollowerAccountId.eq(account_id.0))
        .filter(local_follow::Column::FollowedAccountId.eq(target_account_id.0))
        .exec(txn)
        .await?;
    local_follow::Entity::delete_many()
        .filter(local_follow::Column::FollowerAccountId.eq(target_account_id.0))
        .filter(local_follow::Column::FollowedAccountId.eq(account_id.0))
        .exec(txn)
        .await?;
    remove_local_account_from_owned_lists(txn, account_id, target_account_id).await?;
    remove_local_account_from_owned_lists(txn, target_account_id, account_id).await?;
    Ok(())
}

/// Remove a local account block when it exists.
pub async fn unblock_local_account<C>(
    db: &C,
    account_id: AccountId,
    target_account_id: AccountId,
) -> Result<()>
where
    C: ConnectionTrait,
{
    if let Some(model) =
        local_account_block::Entity::find_by_id((account_id.0, target_account_id.0))
            .one(db)
            .await?
    {
        model.into_active_model().delete(db).await?;
    }

    Ok(())
}

/// Mute a local account, replacing notification and duration settings when it already exists.
pub async fn mute_local_account<C>(
    db: &C,
    account_id: AccountId,
    target_account_id: AccountId,
    notifications: bool,
    duration_seconds: u64,
) -> Result<LocalAccountMute>
where
    C: ConnectionTrait,
{
    ensure_local_relation_target(db, account_id, target_account_id).await?;
    let now = OffsetDateTime::now_utc();
    let expires_at = if duration_seconds == 0 {
        None
    } else {
        let seconds = i64::try_from(duration_seconds)
            .map_err(|_| RoostyError::InvalidInput("mute duration is too large".to_owned()))?;
        Some(now + Duration::seconds(seconds))
    };
    let mute = match local_account_mute::Entity::find_by_id((account_id.0, target_account_id.0))
        .one(db)
        .await?
    {
        Some(model) => {
            let mut active = model.into_active_model();
            active.notifications = Set(notifications);
            active.expires_at = Set(expires_at);
            active.updated_at = Set(now);
            active.update(db).await?
        }
        None => {
            local_account_mute::ActiveModel {
                id: Set(Uuid::now_v7()),
                account_id: Set(account_id.0),
                target_account_id: Set(target_account_id.0),
                notifications: Set(notifications),
                expires_at: Set(expires_at),
                created_at: Set(now),
                updated_at: Set(now),
            }
            .insert(db)
            .await?
        }
    };

    Ok(local_account_mute_from_model(mute))
}

/// Remove a local account mute when it exists.
pub async fn unmute_local_account<C>(
    db: &C,
    account_id: AccountId,
    target_account_id: AccountId,
) -> Result<()>
where
    C: ConnectionTrait,
{
    if let Some(model) = local_account_mute::Entity::find_by_id((account_id.0, target_account_id.0))
        .one(db)
        .await?
    {
        model.into_active_model().delete(db).await?;
    }

    Ok(())
}

/// Return whether either of two local accounts blocks the other.
pub async fn local_accounts_are_blocked(
    db: &impl ConnectionTrait,
    first_account_id: AccountId,
    second_account_id: AccountId,
) -> Result<bool> {
    if first_account_id == second_account_id {
        return Ok(false);
    }

    Ok(local_account_block::Entity::find()
        .filter(
            Condition::any()
                .add(
                    Condition::all()
                        .add(local_account_block::Column::AccountId.eq(first_account_id.0))
                        .add(local_account_block::Column::TargetAccountId.eq(second_account_id.0)),
                )
                .add(
                    Condition::all()
                        .add(local_account_block::Column::AccountId.eq(second_account_id.0))
                        .add(local_account_block::Column::TargetAccountId.eq(first_account_id.0)),
                ),
        )
        .one(db)
        .await?
        .is_some())
}

/// Return whether one local account directly blocks another.
pub async fn local_account_blocks(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    target_account_id: AccountId,
) -> Result<bool> {
    Ok(
        local_account_block::Entity::find_by_id((account_id.0, target_account_id.0))
            .one(db)
            .await?
            .is_some(),
    )
}

/// Return an active local mute relationship, ignoring rows whose duration has elapsed.
pub async fn active_local_account_mute(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    target_account_id: AccountId,
) -> Result<Option<LocalAccountMute>> {
    let now = OffsetDateTime::now_utc();
    let mute = local_account_mute::Entity::find_by_id((account_id.0, target_account_id.0))
        .filter(
            Condition::any()
                .add(local_account_mute::Column::ExpiresAt.is_null())
                .add(local_account_mute::Column::ExpiresAt.gt(now)),
        )
        .one(db)
        .await?;

    Ok(mute.map(local_account_mute_from_model))
}

/// Return whether a viewer should hide a local account from personalized timelines.
pub async fn local_account_is_hidden_for_viewer(
    db: &impl ConnectionTrait,
    viewer_account_id: AccountId,
    target_account_id: AccountId,
) -> Result<bool> {
    Ok(
        local_accounts_are_blocked(db, viewer_account_id, target_account_id).await?
            || active_local_account_mute(db, viewer_account_id, target_account_id)
                .await?
                .is_some(),
    )
}

/// Return whether a local interaction may create a notification for its recipient.
pub async fn local_account_allows_notification(
    db: &impl ConnectionTrait,
    recipient_account_id: AccountId,
    actor_account_id: AccountId,
) -> Result<bool> {
    if local_accounts_are_blocked(db, recipient_account_id, actor_account_id).await? {
        return Ok(false);
    }

    Ok(
        !active_local_account_mute(db, recipient_account_id, actor_account_id)
            .await?
            .is_some_and(|mute| mute.notifications),
    )
}

/// Return block targets for an account in either relationship direction.
pub async fn blocked_local_account_ids_for_account(
    db: &impl ConnectionTrait,
    account_id: AccountId,
) -> Result<Vec<AccountId>> {
    let rows = local_account_block::Entity::find()
        .filter(
            local_account_block::Column::AccountId
                .eq(account_id.0)
                .or(local_account_block::Column::TargetAccountId.eq(account_id.0)),
        )
        .all(db)
        .await?;

    Ok(rows
        .into_iter()
        .map(|row| {
            if row.account_id == account_id.0 {
                AccountId(row.target_account_id)
            } else {
                AccountId(row.account_id)
            }
        })
        .collect())
}

/// Return local accounts hidden from one account's personalized timelines.
pub async fn hidden_local_account_ids_for_account(
    db: &impl ConnectionTrait,
    account_id: AccountId,
) -> Result<Vec<AccountId>> {
    let mut account_ids = blocked_local_account_ids_for_account(db, account_id).await?;
    let now = OffsetDateTime::now_utc();
    let mutes = local_account_mute::Entity::find()
        .filter(local_account_mute::Column::AccountId.eq(account_id.0))
        .filter(
            Condition::any()
                .add(local_account_mute::Column::ExpiresAt.is_null())
                .add(local_account_mute::Column::ExpiresAt.gt(now)),
        )
        .all(db)
        .await?;
    account_ids.extend(
        mutes
            .into_iter()
            .map(|mute| AccountId(mute.target_account_id)),
    );
    account_ids.sort_unstable_by_key(|account_id| account_id.0);
    account_ids.dedup();

    Ok(account_ids)
}

/// List active locally muted accounts with Mastodon cursor pagination.
pub async fn muted_local_accounts_for_account(
    db: &DbConnection,
    account_id: AccountId,
    limit: u64,
    cursor: CollectionCursor,
) -> Result<CollectionPage<LocalAccount>> {
    let now = OffsetDateTime::now_utc();
    let rows = local_account_mute::Entity::find()
        .filter(local_account_mute::Column::AccountId.eq(account_id.0))
        .filter(
            Condition::any()
                .add(local_account_mute::Column::ExpiresAt.is_null())
                .add(local_account_mute::Column::ExpiresAt.gt(now)),
        )
        .apply_collection_cursor(cursor)
        .order_by_desc(local_account_mute::Column::Id)
        .limit(page_query_limit(limit))
        .all(db)
        .await?;
    let (rows, has_more) = trim_to_page(rows, limit);
    let first_cursor = rows.first().map(|row| row.id);
    let last_cursor = rows.last().map(|row| row.id);
    let account_ids = rows
        .into_iter()
        .map(|row| AccountId(row.target_account_id))
        .collect();

    Ok(CollectionPage {
        items: local_accounts_by_id(db, account_ids).await?,
        first_cursor,
        last_cursor,
        has_more,
    })
}

/// List locally blocked accounts with Mastodon cursor pagination.
pub async fn blocked_local_accounts_for_account(
    db: &DbConnection,
    account_id: AccountId,
    limit: u64,
    cursor: CollectionCursor,
) -> Result<CollectionPage<LocalAccount>> {
    let rows = local_account_block::Entity::find()
        .filter(local_account_block::Column::AccountId.eq(account_id.0))
        .apply_collection_cursor(cursor)
        .order_by_desc(local_account_block::Column::Id)
        .limit(page_query_limit(limit))
        .all(db)
        .await?;
    let (rows, has_more) = trim_to_page(rows, limit);
    let first_cursor = rows.first().map(|row| row.id);
    let last_cursor = rows.last().map(|row| row.id);
    let account_ids = rows
        .into_iter()
        .map(|row| AccountId(row.target_account_id))
        .collect();

    Ok(CollectionPage {
        items: local_accounts_by_id(db, account_ids).await?,
        first_cursor,
        last_cursor,
        has_more,
    })
}

/// Atomically create a local-to-remote block, sever bilateral follows, dismiss
/// prior notifications from the actor, and enqueue its first delivery.
///
/// Returns `true` only for the request that inserted the relationship. This
/// makes concurrent repeated block requests share one stable activity.
pub async fn block_remote_account<C>(
    db: &C,
    local_account_id: AccountId,
    remote_actor_id: AccountId,
    activity_id: &str,
    job: NewJob,
) -> Result<bool>
where
    C: ConnectionTrait,
{
    lock_local_remote_relation(db, local_account_id, remote_actor_id).await?;
    if remote_actor::Entity::find_by_id(remote_actor_id.0)
        .one(db)
        .await?
        .is_none()
    {
        return Err(AccountRelationshipError::ModerationTargetNotFound.into());
    }
    let now = OffsetDateTime::now_utc();
    let inserted = db
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"INSERT INTO local_remote_account_block
           (id, local_account_id, remote_actor_id, activity_id, created_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, $5)
           ON CONFLICT (local_account_id, remote_actor_id) DO NOTHING
           RETURNING id"#,
            vec![
                Uuid::now_v7().into(),
                local_account_id.0.into(),
                remote_actor_id.0.into(),
                activity_id.to_owned().into(),
                now.into(),
            ],
        ))
        .await?
        .is_some();

    remote_following::Entity::delete_many()
        .filter(remote_following::Column::LocalAccountId.eq(local_account_id.0))
        .filter(remote_following::Column::RemoteActorId.eq(remote_actor_id.0))
        .exec(db)
        .await?;
    remove_remote_account_from_owned_lists(db, local_account_id, remote_actor_id).await?;
    remote_follow::Entity::delete_many()
        .filter(remote_follow::Column::LocalAccountId.eq(local_account_id.0))
        .filter(remote_follow::Column::RemoteActorId.eq(remote_actor_id.0))
        .exec(db)
        .await?;
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "UPDATE local_notification SET dismissed_at = $3 WHERE account_id = $1 AND remote_actor_id = $2 AND dismissed_at IS NULL",
        vec![local_account_id.0.into(), remote_actor_id.0.into(), now.into()],
    )).await?;
    if inserted {
        enqueue_job_on_connection(db, job).await?;
    }
    Ok(inserted)
}

/// Find a local account's block of a remote actor.
pub async fn find_local_remote_account_block<C>(
    db: &C,
    local_account_id: AccountId,
    remote_actor_id: AccountId,
) -> Result<Option<LocalRemoteAccountBlock>>
where
    C: ConnectionTrait,
{
    Ok(
        local_remote_account_block::Entity::find_by_id((local_account_id.0, remote_actor_id.0))
            .one(db)
            .await?
            .map(|row| LocalRemoteAccountBlock {
                local_account_id: AccountId(row.local_account_id),
                remote_actor_id: AccountId(row.remote_actor_id),
                activity_id: row.activity_id,
            }),
    )
}

/// Delete a local-to-remote block and enqueue its `Undo` in the same transaction.
pub async fn unblock_remote_account<C>(
    db: &C,
    local_account_id: AccountId,
    remote_actor_id: AccountId,
    job: NewJob,
) -> Result<bool>
where
    C: ConnectionTrait,
{
    lock_local_remote_relation(db, local_account_id, remote_actor_id).await?;
    let result =
        local_remote_account_block::Entity::delete_by_id((local_account_id.0, remote_actor_id.0))
            .exec(db)
            .await?;
    if result.rows_affected == 1 {
        enqueue_job_on_connection(db, job).await?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Create or update a local-only mute of a remote actor.
pub async fn mute_remote_account<C>(
    db: &C,
    local_account_id: AccountId,
    remote_actor_id: AccountId,
    notifications: bool,
    duration_seconds: u64,
) -> Result<LocalRemoteAccountMute>
where
    C: ConnectionTrait,
{
    if remote_actor::Entity::find_by_id(remote_actor_id.0)
        .one(db)
        .await?
        .is_none()
    {
        return Err(AccountRelationshipError::ModerationTargetNotFound.into());
    }
    let now = OffsetDateTime::now_utc();
    let expires_at =
        if duration_seconds == 0 {
            None
        } else {
            Some(
                now + Duration::seconds(i64::try_from(duration_seconds).map_err(|_| {
                    RoostyError::InvalidInput("mute duration is too large".to_owned())
                })?),
            )
        };
    let row = match local_remote_account_mute::Entity::find_by_id((
        local_account_id.0,
        remote_actor_id.0,
    ))
    .one(db)
    .await?
    {
        Some(row) => {
            let mut active = row.into_active_model();
            active.notifications = Set(notifications);
            active.expires_at = Set(expires_at);
            active.updated_at = Set(now);
            active.update(db).await?
        }
        None => {
            local_remote_account_mute::ActiveModel {
                id: Set(Uuid::now_v7()),
                local_account_id: Set(local_account_id.0),
                remote_actor_id: Set(remote_actor_id.0),
                notifications: Set(notifications),
                expires_at: Set(expires_at),
                created_at: Set(now),
                updated_at: Set(now),
            }
            .insert(db)
            .await?
        }
    };
    Ok(local_remote_account_mute_from_model(row))
}

/// Remove a local-only mute of a remote actor.
pub async fn unmute_remote_account<C>(
    db: &C,
    local_account_id: AccountId,
    remote_actor_id: AccountId,
) -> Result<()>
where
    C: ConnectionTrait,
{
    local_remote_account_mute::Entity::delete_by_id((local_account_id.0, remote_actor_id.0))
        .exec(db)
        .await?;
    Ok(())
}

/// Return an unexpired remote mute.
pub async fn active_local_remote_account_mute<C>(
    db: &C,
    local_account_id: AccountId,
    remote_actor_id: AccountId,
) -> Result<Option<LocalRemoteAccountMute>>
where
    C: ConnectionTrait,
{
    let row =
        local_remote_account_mute::Entity::find_by_id((local_account_id.0, remote_actor_id.0))
            .filter(
                Condition::any()
                    .add(local_remote_account_mute::Column::ExpiresAt.is_null())
                    .add(
                        local_remote_account_mute::Column::ExpiresAt.gt(OffsetDateTime::now_utc()),
                    ),
            )
            .one(db)
            .await?;
    Ok(row.map(local_remote_account_mute_from_model))
}

/// Return whether either side has blocked the local/remote relationship.
pub async fn local_remote_accounts_are_blocked<C>(
    db: &C,
    local_account_id: AccountId,
    remote_actor_id: AccountId,
) -> Result<bool>
where
    C: ConnectionTrait,
{
    Ok(
        find_local_remote_account_block(db, local_account_id, remote_actor_id)
            .await?
            .is_some()
            || remote_local_account_block::Entity::find_by_id((
                remote_actor_id.0,
                local_account_id.0,
            ))
            .one(db)
            .await?
            .is_some(),
    )
}

/// Return whether a remote actor directly blocks a local account.
pub async fn remote_actor_blocks_local_account<C>(
    db: &C,
    remote_actor_id: AccountId,
    local_account_id: AccountId,
) -> Result<bool>
where
    C: ConnectionTrait,
{
    Ok(
        remote_local_account_block::Entity::find_by_id((remote_actor_id.0, local_account_id.0))
            .one(db)
            .await?
            .is_some(),
    )
}

/// Return whether a remote actor is hidden from a viewer's personalized surfaces.
pub async fn remote_account_is_hidden_for_viewer<C>(
    db: &C,
    viewer: AccountId,
    actor: AccountId,
) -> Result<bool>
where
    C: ConnectionTrait,
{
    Ok(find_local_remote_account_block(db, viewer, actor)
        .await?
        .is_some()
        || active_local_remote_account_mute(db, viewer, actor)
            .await?
            .is_some())
}

/// Return active remote actors hidden from personalized surfaces for one viewer.
pub async fn hidden_remote_actor_ids_for_account<C>(
    db: &C,
    account_id: AccountId,
) -> Result<Vec<AccountId>>
where
    C: ConnectionTrait,
{
    let now = OffsetDateTime::now_utc();
    let blocks = local_remote_account_block::Entity::find()
        .filter(local_remote_account_block::Column::LocalAccountId.eq(account_id.0))
        .all(db)
        .await?;
    let mutes = local_remote_account_mute::Entity::find()
        .filter(local_remote_account_mute::Column::LocalAccountId.eq(account_id.0))
        .filter(
            Condition::any()
                .add(local_remote_account_mute::Column::ExpiresAt.is_null())
                .add(local_remote_account_mute::Column::ExpiresAt.gt(now)),
        )
        .all(db)
        .await?;
    let mut ids = blocks
        .into_iter()
        .map(|row| AccountId(row.remote_actor_id))
        .chain(mutes.into_iter().map(|row| AccountId(row.remote_actor_id)))
        .collect::<Vec<_>>();
    ids.sort_unstable_by_key(|id| id.0);
    ids.dedup();
    Ok(ids)
}

/// Reconcile cached actors covered by operator domain suspension.
///
/// Cached actors and statuses are retained, while follows are permanently
/// severed, notifications dismissed, and pending deliveries completed.
pub async fn reconcile_suspended_remote_domains<C>(db: &C, domains: &[String]) -> Result<u64>
where
    C: ConnectionTrait,
{
    if domains.is_empty() {
        return Ok(0);
    }
    let actors = remote_actor::Entity::find().all(db).await?;
    let actor_ids = actors
        .into_iter()
        .filter(|actor| {
            domains.iter().any(|blocked| {
                actor.domain == *blocked
                    || actor
                        .domain
                        .strip_suffix(blocked)
                        .is_some_and(|prefix| prefix.ends_with('.'))
            })
        })
        .map(|actor| actor.id)
        .collect::<Vec<_>>();
    if actor_ids.is_empty() {
        return Ok(0);
    }
    let now = OffsetDateTime::now_utc();
    remote_follow::Entity::delete_many()
        .filter(remote_follow::Column::RemoteActorId.is_in(actor_ids.clone()))
        .exec(db)
        .await?;
    remote_following::Entity::delete_many()
        .filter(remote_following::Column::RemoteActorId.is_in(actor_ids.clone()))
        .exec(db)
        .await?;
    local_list_remote_member::Entity::delete_many()
        .filter(local_list_remote_member::Column::RemoteActorId.is_in(actor_ids.clone()))
        .exec(db)
        .await?;
    db.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,
        "UPDATE local_notification SET dismissed_at = $2 WHERE remote_actor_id = ANY($1) AND dismissed_at IS NULL",
        vec![actor_ids.clone().into(), now.into()])).await?;
    let actor_id_strings = actor_ids.iter().map(Uuid::to_string).collect::<Vec<_>>();
    db.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,
        "UPDATE job SET completed_at = $2, locked_at = NULL, locked_by = NULL, claim_id = NULL, last_error = 'remote domain suspended' WHERE completed_at IS NULL AND payload->>'remote_actor_id' = ANY($1)",
        vec![actor_id_strings.into(), now.into()])).await?;
    Ok(actor_ids.len() as u64)
}

/// Return whether a remote actor may create a notification for a local account.
pub async fn remote_account_allows_notification<C>(
    db: &C,
    recipient: AccountId,
    actor: AccountId,
) -> Result<bool>
where
    C: ConnectionTrait,
{
    if local_remote_accounts_are_blocked(db, recipient, actor).await? {
        return Ok(false);
    }
    Ok(!active_local_remote_account_mute(db, recipient, actor)
        .await?
        .is_some_and(|mute| mute.notifications))
}

/// Persist a validated inbound block and sever all relationships for its pair.
pub async fn process_remote_block<C>(
    db: &C,
    remote_actor_id: AccountId,
    local_account_id: AccountId,
    activity_id: &str,
) -> Result<bool>
where
    C: ConnectionTrait,
{
    lock_local_remote_relation(db, local_account_id, remote_actor_id).await?;
    let inserted = db.query_one(Statement::from_sql_and_values(DatabaseBackend::Postgres,
        r#"INSERT INTO remote_local_account_block (id, remote_actor_id, local_account_id, activity_id)
           VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING RETURNING id"#,
        vec![Uuid::now_v7().into(), remote_actor_id.0.into(), local_account_id.0.into(), activity_id.to_owned().into()])).await?.is_some();
    remote_follow::Entity::delete_many()
        .filter(remote_follow::Column::RemoteActorId.eq(remote_actor_id.0))
        .filter(remote_follow::Column::LocalAccountId.eq(local_account_id.0))
        .exec(db)
        .await?;
    remote_following::Entity::delete_many()
        .filter(remote_following::Column::RemoteActorId.eq(remote_actor_id.0))
        .filter(remote_following::Column::LocalAccountId.eq(local_account_id.0))
        .exec(db)
        .await?;
    remove_remote_account_from_owned_lists(db, local_account_id, remote_actor_id).await?;
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "UPDATE local_notification SET dismissed_at = now() WHERE account_id = $1 AND remote_actor_id = $2 AND dismissed_at IS NULL",
        vec![local_account_id.0.into(), remote_actor_id.0.into()],
    )).await?;
    Ok(inserted)
}

/// Serialize block/follow changes for one local/remote pair across processes.
async fn lock_local_remote_relation<C>(db: &C, local: AccountId, remote: AccountId) -> Result<()>
where
    C: ConnectionTrait,
{
    let key = format!("{}:{}", local.0, remote.0);
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        vec![key.into()],
    ))
    .await?;
    Ok(())
}

/// Remove an inbound block only when the Undo names its currently stored activity.
pub async fn process_remote_undo_block<C>(
    db: &C,
    remote_actor_id: AccountId,
    local_account_id: AccountId,
    activity_id: &str,
) -> Result<bool>
where
    C: ConnectionTrait,
{
    lock_local_remote_relation(db, local_account_id, remote_actor_id).await?;
    let result = remote_local_account_block::Entity::delete_many()
        .filter(remote_local_account_block::Column::RemoteActorId.eq(remote_actor_id.0))
        .filter(remote_local_account_block::Column::LocalAccountId.eq(local_account_id.0))
        .filter(remote_local_account_block::Column::ActivityId.eq(activity_id))
        .exec(db)
        .await?;
    Ok(result.rows_affected == 1)
}

/// Resolve the local target of a currently active inbound block by activity identity.
pub async fn find_remote_local_block_by_activity<C>(
    db: &C,
    remote_actor_id: AccountId,
    activity_id: &str,
) -> Result<Option<AccountId>>
where
    C: ConnectionTrait,
{
    Ok(remote_local_account_block::Entity::find()
        .filter(remote_local_account_block::Column::RemoteActorId.eq(remote_actor_id.0))
        .filter(remote_local_account_block::Column::ActivityId.eq(activity_id))
        .one(db)
        .await?
        .map(|row| AccountId(row.local_account_id)))
}

/// List local and remote block targets in one UUIDv7 cursor order.
pub async fn blocked_accounts_for_account(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    limit: u64,
    cursor: CollectionCursor,
) -> Result<CollectionPage<FollowCollectionEntry>> {
    follow_collection_page(
        db,
        local_account_block::Entity::find()
            .filter(local_account_block::Column::AccountId.eq(account_id.0)),
        local_remote_account_block::Entity::find()
            .filter(local_remote_account_block::Column::LocalAccountId.eq(account_id.0)),
        limit,
        cursor,
        |row| (row.id, AccountId(row.target_account_id)),
        |row| (row.id, AccountId(row.remote_actor_id)),
    )
    .await
}

/// List active local and remote mute targets in one UUIDv7 cursor order.
pub async fn muted_accounts_for_account(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    limit: u64,
    cursor: CollectionCursor,
) -> Result<CollectionPage<FollowCollectionEntry>> {
    let now = OffsetDateTime::now_utc();
    follow_collection_page(
        db,
        local_account_mute::Entity::find()
            .filter(local_account_mute::Column::AccountId.eq(account_id.0))
            .filter(
                Condition::any()
                    .add(local_account_mute::Column::ExpiresAt.is_null())
                    .add(local_account_mute::Column::ExpiresAt.gt(now)),
            ),
        local_remote_account_mute::Entity::find()
            .filter(local_remote_account_mute::Column::LocalAccountId.eq(account_id.0))
            .filter(
                Condition::any()
                    .add(local_remote_account_mute::Column::ExpiresAt.is_null())
                    .add(local_remote_account_mute::Column::ExpiresAt.gt(now)),
            ),
        limit,
        cursor,
        |row| (row.id, AccountId(row.target_account_id)),
        |row| (row.id, AccountId(row.remote_actor_id)),
    )
    .await
}

/// Validate that a local relation has an existing, distinct target account.
async fn ensure_local_relation_target(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    target_account_id: AccountId,
) -> Result<()> {
    if account_id == target_account_id {
        return Err(AccountRelationshipError::SelfModeration.into());
    }
    if local_account::Entity::find_by_id(target_account_id.0)
        .one(db)
        .await?
        .is_none()
    {
        return Err(AccountRelationshipError::ModerationTargetNotFound.into());
    }

    Ok(())
}

/// Create or return an existing local notification for one logical event.
pub async fn notify_local_account(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    notification_type: LocalNotificationType,
    actor_account_id: AccountId,
    status_id: Option<StatusId>,
) -> Result<LocalNotification> {
    if account_id == actor_account_id {
        return Err(RoostyError::InvalidInput(
            "accounts cannot notify themselves".to_owned(),
        ));
    }
    let status_uuid = status_id.map(|id| id.0);
    if let Some(existing) = local_notification::Entity::find()
        .filter(local_notification::Column::AccountId.eq(account_id.0))
        .filter(local_notification::Column::NotificationType.eq(notification_type))
        .filter(local_notification::Column::ActorAccountId.eq(Some(actor_account_id.0)))
        .filter(match status_uuid {
            Some(status_id) => local_notification::Column::StatusId.eq(status_id),
            None => local_notification::Column::StatusId.is_null(),
        })
        .one(db)
        .await?
    {
        return Ok(local_notification_from_model(existing));
    }

    let model = local_notification::ActiveModel {
        id: Set(Uuid::now_v7()),
        account_id: Set(account_id.0),
        notification_type: Set(notification_type),
        actor_account_id: Set(Some(actor_account_id.0)),
        remote_actor_id: Set(None),
        status_id: Set(status_uuid),
        remote_status_id: Set(None),
        group_id: Set(None),
        filtered: Set(false),
        notification_request_id: Set(None),
        report_id: Set(None),
        created_at: Set(OffsetDateTime::now_utc()),
        dismissed_at: Set(None),
    }
    .insert(db)
    .await?;

    Ok(local_notification_from_model(model))
}

/// Create a local notification while applying Mastodon's filterable-type policy.
pub async fn notify_local_account_with_policy(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    notification_type: LocalNotificationType,
    actor_account_id: AccountId,
    status_id: Option<StatusId>,
) -> Result<Option<LocalNotification>> {
    if account_id == actor_account_id
        || !local_account_allows_notification(db, account_id, actor_account_id).await?
    {
        return Ok(None);
    }
    let filterable = matches!(
        notification_type,
        LocalNotificationType::Mention
            | LocalNotificationType::Reblog
            | LocalNotificationType::Follow
            | LocalNotificationType::FollowRequest
            | LocalNotificationType::Favourite
            | LocalNotificationType::Quote
    );
    if !filterable {
        return notify_local_account(
            db,
            account_id,
            notification_type,
            actor_account_id,
            status_id,
        )
        .await
        .map(Some);
    }
    let status_uuid = status_id.map(|id| id.0);
    if let Some(existing) = local_notification::Entity::find()
        .filter(local_notification::Column::AccountId.eq(account_id.0))
        .filter(local_notification::Column::NotificationType.eq(notification_type))
        .filter(local_notification::Column::ActorAccountId.eq(actor_account_id.0))
        .filter(match status_uuid {
            Some(status_id) => local_notification::Column::StatusId.eq(status_id),
            None => local_notification::Column::StatusId.is_null(),
        })
        .one(db)
        .await?
    {
        return Ok(Some(local_notification_from_model(existing)));
    }
    let action = local_notification_policy_action(
        db,
        account_id,
        actor_account_id,
        notification_type,
        status_id,
    )
    .await?;
    if action == NotificationPolicyAction::Drop {
        return Ok(None);
    }
    let request_id = if action == NotificationPolicyAction::Filter {
        if matches!(
            notification_type,
            LocalNotificationType::Mention | LocalNotificationType::Quote
        ) && let Some(status_id) = status_id
        {
            Some(
                upsert_notification_request(
                    db,
                    account_id,
                    NotificationActor::Local(actor_account_id),
                    status_id,
                )
                .await?,
            )
        } else {
            active_notification_request_id(
                db,
                account_id,
                NotificationActor::Local(actor_account_id),
            )
            .await?
        }
    } else {
        None
    };
    let model = local_notification::ActiveModel {
        id: Set(Uuid::now_v7()),
        account_id: Set(account_id.0),
        notification_type: Set(notification_type),
        actor_account_id: Set(Some(actor_account_id.0)),
        remote_actor_id: Set(None),
        status_id: Set(status_uuid),
        remote_status_id: Set(None),
        group_id: Set(None),
        filtered: Set(action == NotificationPolicyAction::Filter),
        notification_request_id: Set(request_id),
        report_id: Set(None),
        created_at: Set(OffsetDateTime::now_utc()),
        dismissed_at: Set(None),
    }
    .insert(db)
    .await?;
    Ok(Some(local_notification_from_model(model)))
}

/// Load the recipient's notification policy, creating the Mastodon defaults when necessary.
pub async fn notification_policy(
    db: &impl ConnectionTrait,
    account_id: AccountId,
) -> Result<NotificationPolicy> {
    let policy = if let Some(policy) = local_notification_policy::Entity::find_by_id(account_id.0)
        .one(db)
        .await?
    {
        policy
    } else {
        local_notification_policy::ActiveModel {
            account_id: Set(account_id.0),
            for_not_following: Set(NotificationPolicyAction::Accept),
            for_not_followers: Set(NotificationPolicyAction::Accept),
            for_new_accounts: Set(NotificationPolicyAction::Accept),
            for_private_mentions: Set(NotificationPolicyAction::Filter),
            for_limited_accounts: Set(NotificationPolicyAction::Filter),
            updated_at: Set(OffsetDateTime::now_utc()),
        }
        .insert(db)
        .await?
    };
    Ok(notification_policy_from_model(policy))
}

/// Apply a partial Mastodon notification-policy update.
pub async fn update_notification_policy(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    update: NotificationPolicyUpdate,
) -> Result<NotificationPolicy> {
    let policy = local_notification_policy::Entity::find_by_id(account_id.0)
        .one(db)
        .await?
        .ok_or_else(|| {
            RoostyError::InvalidInput("notification policy does not exist".to_owned())
        })?;
    let mut active = policy.into_active_model();
    if let Some(value) = update.for_not_following {
        active.for_not_following = Set(value);
    }
    if let Some(value) = update.for_not_followers {
        active.for_not_followers = Set(value);
    }
    if let Some(value) = update.for_new_accounts {
        active.for_new_accounts = Set(value);
    }
    if let Some(value) = update.for_private_mentions {
        active.for_private_mentions = Set(value);
    }
    if let Some(value) = update.for_limited_accounts {
        active.for_limited_accounts = Set(value);
    }
    active.updated_at = Set(OffsetDateTime::now_utc());
    Ok(notification_policy_from_model(active.update(db).await?))
}

fn strongest_notification_policy_action(
    actions: impl IntoIterator<Item = NotificationPolicyAction>,
) -> NotificationPolicyAction {
    actions
        .into_iter()
        .max_by_key(|action| match action {
            NotificationPolicyAction::Accept => 0,
            NotificationPolicyAction::Filter => 1,
            NotificationPolicyAction::Drop => 2,
        })
        .unwrap_or(NotificationPolicyAction::Accept)
}

/// Check up to 100 mixed local/remote ancestors for a private mention initiated by the recipient.
async fn recipient_started_private_thread(
    db: &impl ConnectionTrait,
    recipient: AccountId,
    actor: NotificationActor,
    local_parent_id: Option<Uuid>,
    remote_parent_id: Option<Uuid>,
) -> Result<bool> {
    if local_parent_id.is_none() && remote_parent_id.is_none() {
        return Ok(false);
    }
    let (local_actor_id, remote_actor_id) = match actor {
        NotificationActor::Local(id) => (Some(id.0), None),
        NotificationActor::Remote(id) => (None, Some(id.0)),
    };
    let row = db
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
            WITH RECURSIVE ancestors(local_id, remote_id, depth, path) AS (
                SELECT $1::uuid, $2::uuid, 0,
                    ARRAY[COALESCE('l:' || $1::text, 'r:' || $2::text)]::text[]
                UNION ALL
                SELECT next.local_id, next.remote_id, ancestors.depth + 1,
                    ancestors.path || next.key
                FROM ancestors
                CROSS JOIN LATERAL (
                    SELECT status.in_reply_to_id AS local_id,
                           status.in_reply_to_remote_status_id AS remote_id,
                           COALESCE('l:' || status.in_reply_to_id::text,
                                    'r:' || status.in_reply_to_remote_status_id::text) AS key
                    FROM local_status status WHERE status.id = ancestors.local_id
                    UNION ALL
                    SELECT status.in_reply_to_local_status_id,
                           status.in_reply_to_remote_status_id,
                           COALESCE('l:' || status.in_reply_to_local_status_id::text,
                                    'r:' || status.in_reply_to_remote_status_id::text)
                    FROM remote_status status WHERE status.id = ancestors.remote_id
                ) next
                WHERE ancestors.depth < 100
                  AND next.key IS NOT NULL
                  AND NOT next.key = ANY(ancestors.path)
            )
            SELECT EXISTS (
                SELECT 1 FROM ancestors
                JOIN local_status status ON status.id = ancestors.local_id
                WHERE status.account_id = $3
                  AND status.visibility = 'direct'
                  AND (($4::uuid IS NOT NULL AND EXISTS (
                        SELECT 1 FROM local_status_local_mention mention
                        WHERE mention.status_id = status.id AND mention.account_id = $4
                      )) OR ($5::uuid IS NOT NULL AND EXISTS (
                        SELECT 1 FROM local_status_remote_mention mention
                        WHERE mention.status_id = status.id AND mention.remote_actor_id = $5
                      )))
            ) AS trusted
            "#,
            vec![
                local_parent_id.into(),
                remote_parent_id.into(),
                recipient.0.into(),
                local_actor_id.into(),
                remote_actor_id.into(),
            ],
        ))
        .await?
        .ok_or_else(|| {
            RoostyError::InvalidInput("private notification policy result is missing".to_owned())
        })?;
    Ok(row.try_get("", "trusted")?)
}

async fn local_notification_policy_action(
    db: &impl ConnectionTrait,
    recipient: AccountId,
    actor: AccountId,
    notification_type: LocalNotificationType,
    status_id: Option<StatusId>,
) -> Result<NotificationPolicyAction> {
    if local_notification_permission::Entity::find()
        .filter(local_notification_permission::Column::AccountId.eq(recipient.0))
        .filter(local_notification_permission::Column::ActorAccountId.eq(actor.0))
        .one(db)
        .await?
        .is_some()
    {
        return Ok(NotificationPolicyAction::Accept);
    }
    let policy = notification_policy(db, recipient).await?;
    let recipient_follows_actor = local_follow::Entity::find_by_id((recipient.0, actor.0))
        .one(db)
        .await?
        .is_some();
    let actor_follow = local_follow::Entity::find_by_id((actor.0, recipient.0))
        .one(db)
        .await?;
    let actor_model = local_account::Entity::find_by_id(actor.0)
        .one(db)
        .await?
        .ok_or_else(|| RoostyError::InvalidInput("notification actor does not exist".to_owned()))?;
    let status = if let Some(status_id) = status_id {
        Some(
            local_status::Entity::find_by_id(status_id.0)
                .one(db)
                .await?
                .ok_or_else(|| {
                    RoostyError::InvalidInput("notification status does not exist".to_owned())
                })?,
        )
    } else {
        None
    };
    let now = OffsetDateTime::now_utc();
    let mut actions = Vec::new();
    if !recipient_follows_actor {
        actions.push(policy.for_not_following);
    }
    if actor_follow.is_none() {
        actions.push(policy.for_not_followers);
    }
    if !recipient_follows_actor {
        if actor_model.created_at > now - Duration::days(30) {
            actions.push(policy.for_new_accounts);
        }
        let user_initiated_private_thread = if status
            .as_ref()
            .is_some_and(|status| status.visibility == StatusVisibility::Direct)
        {
            let status = status.as_ref().ok_or_else(|| {
                RoostyError::InvalidInput("notification status does not exist".to_owned())
            })?;
            recipient_started_private_thread(
                db,
                recipient,
                NotificationActor::Local(actor),
                status.in_reply_to_id,
                status.in_reply_to_remote_status_id,
            )
            .await?
        } else {
            false
        };
        if notification_type == LocalNotificationType::Mention
            && status
                .as_ref()
                .is_some_and(|status| status.visibility == StatusVisibility::Direct)
            && !user_initiated_private_thread
        {
            actions.push(policy.for_private_mentions);
        }
        if actor_model.limited_at.is_some() {
            actions.push(policy.for_limited_accounts);
        }
    }
    // Mastodon only trusts an inbound follow after three days for the follower predicate.
    if actor_follow.is_some_and(|follow| follow.created_at > now - Duration::days(3)) {
        actions.push(policy.for_not_followers);
    }
    Ok(strongest_notification_policy_action(actions))
}

async fn remote_notification_policy_action<C>(
    db: &C,
    recipient: AccountId,
    actor: AccountId,
    notification_type: LocalNotificationType,
    status_id: Option<StatusId>,
) -> Result<NotificationPolicyAction>
where
    C: ConnectionTrait,
{
    if local_notification_permission::Entity::find()
        .filter(local_notification_permission::Column::AccountId.eq(recipient.0))
        .filter(local_notification_permission::Column::RemoteActorId.eq(actor.0))
        .one(db)
        .await?
        .is_some()
    {
        return Ok(NotificationPolicyAction::Accept);
    }
    let policy = notification_policy(db, recipient).await?;
    let recipient_follows_actor = remote_following::Entity::find()
        .filter(remote_following::Column::LocalAccountId.eq(recipient.0))
        .filter(remote_following::Column::RemoteActorId.eq(actor.0))
        .filter(remote_following::Column::State.eq(RemoteFollowState::Accepted))
        .filter(remote_following::Column::DeactivatedAt.is_null())
        .one(db)
        .await?
        .is_some();
    let actor_follow = remote_follow::Entity::find()
        .filter(remote_follow::Column::RemoteActorId.eq(actor.0))
        .filter(remote_follow::Column::LocalAccountId.eq(recipient.0))
        .filter(remote_follow::Column::State.eq(RemoteFollowState::Accepted))
        .one(db)
        .await?;
    let actor_model = remote_actor::Entity::find_by_id(actor.0)
        .one(db)
        .await?
        .ok_or_else(|| {
            RoostyError::InvalidInput("remote notification actor does not exist".to_owned())
        })?;
    let status = if let Some(status_id) = status_id {
        Some(
            remote_status::Entity::find_by_id(status_id.0)
                .one(db)
                .await?
                .ok_or_else(|| {
                    RoostyError::InvalidInput(
                        "remote notification status does not exist".to_owned(),
                    )
                })?,
        )
    } else {
        None
    };
    let now = OffsetDateTime::now_utc();
    let mut actions = Vec::new();
    if !recipient_follows_actor {
        actions.push(policy.for_not_following);
    }
    if actor_follow.is_none()
        || actor_follow
            .as_ref()
            .is_some_and(|follow| follow.created_at > now - Duration::days(3))
    {
        actions.push(policy.for_not_followers);
    }
    if !recipient_follows_actor {
        if actor_model
            .profile_created_at
            .unwrap_or(actor_model.created_at)
            > now - Duration::days(30)
        {
            actions.push(policy.for_new_accounts);
        }
        let user_initiated_private_thread = if status
            .as_ref()
            .is_some_and(|status| status.visibility == StatusVisibility::Direct)
        {
            let status = status.as_ref().ok_or_else(|| {
                RoostyError::InvalidInput("remote notification status does not exist".to_owned())
            })?;
            recipient_started_private_thread(
                db,
                recipient,
                NotificationActor::Remote(actor),
                status.in_reply_to_local_status_id,
                status.in_reply_to_remote_status_id,
            )
            .await?
        } else {
            false
        };
        if notification_type == LocalNotificationType::Mention
            && status
                .as_ref()
                .is_some_and(|status| status.visibility == StatusVisibility::Direct)
            && !user_initiated_private_thread
        {
            actions.push(policy.for_private_mentions);
        }
        if actor_model.limited_at.is_some() {
            actions.push(policy.for_limited_accounts);
        }
    }
    Ok(strongest_notification_policy_action(actions))
}

async fn upsert_notification_request(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    actor: NotificationActor,
    status_id: StatusId,
) -> Result<Uuid> {
    let (actor_account_id, remote_actor_id, last_status_id, last_remote_status_id) = match actor {
        NotificationActor::Local(actor_id) => (Some(actor_id.0), None, Some(status_id.0), None),
        NotificationActor::Remote(actor_id) => (None, Some(actor_id.0), None, Some(status_id.0)),
    };
    let conflict_target = if actor_account_id.is_some() {
        "(account_id, actor_account_id) WHERE actor_account_id IS NOT NULL AND state IN ('pending', 'merging')"
    } else {
        "(account_id, remote_actor_id) WHERE remote_actor_id IS NOT NULL AND state IN ('pending', 'merging')"
    };
    let sql = format!(
        "INSERT INTO local_notification_request
            (id, account_id, actor_account_id, remote_actor_id, last_status_id,
             last_remote_status_id, state, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, 'pending', now(), now())
         ON CONFLICT {conflict_target} DO UPDATE SET
            last_status_id = EXCLUDED.last_status_id,
            last_remote_status_id = EXCLUDED.last_remote_status_id,
            updated_at = now()
         RETURNING id"
    );
    let row = db
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            vec![
                Uuid::now_v7().into(),
                account_id.0.into(),
                actor_account_id.into(),
                remote_actor_id.into(),
                last_status_id.into(),
                last_remote_status_id.into(),
            ],
        ))
        .await?
        .ok_or_else(|| {
            RoostyError::InvalidInput("notification request could not be saved".to_owned())
        })?;
    let request_id: Uuid = row.try_get("", "id")?;
    let mut notifications = local_notification::Entity::update_many()
        .col_expr(
            local_notification::Column::NotificationRequestId,
            Expr::value(Some(request_id)),
        )
        .filter(local_notification::Column::AccountId.eq(account_id.0))
        .filter(local_notification::Column::Filtered.eq(true))
        .filter(local_notification::Column::NotificationRequestId.is_null());
    notifications = match actor {
        NotificationActor::Local(actor_id) => {
            notifications.filter(local_notification::Column::ActorAccountId.eq(actor_id.0))
        }
        NotificationActor::Remote(actor_id) => {
            notifications.filter(local_notification::Column::RemoteActorId.eq(actor_id.0))
        }
    };
    notifications.exec(db).await?;
    Ok(request_id)
}

async fn active_notification_request_id(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    actor: NotificationActor,
) -> Result<Option<Uuid>> {
    let mut query = local_notification_request::Entity::find()
        .filter(local_notification_request::Column::AccountId.eq(account_id.0))
        .filter(local_notification_request::Column::State.is_in([
            NotificationRequestState::Pending,
            NotificationRequestState::Merging,
        ]));
    query = match actor {
        NotificationActor::Local(actor_id) => {
            query.filter(local_notification_request::Column::ActorAccountId.eq(actor_id.0))
        }
        NotificationActor::Remote(actor_id) => {
            query.filter(local_notification_request::Column::RemoteActorId.eq(actor_id.0))
        }
    };
    Ok(query.one(db).await?.map(|request| request.id))
}

/// Create a local mention notification only when the logical event is new.
pub async fn notify_local_status_mention(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    actor_account_id: AccountId,
    status_id: StatusId,
) -> Result<Option<LocalNotification>> {
    if account_id == actor_account_id
        || !local_account_allows_notification(db, account_id, actor_account_id).await?
    {
        return Ok(None);
    }
    if let Some(existing) = local_notification::Entity::find()
        .filter(local_notification::Column::AccountId.eq(account_id.0))
        .filter(local_notification::Column::NotificationType.eq(LocalNotificationType::Mention))
        .filter(local_notification::Column::ActorAccountId.eq(Some(actor_account_id.0)))
        .filter(local_notification::Column::StatusId.eq(status_id.0))
        .one(db)
        .await?
    {
        return Ok((!existing.filtered).then(|| local_notification_from_model(existing)));
    }
    let action = local_notification_policy_action(
        db,
        account_id,
        actor_account_id,
        LocalNotificationType::Mention,
        Some(status_id),
    )
    .await?;
    if action == NotificationPolicyAction::Drop {
        return Ok(None);
    }
    let request_id = if action == NotificationPolicyAction::Filter {
        Some(
            upsert_notification_request(
                db,
                account_id,
                NotificationActor::Local(actor_account_id),
                status_id,
            )
            .await?,
        )
    } else {
        None
    };
    let model = local_notification::ActiveModel {
        id: Set(Uuid::now_v7()),
        account_id: Set(account_id.0),
        notification_type: Set(LocalNotificationType::Mention),
        actor_account_id: Set(Some(actor_account_id.0)),
        remote_actor_id: Set(None),
        status_id: Set(Some(status_id.0)),
        remote_status_id: Set(None),
        group_id: Set(None),
        filtered: Set(request_id.is_some()),
        notification_request_id: Set(request_id),
        report_id: Set(None),
        created_at: Set(OffsetDateTime::now_utc()),
        dismissed_at: Set(None),
    }
    .insert(db)
    .await?;
    Ok(request_id
        .is_none()
        .then(|| local_notification_from_model(model)))
}

/// List visible local notifications for one recipient with Mastodon cursor filters.
pub async fn local_notifications_for_account(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    limit: u64,
    cursor: CollectionCursor,
    filter: NotificationFilter,
) -> Result<CollectionPage<LocalNotification>> {
    let mut query = local_notification::Entity::find()
        .filter(local_notification::Column::AccountId.eq(account_id.0))
        .filter(local_notification::Column::DismissedAt.is_null())
        .apply_collection_cursor(cursor)
        .order_by_desc(local_notification::Column::Id)
        .limit(page_query_limit(limit));

    if !filter.include_filtered {
        query = query.filter(local_notification::Column::Filtered.eq(false));
    }

    let hidden_remote_ids = hidden_remote_actor_ids_for_account(db, account_id).await?;
    if !hidden_remote_ids.is_empty() {
        query = query.filter(
            Condition::any()
                .add(local_notification::Column::RemoteActorId.is_null())
                .add(
                    local_notification::Column::RemoteActorId
                        .is_not_in(hidden_remote_ids.into_iter().map(|id| id.0)),
                ),
        );
    }

    if !filter.include_types.is_empty() {
        query = query.filter(
            local_notification::Column::NotificationType
                .is_in(filter.include_types.iter().map(|value| value.as_str())),
        );
    }
    if !filter.exclude_types.is_empty() {
        query = query.filter(
            local_notification::Column::NotificationType
                .is_not_in(filter.exclude_types.iter().map(|value| value.as_str())),
        );
    }
    if let Some(actor_id) = filter.account_id {
        query = query.filter(
            Condition::any()
                .add(local_notification::Column::ActorAccountId.eq(actor_id.0))
                .add(local_notification::Column::RemoteActorId.eq(actor_id.0)),
        );
    }

    let rows = query.all(db).await?;
    let (rows, has_more) = trim_to_page(rows, limit);
    let first_cursor = rows.first().map(|model| model.id);
    let last_cursor = rows.last().map(|model| model.id);
    let items = rows
        .into_iter()
        .map(local_notification_from_model)
        .collect();

    Ok(CollectionPage {
        items,
        first_cursor,
        last_cursor,
        has_more,
    })
}

/// List notification groups newest-first using each group's newest notification as its cursor.
pub async fn notification_groups_for_account(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    limit: u64,
    cursor: CollectionCursor,
    filter: NotificationFilter,
    grouped_types: &[LocalNotificationType],
) -> Result<NotificationGroupPage> {
    let groupable = grouped_types
        .iter()
        .copied()
        .filter(|kind| {
            matches!(
                kind,
                LocalNotificationType::Favourite
                    | LocalNotificationType::Follow
                    | LocalNotificationType::Reblog
            )
        })
        .collect::<Vec<_>>();
    let quoted = |values: &[LocalNotificationType]| {
        values
            .iter()
            .map(|value| format!("'{}'", value.as_str()))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut conditions = vec![
        format!("account_id = '{}'::uuid", account_id.0),
        "dismissed_at IS NULL".to_owned(),
    ];
    if !filter.include_filtered {
        conditions.push("filtered = false".to_owned());
    }
    if !filter.include_types.is_empty() {
        conditions.push(format!(
            "notification_type IN ({})",
            quoted(&filter.include_types)
        ));
    }
    if !filter.exclude_types.is_empty() {
        conditions.push(format!(
            "notification_type NOT IN ({})",
            quoted(&filter.exclude_types)
        ));
    }
    if let Some(actor_id) = filter.account_id {
        conditions.push(format!(
            "(actor_account_id = '{0}'::uuid OR remote_actor_id = '{0}'::uuid)",
            actor_id.0
        ));
    }
    let hidden_remote_ids = hidden_remote_actor_ids_for_account(db, account_id).await?;
    if !hidden_remote_ids.is_empty() {
        conditions.push(format!(
            "(remote_actor_id IS NULL OR remote_actor_id NOT IN ({}))",
            hidden_remote_ids
                .iter()
                .map(|id| format!("'{}'::uuid", id.0))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let grouped = if groupable.is_empty() {
        "NULL".to_owned()
    } else {
        quoted(&groupable)
    };
    let mut cursor_conditions = Vec::new();
    if let Some(id) = cursor.max_id {
        cursor_conditions.push(format!("most_recent_notification_id < '{}'::uuid", id));
    }
    if let Some(id) = cursor.since_id {
        cursor_conditions.push(format!("most_recent_notification_id > '{}'::uuid", id));
    }
    if let Some(id) = cursor.min_id {
        cursor_conditions.push(format!("most_recent_notification_id > '{}'::uuid", id));
    }
    let cursor_where = if cursor_conditions.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", cursor_conditions.join(" AND "))
    };
    let query_limit = page_query_limit(limit);
    let sql = format!(
        r#"
        WITH filtered AS (
            SELECT *, COALESCE(actor_account_id, remote_actor_id) AS source_account_id
            FROM local_notification WHERE {conditions}
        ), effective AS (
            SELECT *, CASE
                WHEN group_id IS NOT NULL AND notification_type IN ({grouped})
                THEN notification_type || '-' || group_id::text
                ELSE 'ungrouped-' || id::text END AS effective_group_key
            FROM filtered
        ), grouped AS (
            SELECT effective_group_key AS group_key, notification_type,
                count(*)::bigint AS notifications_count,
                (array_agg(id ORDER BY id DESC))[1] AS most_recent_notification_id,
                (array_agg(id ORDER BY id ASC))[1] AS page_min_id,
                (array_agg(id ORDER BY id DESC))[1] AS page_max_id,
                max(created_at) AS latest_page_notification_at,
                (array_agg(COALESCE(status_id, remote_status_id) ORDER BY id DESC)
                    FILTER (WHERE status_id IS NOT NULL OR remote_status_id IS NOT NULL))[1] AS status_id,
                bool_or(remote_status_id IS NOT NULL) AS remote_status
            FROM effective GROUP BY effective_group_key, notification_type
        )
        SELECT grouped.*,
            COALESCE((SELECT jsonb_agg(sample.source_account_id ORDER BY sample.id DESC)
                FROM (SELECT distinct_actor.source_account_id, distinct_actor.id
                    FROM (SELECT DISTINCT ON (source_account_id) source_account_id, id
                        FROM effective e2 WHERE e2.effective_group_key = grouped.group_key
                            AND source_account_id IS NOT NULL
                        ORDER BY source_account_id, id DESC) distinct_actor
                    ORDER BY distinct_actor.id DESC LIMIT 8) sample), '[]'::jsonb) AS sample_account_ids
        FROM grouped {cursor_where}
        ORDER BY most_recent_notification_id DESC LIMIT {query_limit}
    "#,
        conditions = conditions.join(" AND ")
    );
    let rows = NotificationGroupRow::find_by_statement(Statement::from_string(
        DatabaseBackend::Postgres,
        sql,
    ))
    .all(db)
    .await?;
    let (rows, has_more) = trim_to_page(rows, limit);
    let first_cursor = rows.first().map(|row| row.most_recent_notification_id);
    let last_cursor = rows.last().map(|row| row.most_recent_notification_id);
    let items = rows
        .into_iter()
        .map(|row| {
            let sample_account_ids = serde_json::from_value::<Vec<Uuid>>(row.sample_account_ids)
                .map_err(|error| {
                    RoostyError::InvalidInput(format!(
                        "stored notification samples are invalid: {error}"
                    ))
                })?
                .into_iter()
                .map(AccountId)
                .collect();
            Ok(NotificationGroup {
                group_key: row.group_key,
                notifications_count: u64::try_from(row.notifications_count).map_err(|_| {
                    RoostyError::InvalidInput("stored notification count is invalid".to_owned())
                })?,
                notification_type: row.notification_type,
                most_recent_notification_id: row.most_recent_notification_id,
                page_min_id: row.page_min_id,
                page_max_id: row.page_max_id,
                latest_page_notification_at: row.latest_page_notification_at,
                sample_account_ids,
                status_id: row.status_id.map(StatusId),
                remote_status: row.remote_status,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let page = NotificationGroupPage {
        items,
        first_cursor,
        last_cursor,
        has_more,
    };
    Ok(page)
}

fn parse_notification_group_key(group_key: &str) -> Option<(Option<LocalNotificationType>, Uuid)> {
    if let Some(id) = group_key.strip_prefix("ungrouped-") {
        return Uuid::parse_str(id).ok().map(|id| (None, id));
    }
    for kind in [
        LocalNotificationType::Favourite,
        LocalNotificationType::Follow,
        LocalNotificationType::Reblog,
    ] {
        if let Some(id) = group_key.strip_prefix(&format!("{}-", kind.as_str())) {
            return Uuid::parse_str(id).ok().map(|id| (Some(kind), id));
        }
    }
    None
}

/// Return all visible rows belonging to one opaque notification group key.
pub async fn notifications_in_group(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    group_key: &str,
) -> Result<Vec<LocalNotification>> {
    notifications_in_group_with_connection(db, account_id, group_key).await
}

async fn notifications_in_group_with_connection<C>(
    db: &C,
    account_id: AccountId,
    group_key: &str,
) -> Result<Vec<LocalNotification>>
where
    C: ConnectionTrait,
{
    let Some((kind, id)) = parse_notification_group_key(group_key) else {
        return Ok(Vec::new());
    };
    let mut query = local_notification::Entity::find()
        .filter(local_notification::Column::AccountId.eq(account_id.0))
        .filter(local_notification::Column::DismissedAt.is_null())
        .filter(local_notification::Column::Filtered.eq(false));
    query = match kind {
        Some(kind) => query
            .filter(local_notification::Column::NotificationType.eq(kind))
            .filter(local_notification::Column::GroupId.eq(id)),
        None => query.filter(local_notification::Column::Id.eq(id)),
    };
    let hidden = hidden_remote_actor_ids_for_account(db, account_id).await?;
    if !hidden.is_empty() {
        query = query.filter(
            Condition::any()
                .add(local_notification::Column::RemoteActorId.is_null())
                .add(
                    local_notification::Column::RemoteActorId
                        .is_not_in(hidden.into_iter().map(|id| id.0)),
                ),
        );
    }
    Ok(query
        .order_by_desc(local_notification::Column::Id)
        .all(db)
        .await?
        .into_iter()
        .map(local_notification_from_model)
        .collect())
}

/// Soft-dismiss every row in one notification group owned by the recipient.
pub async fn dismiss_notification_group(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    group_key: &str,
) -> Result<bool> {
    let rows = notifications_in_group_with_connection(db, account_id, group_key).await?;
    if rows.is_empty() {
        return Ok(false);
    }
    local_notification::Entity::update_many()
        .col_expr(
            local_notification::Column::DismissedAt,
            Expr::value(OffsetDateTime::now_utc()),
        )
        .filter(local_notification::Column::Id.is_in(rows.into_iter().map(|row| row.id)))
        .exec(db)
        .await?;
    Ok(true)
}

/// Find one visible local notification belonging to a recipient.
pub async fn find_local_notification_for_account(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    notification_id: Uuid,
) -> Result<Option<LocalNotification>> {
    let notification = local_notification::Entity::find_by_id(notification_id)
        .filter(local_notification::Column::AccountId.eq(account_id.0))
        .filter(local_notification::Column::DismissedAt.is_null())
        .filter(local_notification::Column::Filtered.eq(false))
        .one(db)
        .await?;

    Ok(notification.map(local_notification_from_model))
}

/// Dismiss one visible local notification for a recipient.
pub async fn dismiss_local_notification(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    notification_id: Uuid,
) -> Result<bool> {
    let Some(model) = local_notification::Entity::find_by_id(notification_id)
        .filter(local_notification::Column::AccountId.eq(account_id.0))
        .filter(local_notification::Column::DismissedAt.is_null())
        .filter(local_notification::Column::Filtered.eq(false))
        .one(db)
        .await?
    else {
        return Ok(false);
    };

    let mut active = model.into_active_model();
    active.dismissed_at = Set(Some(OffsetDateTime::now_utc()));
    active.update(db).await?;
    Ok(true)
}

/// Dismiss every visible local notification for a recipient.
pub async fn clear_local_notifications(
    db: &impl ConnectionTrait,
    account_id: AccountId,
) -> Result<()> {
    let notifications = local_notification::Entity::find()
        .filter(local_notification::Column::AccountId.eq(account_id.0))
        .filter(local_notification::Column::DismissedAt.is_null())
        .filter(local_notification::Column::Filtered.eq(false))
        .all(db)
        .await?;
    let now = OffsetDateTime::now_utc();
    for notification in notifications {
        let mut active = notification.into_active_model();
        active.dismissed_at = Set(Some(now));
        active.update(db).await?;
    }
    Ok(())
}

/// List pending notification requests with their notification counts in one query.
pub async fn notification_requests_for_account(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    limit: u64,
    cursor: CollectionCursor,
) -> Result<CollectionPage<NotificationRequest>> {
    let mut conditions = vec![
        "r.account_id = $1".to_owned(),
        "r.state = 'pending'".to_owned(),
    ];
    let mut values = vec![account_id.0.into()];
    for (column, value) in [
        ("r.id <", cursor.max_id),
        ("r.id >", cursor.since_id),
        ("r.id >", cursor.min_id),
    ] {
        if let Some(value) = value {
            values.push(value.into());
            conditions.push(format!("{column} ${}", values.len()));
        }
    }
    let query_limit = page_query_limit(limit);
    let sql = format!(
        "SELECT r.id, r.account_id, r.actor_account_id, r.remote_actor_id,
                r.last_status_id, r.last_remote_status_id, r.created_at, r.updated_at,
                count(n.id)::bigint AS notifications_count
         FROM local_notification_request r
         LEFT JOIN local_notification n ON n.notification_request_id = r.id
         WHERE {}
         GROUP BY r.id ORDER BY r.id DESC LIMIT {query_limit}",
        conditions.join(" AND ")
    );
    let rows = NotificationRequestRow::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        sql,
        values,
    ))
    .all(db)
    .await?;
    let (rows, has_more) = trim_to_page(rows, limit);
    let first_cursor = rows.first().map(|row| row.id);
    let last_cursor = rows.last().map(|row| row.id);
    let items = rows
        .into_iter()
        .map(notification_request_from_row)
        .collect::<Result<Vec<_>>>()?;
    Ok(CollectionPage {
        items,
        first_cursor,
        last_cursor,
        has_more,
    })
}

/// Return pending request and held-notification counts for a policy summary.
pub async fn notification_request_summary(
    db: &impl ConnectionTrait,
    account_id: AccountId,
) -> Result<(u64, u64)> {
    let requests_count = local_notification_request::Entity::find()
        .filter(local_notification_request::Column::AccountId.eq(account_id.0))
        .filter(local_notification_request::Column::State.eq(NotificationRequestState::Pending))
        .count(db)
        .await?;
    let notifications_count = local_notification::Entity::find()
        .filter(local_notification::Column::AccountId.eq(account_id.0))
        .filter(local_notification::Column::Filtered.eq(true))
        .filter(local_notification::Column::DismissedAt.is_null())
        .count(db)
        .await?;
    Ok((requests_count, notifications_count))
}

/// Find one pending notification request belonging to an account.
pub async fn find_notification_request_for_account(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    request_id: Uuid,
) -> Result<Option<NotificationRequest>> {
    let row = NotificationRequestRow::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT r.id, r.account_id, r.actor_account_id, r.remote_actor_id,
                r.last_status_id, r.last_remote_status_id, r.created_at, r.updated_at,
                count(n.id)::bigint AS notifications_count
         FROM local_notification_request r
         LEFT JOIN local_notification n ON n.notification_request_id = r.id
         WHERE r.id = $1 AND r.account_id = $2 AND r.state = 'pending'
         GROUP BY r.id",
        vec![request_id.into(), account_id.0.into()],
    ))
    .one(db)
    .await?;
    row.map(notification_request_from_row).transpose()
}

/// Count visible unread notifications newer than the account marker.
pub async fn notification_unread_count(
    db: &DbConnection,
    account_id: AccountId,
    since_id: Option<Uuid>,
) -> Result<u64> {
    let mut query = local_notification::Entity::find()
        .filter(local_notification::Column::AccountId.eq(account_id.0))
        .filter(local_notification::Column::DismissedAt.is_null())
        .filter(local_notification::Column::Filtered.eq(false));
    if let Some(since_id) = since_id {
        query = query.filter(local_notification::Column::Id.gt(since_id));
    }
    Ok(query.count(db).await?)
}

/// Accept pending notification requests and enqueue their durable merge.
pub async fn accept_notification_requests(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    request_ids: &[Uuid],
) -> Result<bool> {
    let mut query = local_notification_request::Entity::find()
        .filter(local_notification_request::Column::AccountId.eq(account_id.0))
        .filter(local_notification_request::Column::State.eq(NotificationRequestState::Pending));
    if !request_ids.is_empty() {
        query =
            query.filter(local_notification_request::Column::Id.is_in(request_ids.iter().copied()));
    }
    let requests = query.lock_exclusive().all(db).await?;
    if requests.is_empty() || (!request_ids.is_empty() && requests.len() != request_ids.len()) {
        return Ok(false);
    }
    for request in &requests {
        let (conflict_target, actor_id) = if let Some(actor_id) = request.actor_account_id {
            (
                "(account_id, actor_account_id) WHERE actor_account_id IS NOT NULL",
                actor_id,
            )
        } else if let Some(actor_id) = request.remote_actor_id {
            (
                "(account_id, remote_actor_id) WHERE remote_actor_id IS NOT NULL",
                actor_id,
            )
        } else {
            return Err(RoostyError::InvalidInput(
                "notification request actor is invalid".to_owned(),
            ));
        };
        let (local_actor, remote_actor) = if request.actor_account_id.is_some() {
            (Some(actor_id), None)
        } else {
            (None, Some(actor_id))
        };
        db.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            format!(
                "INSERT INTO local_notification_permission
                    (id, account_id, actor_account_id, remote_actor_id, created_at)
                 VALUES ($1, $2, $3, $4, now())
                 ON CONFLICT {conflict_target} DO NOTHING"
            ),
            vec![
                Uuid::now_v7().into(),
                account_id.0.into(),
                local_actor.into(),
                remote_actor.into(),
            ],
        ))
        .await?;
    }
    local_notification_request::Entity::update_many()
        .col_expr(
            local_notification_request::Column::State,
            Expr::value(NotificationRequestState::Merging),
        )
        .col_expr(
            local_notification_request::Column::UpdatedAt,
            Expr::value(OffsetDateTime::now_utc()),
        )
        .filter(local_notification_request::Column::Id.is_in(requests.iter().map(|row| row.id)))
        .exec(db)
        .await?;
    enqueue_job_in_transaction(
        db,
        NewJob {
            kind: JobKind::NotificationRequestMerge,
            payload: serde_json::json!({ "account_id": account_id.0 }),
            deduplication_key: Some(format!("notification-request-merge:{}", account_id.0)),
            run_after: OffsetDateTime::now_utc(),
        },
    )
    .await?;
    Ok(true)
}

/// Dismiss pending notification requests and enqueue durable cleanup.
pub async fn dismiss_notification_requests(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    request_ids: &[Uuid],
) -> Result<bool> {
    let mut query = local_notification_request::Entity::find()
        .filter(local_notification_request::Column::AccountId.eq(account_id.0))
        .filter(local_notification_request::Column::State.eq(NotificationRequestState::Pending));
    if !request_ids.is_empty() {
        query =
            query.filter(local_notification_request::Column::Id.is_in(request_ids.iter().copied()));
    }
    let requests = query.lock_exclusive().all(db).await?;
    if requests.is_empty() || (!request_ids.is_empty() && requests.len() != request_ids.len()) {
        return Ok(false);
    }
    local_notification_request::Entity::update_many()
        .col_expr(
            local_notification_request::Column::State,
            Expr::value(NotificationRequestState::Dismissed),
        )
        .col_expr(
            local_notification_request::Column::UpdatedAt,
            Expr::value(OffsetDateTime::now_utc()),
        )
        .filter(local_notification_request::Column::Id.is_in(requests.iter().map(|row| row.id)))
        .exec(db)
        .await?;
    enqueue_job_in_transaction(
        db,
        NewJob {
            kind: JobKind::NotificationRequestCleanup,
            payload: serde_json::json!({ "account_id": account_id.0 }),
            deduplication_key: Some(format!("notification-request-cleanup:{}", account_id.0)),
            run_after: OffsetDateTime::now_utc(),
        },
    )
    .await?;
    Ok(true)
}

/// Return whether no accepted notification requests are awaiting a merge.
pub async fn notification_requests_merged(
    db: &impl ConnectionTrait,
    account_id: AccountId,
) -> Result<bool> {
    Ok(local_notification_request::Entity::find()
        .filter(local_notification_request::Column::AccountId.eq(account_id.0))
        .filter(local_notification_request::Column::State.eq(NotificationRequestState::Merging))
        .one(db)
        .await?
        .is_none())
}

/// Merge all accepted requests for one account. Safe to retry after worker interruption.
pub async fn merge_notification_requests(db: &DbConnection, account_id: AccountId) -> Result<()> {
    let txn = db.begin().await?;
    let requests = local_notification_request::Entity::find()
        .filter(local_notification_request::Column::AccountId.eq(account_id.0))
        .filter(local_notification_request::Column::State.eq(NotificationRequestState::Merging))
        .lock_exclusive()
        .all(&txn)
        .await?;
    if !requests.is_empty() {
        let ids = requests
            .iter()
            .map(|request| request.id)
            .collect::<Vec<_>>();
        local_notification::Entity::update_many()
            .col_expr(local_notification::Column::Filtered, Expr::value(false))
            .col_expr(
                local_notification::Column::NotificationRequestId,
                Expr::value(None::<Uuid>),
            )
            .filter(local_notification::Column::NotificationRequestId.is_in(ids.clone()))
            .exec(&txn)
            .await?;
        local_notification_request::Entity::delete_many()
            .filter(local_notification_request::Column::Id.is_in(ids))
            .exec(&txn)
            .await?;
    }
    txn.commit().await?;
    Ok(())
}

/// Delete notifications belonging to dismissed requests. Safe to retry.
pub async fn cleanup_notification_requests(db: &DbConnection, account_id: AccountId) -> Result<()> {
    let txn = db.begin().await?;
    let requests = local_notification_request::Entity::find()
        .filter(local_notification_request::Column::AccountId.eq(account_id.0))
        .filter(local_notification_request::Column::State.eq(NotificationRequestState::Dismissed))
        .lock_exclusive()
        .all(&txn)
        .await?;
    if !requests.is_empty() {
        let ids = requests
            .iter()
            .map(|request| request.id)
            .collect::<Vec<_>>();
        local_notification::Entity::delete_many()
            .filter(local_notification::Column::NotificationRequestId.is_in(ids.clone()))
            .exec(&txn)
            .await?;
        local_notification_request::Entity::delete_many()
            .filter(local_notification_request::Column::Id.is_in(ids))
            .exec(&txn)
            .await?;
    }
    txn.commit().await?;
    Ok(())
}

/// Return saved timeline markers for an account and a requested set of timelines.
pub async fn local_timeline_markers_for_account(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    timelines: &[LocalTimeline],
) -> Result<Vec<LocalTimelineMarker>> {
    if timelines.is_empty() {
        return Ok(Vec::new());
    }

    let markers = local_timeline_marker::Entity::find()
        .filter(local_timeline_marker::Column::AccountId.eq(account_id.0))
        .filter(local_timeline_marker::Column::Timeline.is_in(timelines.iter().copied()))
        .all(db)
        .await?;

    markers
        .into_iter()
        .map(local_timeline_marker_from_model)
        .collect()
}

/// Save a local account's read position for a Mastodon timeline.
pub async fn save_local_timeline_marker(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    timeline: LocalTimeline,
    last_read_id: Uuid,
) -> Result<LocalTimelineMarker> {
    let now = OffsetDateTime::now_utc();
    let marker = local_timeline_marker::Entity::find_by_id((account_id.0, timeline))
        .one(db)
        .await?;

    let marker = match marker {
        Some(marker) => {
            let version = marker.version.checked_add(1).ok_or_else(|| {
                RoostyError::InvalidInput("timeline marker version is exhausted".to_owned())
            })?;
            let mut active = marker.into_active_model();
            active.last_read_id = Set(last_read_id);
            active.version = Set(version);
            active.updated_at = Set(now);
            active.update(db).await?
        }
        None => {
            local_timeline_marker::ActiveModel {
                account_id: Set(account_id.0),
                timeline: Set(timeline),
                last_read_id: Set(last_read_id),
                version: Set(1),
                updated_at: Set(now),
            }
            .insert(db)
            .await?
        }
    };

    local_timeline_marker_from_model(marker)
}

/// List local accounts following this account with Mastodon cursor filters.
pub async fn local_followers_for_account(
    db: &DbConnection,
    account_id: AccountId,
    limit: u64,
    cursor: CollectionCursor,
) -> Result<CollectionPage<LocalAccount>> {
    let rows = local_follow::Entity::find()
        .filter(local_follow::Column::FollowedAccountId.eq(account_id.0))
        .apply_collection_cursor(cursor)
        .order_by_desc(local_follow::Column::Id)
        .limit(page_query_limit(limit))
        .all(db)
        .await?;
    let (rows, has_more) = trim_to_page(rows, limit);
    let first_cursor = rows.first().map(|follow| follow.id);
    let last_cursor = rows.last().map(|follow| follow.id);
    let account_ids = rows
        .into_iter()
        .map(|follow| AccountId(follow.follower_account_id))
        .collect::<Vec<_>>();

    Ok(CollectionPage {
        items: local_accounts_by_id(db, account_ids).await?,
        first_cursor,
        last_cursor,
        has_more,
    })
}

/// List local accounts followed by this account with Mastodon cursor filters.
pub async fn local_following_for_account(
    db: &DbConnection,
    account_id: AccountId,
    limit: u64,
    cursor: CollectionCursor,
) -> Result<CollectionPage<LocalAccount>> {
    let rows = local_follow::Entity::find()
        .filter(local_follow::Column::FollowerAccountId.eq(account_id.0))
        .apply_collection_cursor(cursor)
        .order_by_desc(local_follow::Column::Id)
        .limit(page_query_limit(limit))
        .all(db)
        .await?;
    let (rows, has_more) = trim_to_page(rows, limit);
    let first_cursor = rows.first().map(|follow| follow.id);
    let last_cursor = rows.last().map(|follow| follow.id);
    let account_ids = rows
        .into_iter()
        .map(|follow| AccountId(follow.followed_account_id))
        .collect::<Vec<_>>();

    Ok(CollectionPage {
        items: local_accounts_by_id(db, account_ids).await?,
        first_cursor,
        last_cursor,
        has_more,
    })
}

/// Update mutable local account settings and return the refreshed account.
pub async fn update_local_account_settings<C>(
    db: &C,
    account_id: AccountId,
    update: LocalAccountSettingsUpdate,
) -> Result<LocalAccount>
where
    C: ConnectionTrait,
{
    let account = local_account::Entity::find_by_id(account_id.0)
        .one(db)
        .await?
        .ok_or_else(|| RoostyError::InvalidInput("local account does not exist".to_owned()))?;
    let mut active = account.into_active_model();
    let discoverability_changed = update.discoverable.is_some();

    set_if_some(&mut active.display_name, update.display_name);
    set_if_some(&mut active.note, update.note);
    set_if_some(&mut active.locked, update.locked);
    set_if_some(&mut active.bot, update.bot);
    set_if_some(&mut active.discoverable, update.discoverable);
    if let Some(visibility) = update.default_visibility {
        active.default_visibility = Set(visibility);
    }
    set_if_some(&mut active.default_sensitive, update.default_sensitive);
    set_if_some(&mut active.default_language, update.default_language);
    set_if_some(
        &mut active.default_quote_policy,
        update.default_quote_policy,
    );
    set_if_some(&mut active.profile_fields, update.profile_fields);
    if let Some(path) = update.avatar_file_path {
        active.avatar_file_path = Set(Some(path));
    }
    if let Some(path) = update.header_file_path {
        active.header_file_path = Set(Some(path));
    }
    active.updated_at = Set(OffsetDateTime::now_utc());

    let account = local_account_from_model(active.update(db).await?)?;
    if discoverability_changed {
        mark_account_status_trends_dirty(db, "local", account.id).await?;
    }
    Ok(account)
}

/// Replace a local account password hash by username for operator password resets.
pub async fn update_local_account_password_hash(
    db: &DbConnection,
    username: &str,
    password_hash: &str,
) -> Result<Option<LocalAccount>> {
    let Some(account) = local_account::Entity::find()
        .filter(local_account::Column::Username.eq(username))
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    let mut active = account.into_active_model();
    active.password_hash = Set(password_hash.to_owned());
    active.updated_at = Set(OffsetDateTime::now_utc());

    Ok(Some(local_account_from_model(active.update(db).await?)?))
}

/// Replace a local account password hash by its stable account identifier.
pub async fn update_local_account_password_hash_by_id(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    password_hash: &str,
) -> Result<LocalAccount> {
    let account = local_account::Entity::find_by_id(account_id.0)
        .one(db)
        .await?
        .ok_or_else(|| RoostyError::InvalidInput("local account does not exist".to_owned()))?;
    let mut active = account.into_active_model();
    active.password_hash = Set(password_hash.to_owned());
    active.updated_at = Set(OffsetDateTime::now_utc());

    local_account_from_model(active.update(db).await?)
}

/// Create a local status authored by an account on this instance.
pub async fn create_local_status(
    db: &DbConnection,
    new_status: NewLocalStatus,
) -> Result<LocalStatus> {
    let txn = db.begin().await?;
    let status_id = new_status.id.map_or_else(Uuid::now_v7, |id| id.0);
    let created_at = OffsetDateTime::now_utc();
    let account_id = new_status.account_id;

    let preview_content = new_status.content.clone();
    let status = local_status::ActiveModel {
        id: Set(status_id),
        account_id: Set(account_id.0),
        content: Set(new_status.content),
        visibility: Set(new_status.visibility),
        sensitive: Set(new_status.sensitive),
        spoiler_text: Set(new_status.spoiler_text),
        language: Set(new_status.language),
        in_reply_to_id: Set(new_status.in_reply_to_id.map(|id| id.0)),
        in_reply_to_remote_status_id: Set(new_status.in_reply_to_remote_status_id.map(|id| id.0)),
        conversation_id: Set(None),
        created_at: Set(created_at),
        updated_at: Set(created_at),
        deleted_at: Set(None),
        quote_approval_policy: Set(new_status.quote_approval_policy),
    }
    .insert(&txn)
    .await?;

    update_local_account_last_status_at(&txn, account_id, created_at).await?;
    Box::pin(replace_status_preview_card(
        &txn,
        PreviewStatusTarget::Local(StatusId(status_id)),
        &preview_content,
        utc_date(created_at),
        PreviewActorOrigin::Local,
        account_id.0,
        false,
    ))
    .await?;
    refresh_status_search_document(&txn, StatusReference::Local(StatusId(status_id))).await?;
    txn.commit().await?;
    local_status_from_model(status)
}

/// Create a local status with media, tags, and remote mentions in one transaction.
pub async fn create_local_status_with_media(
    txn: &sea_orm::DatabaseTransaction,
    new_status: NewLocalStatus,
    media_ids: &[Uuid],
    metadata: LocalStatusMetadata,
) -> Result<LocalStatus> {
    let LocalStatusMetadata {
        scheduled_status_id,
        mut tag_names,
        remote_actor_ids,
        local_recipient_ids,
        local_mention_ids,
    } = metadata;
    let status_id = new_status.id.map_or_else(Uuid::now_v7, |id| id.0);
    let created_at = OffsetDateTime::now_utc();
    let account_id = new_status.account_id;

    for media_id in media_ids {
        let Some(media) = local_media_attachment::Entity::find_by_id(*media_id)
            .one(txn)
            .await?
        else {
            return Err(RoostyError::InvalidInput(
                "media attachment not found".to_owned(),
            ));
        };
        if media.account_id != account_id.0
            || media.status_id.is_some()
            || (media.scheduled_status_id.is_some()
                && media.scheduled_status_id != scheduled_status_id)
        {
            return Err(RoostyError::InvalidInput(
                "media attachment is not available".to_owned(),
            ));
        }
    }

    let preview_content = new_status.content.clone();
    let status = local_status::ActiveModel {
        id: Set(status_id),
        account_id: Set(account_id.0),
        content: Set(new_status.content),
        visibility: Set(new_status.visibility),
        sensitive: Set(new_status.sensitive),
        spoiler_text: Set(new_status.spoiler_text),
        language: Set(new_status.language),
        in_reply_to_id: Set(new_status.in_reply_to_id.map(|id| id.0)),
        in_reply_to_remote_status_id: Set(new_status.in_reply_to_remote_status_id.map(|id| id.0)),
        conversation_id: Set(None),
        created_at: Set(created_at),
        updated_at: Set(created_at),
        deleted_at: Set(None),
        quote_approval_policy: Set(new_status.quote_approval_policy),
    }
    .insert(txn)
    .await?;
    update_local_account_last_status_at(txn, account_id, created_at).await?;
    Box::pin(replace_status_preview_card(
        txn,
        PreviewStatusTarget::Local(StatusId(status_id)),
        &preview_content,
        utc_date(created_at),
        PreviewActorOrigin::Local,
        account_id.0,
        false,
    ))
    .await?;

    for (index, media_id) in media_ids.iter().enumerate() {
        let Some(media) = local_media_attachment::Entity::find_by_id(*media_id)
            .one(txn)
            .await?
        else {
            return Err(RoostyError::InvalidInput(
                "media attachment not found".to_owned(),
            ));
        };
        let mut active = media.into_active_model();
        active.status_id = Set(Some(status_id));
        active.scheduled_status_id = Set(None);
        active.status_order = Set(index as i32);
        active.updated_at = Set(OffsetDateTime::now_utc());
        active.update(txn).await?;
    }
    refresh_status_search_document(txn, StatusReference::Local(StatusId(status_id))).await?;

    let now = OffsetDateTime::now_utc();
    tag_names.sort();
    tag_names.dedup();
    let mut tag_ids = Vec::with_capacity(tag_names.len());
    for name in tag_names {
        let tag = find_or_create_local_tag(txn, &name, now).await?;
        tag_ids.push(tag.id);
        local_status_tag::ActiveModel {
            status_id: Set(status_id),
            tag_id: Set(tag.id),
            created_at: Set(now),
        }
        .insert(txn)
        .await?;
    }
    if status.visibility == StatusVisibility::Public {
        adjust_tag_usage(
            txn,
            &tag_ids,
            utc_date(status.created_at),
            "local",
            status.account_id,
            1,
        )
        .await?;
    }
    let mut remote_actor_ids = remote_actor_ids
        .into_iter()
        .map(|id| id.0)
        .collect::<Vec<_>>();
    remote_actor_ids.sort();
    remote_actor_ids.dedup();
    for remote_actor_id in remote_actor_ids {
        local_status_remote_mention::ActiveModel {
            status_id: Set(status_id),
            remote_actor_id: Set(remote_actor_id),
            created_at: Set(now),
        }
        .insert(txn)
        .await?;
    }
    let mut local_recipient_ids = local_recipient_ids
        .into_iter()
        .map(|id| id.0)
        .collect::<Vec<_>>();
    local_recipient_ids.sort();
    local_recipient_ids.dedup();
    for account_id in local_recipient_ids {
        local_status_local_recipient::ActiveModel {
            status_id: Set(status_id),
            account_id: Set(account_id),
            created_at: Set(now),
        }
        .insert(txn)
        .await?;
    }
    replace_local_status_local_mentions(txn, StatusId(status_id), &local_mention_ids).await?;

    local_status_from_model(status)
}

/// Replace all local hashtag links for one status, creating tag rows as needed.
pub async fn replace_local_status_tags(
    db: &DbConnection,
    status_id: StatusId,
    tag_names: &[String],
) -> Result<()> {
    let txn = db.begin().await?;
    let status = local_status::Entity::find_by_id(status_id.0)
        .one(&txn)
        .await?;
    let old_tag_ids = local_status_tag::Entity::find()
        .filter(local_status_tag::Column::StatusId.eq(status_id.0))
        .all(&txn)
        .await?
        .into_iter()
        .map(|row| row.tag_id)
        .collect::<Vec<_>>();
    if let Some(status) = &status
        && status.deleted_at.is_none()
        && status.visibility == StatusVisibility::Public
    {
        adjust_tag_usage(
            &txn,
            &old_tag_ids,
            utc_date(status.created_at),
            "local",
            status.account_id,
            -1,
        )
        .await?;
    }
    local_status_tag::Entity::delete_many()
        .filter(local_status_tag::Column::StatusId.eq(status_id.0))
        .exec(&txn)
        .await?;

    let now = OffsetDateTime::now_utc();
    let mut names = tag_names.to_vec();
    names.sort();
    names.dedup();

    let mut new_tag_ids = Vec::with_capacity(names.len());
    for name in names {
        let tag = find_or_create_local_tag(&txn, &name, now).await?;
        new_tag_ids.push(tag.id);
        local_status_tag::ActiveModel {
            status_id: Set(status_id.0),
            tag_id: Set(tag.id),
            created_at: Set(now),
        }
        .insert(&txn)
        .await?;
    }
    if let Some(status) = status
        && status.deleted_at.is_none()
        && status.visibility == StatusVisibility::Public
    {
        adjust_tag_usage(
            &txn,
            &new_tag_ids,
            utc_date(status.created_at),
            "local",
            status.account_id,
            1,
        )
        .await?;
    }

    txn.commit().await?;
    Ok(())
}

/// Replace the resolved remote actors explicitly mentioned by one local status.
pub async fn replace_local_status_remote_mentions(
    db: &DbConnection,
    status_id: StatusId,
    remote_actor_ids: &[AccountId],
) -> Result<()> {
    let txn = db.begin().await?;
    local_status_remote_mention::Entity::delete_many()
        .filter(local_status_remote_mention::Column::StatusId.eq(status_id.0))
        .exec(&txn)
        .await?;

    let now = OffsetDateTime::now_utc();
    let mut actor_ids = remote_actor_ids.iter().map(|id| id.0).collect::<Vec<_>>();
    actor_ids.sort();
    actor_ids.dedup();
    for remote_actor_id in actor_ids {
        local_status_remote_mention::ActiveModel {
            status_id: Set(status_id.0),
            remote_actor_id: Set(remote_actor_id),
            created_at: Set(now),
        }
        .insert(&txn)
        .await?;
    }
    txn.commit().await?;
    Ok(())
}

/// List remote actors explicitly mentioned by one local status.
pub async fn remote_mentions_for_local_status(
    db: &impl ConnectionTrait,
    status_id: StatusId,
) -> Result<Vec<RemoteActor>> {
    let rows = local_status_remote_mention::Entity::find()
        .filter(local_status_remote_mention::Column::StatusId.eq(status_id.0))
        .all(db)
        .await?;
    let actor_ids = rows
        .into_iter()
        .map(|row| row.remote_actor_id)
        .collect::<Vec<_>>();
    if actor_ids.is_empty() {
        return Ok(Vec::new());
    }
    Ok(remote_actor::Entity::find()
        .filter(remote_actor::Column::Id.is_in(actor_ids))
        .all(db)
        .await?
        .into_iter()
        .map(remote_actor_from_model)
        .collect())
}

/// Return the author of the cached remote Note that one local status replies to.
pub async fn remote_reply_actor_for_local_status(
    db: &impl ConnectionTrait,
    status: &LocalStatus,
) -> Result<Option<RemoteActor>> {
    let Some(parent_id) = status.in_reply_to_remote_status_id else {
        return Ok(None);
    };
    let Some(parent) = find_remote_status_by_id(db, parent_id).await? else {
        return Ok(None);
    };
    find_remote_actor_by_id(db, parent.remote_actor_id).await
}

/// List tags attached to a local status in normalized name order.
pub async fn local_tags_for_status(
    db: &impl ConnectionTrait,
    status_id: StatusId,
) -> Result<Vec<LocalTag>> {
    let rows = local_status_tag::Entity::find()
        .filter(local_status_tag::Column::StatusId.eq(status_id.0))
        .all(db)
        .await?;
    let mut tags = Vec::with_capacity(rows.len());
    for row in rows {
        if let Some(tag) = local_tag::Entity::find_by_id(row.tag_id).one(db).await? {
            tags.push(local_tag_from_model(tag));
        }
    }
    tags.sort_by(|left, right| left.name.cmp(&right.name));

    Ok(tags)
}

/// List normalized hashtags indexed for a cached remote status.
pub async fn remote_tags_for_status(
    db: &impl ConnectionTrait,
    status_id: StatusId,
) -> Result<Vec<LocalTag>> {
    let rows = remote_status_tag::Entity::find()
        .filter(remote_status_tag::Column::RemoteStatusId.eq(status_id.0))
        .all(db)
        .await?;
    let mut tags = Vec::with_capacity(rows.len());
    for row in rows {
        if let Some(tag) = local_tag::Entity::find_by_id(row.tag_id).one(db).await? {
            tags.push(local_tag_from_model(tag));
        }
    }
    tags.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(tags)
}

/// List accounts following at least one hashtag currently attached to a local status.
pub async fn local_tag_follower_ids_for_status(
    db: &impl ConnectionTrait,
    status_id: StatusId,
) -> Result<Vec<AccountId>> {
    let follows = local_tag_follow::Entity::find()
        .filter(
            local_tag_follow::Column::TagId.in_subquery(
                Query::select()
                    .column(local_status_tag::Column::TagId)
                    .from(local_status_tag::Entity)
                    .and_where(local_status_tag::Column::StatusId.eq(status_id.0))
                    .to_owned(),
            ),
        )
        .all(db)
        .await?;
    let mut account_ids = follows
        .into_iter()
        .map(|follow| AccountId(follow.account_id))
        .collect::<Vec<_>>();
    account_ids.sort_by_key(|id| id.0);
    account_ids.dedup();
    Ok(account_ids)
}

/// List local accounts following a hashtag currently attached to a cached remote status.
pub async fn remote_tag_follower_ids_for_status(
    db: &impl ConnectionTrait,
    status_id: StatusId,
) -> Result<Vec<AccountId>> {
    let follows = local_tag_follow::Entity::find()
        .filter(
            local_tag_follow::Column::TagId.in_subquery(
                Query::select()
                    .column(remote_status_tag::Column::TagId)
                    .from(remote_status_tag::Entity)
                    .and_where(remote_status_tag::Column::RemoteStatusId.eq(status_id.0))
                    .to_owned(),
            ),
        )
        .all(db)
        .await?;
    let mut account_ids = follows
        .into_iter()
        .map(|follow| AccountId(follow.account_id))
        .collect::<Vec<_>>();
    account_ids.sort_by_key(|id| id.0);
    account_ids.dedup();
    Ok(account_ids)
}

/// Search local tags by normalized prefix with offset pagination.
pub async fn search_local_tags(
    db: &impl ConnectionTrait,
    query: &str,
    limit: u64,
    offset: u64,
) -> Result<Vec<LocalTag>> {
    let query = normalize_tag_name(query);
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let tags = local_tag::Entity::find()
        .filter(local_tag::Column::Name.starts_with(&query))
        .order_by_asc(local_tag::Column::Name)
        .offset(offset)
        .limit(limit)
        .all(db)
        .await?;

    Ok(tags.into_iter().map(local_tag_from_model).collect())
}

/// Find a local tag by normalized name.
pub async fn find_local_tag_by_name(
    db: &impl ConnectionTrait,
    name: &str,
) -> Result<Option<LocalTag>> {
    let name = normalize_tag_name(name);
    if name.is_empty() {
        return Ok(None);
    }
    let tag = local_tag::Entity::find()
        .filter(local_tag::Column::Name.eq(name))
        .one(db)
        .await?;

    Ok(tag.map(local_tag_from_model))
}

/// Follow a local hashtag for one account, creating the tag row when necessary.
pub async fn follow_local_tag(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    name: &str,
) -> Result<LocalTag> {
    let now = OffsetDateTime::now_utc();
    let tag = find_or_create_local_tag(db, name, now).await?;

    let existing = local_tag_follow::Entity::find()
        .filter(local_tag_follow::Column::AccountId.eq(account_id.0))
        .filter(local_tag_follow::Column::TagId.eq(tag.id))
        .one(db)
        .await?;
    match existing {
        Some(follow) => {
            let mut active = follow.into_active_model();
            active.updated_at = Set(now);
            active.update(db).await?;
        }
        None => {
            local_tag_follow::ActiveModel {
                account_id: Set(account_id.0),
                tag_id: Set(tag.id),
                created_at: Set(now),
                updated_at: Set(now),
            }
            .insert(db)
            .await?;
        }
    }

    Ok(local_tag_from_model(tag))
}

/// Stop following a local hashtag for one account and return the local tag when it exists.
pub async fn unfollow_local_tag(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    name: &str,
) -> Result<Option<LocalTag>> {
    let Some(tag) = find_local_tag_by_name(db, name).await? else {
        return Ok(None);
    };
    local_tag_follow::Entity::delete_many()
        .filter(local_tag_follow::Column::AccountId.eq(account_id.0))
        .filter(local_tag_follow::Column::TagId.eq(tag.id))
        .exec(db)
        .await?;

    Ok(Some(tag))
}

/// Return hashtags followed by a local account in name order.
pub async fn followed_local_tags(
    db: &impl ConnectionTrait,
    account_id: AccountId,
) -> Result<Vec<LocalTag>> {
    let follows = local_tag_follow::Entity::find()
        .filter(local_tag_follow::Column::AccountId.eq(account_id.0))
        .all(db)
        .await?;
    let mut tags = Vec::with_capacity(follows.len());
    for follow in follows {
        if let Some(tag) = local_tag::Entity::find_by_id(follow.tag_id).one(db).await? {
            tags.push(local_tag_from_model(tag));
        }
    }
    tags.sort_by(|left, right| left.name.cmp(&right.name));

    Ok(tags)
}

/// Return whether a local account follows the tag.
pub async fn is_local_tag_followed(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    tag_id: Uuid,
) -> Result<bool> {
    Ok(local_tag_follow::Entity::find()
        .filter(local_tag_follow::Column::AccountId.eq(account_id.0))
        .filter(local_tag_follow::Column::TagId.eq(tag_id))
        .one(db)
        .await?
        .is_some())
}

/// Return public local and cached-remote usage during the latest seven UTC days.
pub async fn tag_history(db: &impl ConnectionTrait, tag_id: Uuid) -> Result<Vec<LocalTagHistory>> {
    #[derive(Debug, FromQueryResult)]
    struct HistoryRow {
        day: i64,
        uses: i64,
        accounts: i64,
    }

    let rows = HistoryRow::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"SELECT extract(epoch FROM usage_day::timestamp)::bigint AS day,
                  uses, accounts
           FROM tag_daily_usage
           WHERE tag_id = $1
             AND usage_day >= (now() AT TIME ZONE 'UTC')::date - 6
           ORDER BY usage_day DESC"#,
        vec![tag_id.into()],
    ))
    .all(db)
    .await?;

    let history = rows
        .into_iter()
        .map(|row| {
            Ok(LocalTagHistory {
                day: u64::try_from(row.day)
                    .map_err(|_| DbErr::Type("negative tag history day".to_owned()))?,
                uses: u64::try_from(row.uses)
                    .map_err(|_| DbErr::Type("negative tag usage count".to_owned()))?,
                accounts: u64::try_from(row.accounts)
                    .map_err(|_| DbErr::Type("negative tag account count".to_owned()))?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(fill_tag_history(history))
}

/// Rank hashtags from the transactionally maintained shared trend cache.
pub async fn trending_tags(
    db: &impl ConnectionTrait,
    limit: u64,
    offset: u64,
) -> Result<Vec<TrendingTag>> {
    let rows = TrendingTagRow::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"WITH selected AS (
               SELECT trend.tag_id, trend.score
               FROM tag_trend trend
               WHERE trend.expires_at > now() AND trend.score >= 1
               ORDER BY trend.score DESC, trend.tag_id
               LIMIT $1 OFFSET $2
           )
           SELECT t.id, t.name, t.created_at, t.updated_at,
                  extract(epoch FROM u.usage_day::timestamp)::bigint AS day,
                  u.uses, u.accounts
           FROM selected s
           JOIN local_tag t ON t.id = s.tag_id
           JOIN tag_daily_usage u ON u.tag_id = s.tag_id
             AND u.usage_day >= (now() AT TIME ZONE 'UTC')::date - 6
           ORDER BY s.score DESC, s.tag_id, day DESC"#,
        vec![
            i64::try_from(limit)
                .map_err(|_| DbErr::Type("trend limit exceeds bigint".to_owned()))?
                .into(),
            i64::try_from(offset)
                .map_err(|_| DbErr::Type("trend offset exceeds bigint".to_owned()))?
                .into(),
        ],
    ))
    .all(db)
    .await?;

    let mut trends = Vec::<TrendingTag>::new();
    for row in rows {
        if trends.last().is_none_or(|trend| trend.tag.id != row.id) {
            trends.push(TrendingTag {
                tag: LocalTag {
                    id: row.id,
                    name: row.name.clone(),
                    created_at: row.created_at,
                    updated_at: row.updated_at,
                },
                history: Vec::new(),
            });
        }
        if let Some(trend) = trends.last_mut() {
            trend.history.push(LocalTagHistory {
                day: u64::try_from(row.day)
                    .map_err(|_| DbErr::Type("negative tag history day".to_owned()))?,
                uses: u64::try_from(row.uses)
                    .map_err(|_| DbErr::Type("negative tag usage count".to_owned()))?,
                accounts: u64::try_from(row.accounts)
                    .map_err(|_| DbErr::Type("negative tag account count".to_owned()))?,
            });
        }
    }
    for trend in &mut trends {
        trend.history = fill_tag_history(mem::take(&mut trend.history));
    }
    Ok(trends)
}

fn fill_tag_history(history: Vec<LocalTagHistory>) -> Vec<LocalTagHistory> {
    const DAY_SECONDS: i64 = 86_400;
    let by_day = history
        .into_iter()
        .map(|bucket| (bucket.day, bucket))
        .collect::<HashMap<_, _>>();
    let today = OffsetDateTime::now_utc()
        .unix_timestamp()
        .div_euclid(DAY_SECONDS);
    (0..7)
        .filter_map(|days_ago| {
            let day = u64::try_from((today - days_ago) * DAY_SECONDS).ok()?;
            Some(by_day.get(&day).cloned().unwrap_or(LocalTagHistory {
                day,
                uses: 0,
                accounts: 0,
            }))
        })
        .collect()
}

/// Return public, discoverable statuses ranked from cached interaction totals.
pub async fn trending_statuses(
    db: &impl ConnectionTrait,
    limit: u64,
    offset: u64,
) -> Result<Vec<TrendingStatus>> {
    #[derive(Debug, FromQueryResult)]
    struct TrendRow {
        local_status_id: Option<Uuid>,
        remote_status_id: Option<Uuid>,
        favourites_count: i64,
        reblogs_count: i64,
    }

    let rows = TrendRow::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"SELECT metric.local_status_id, metric.remote_status_id,
                  metric.favourites_count, metric.reblogs_count
           FROM status_trend_metric metric
           LEFT JOIN local_status local_status
             ON local_status.id = metric.local_status_id
           LEFT JOIN local_account local_account
             ON local_account.id = local_status.account_id
           LEFT JOIN remote_status remote_status
             ON remote_status.id = metric.remote_status_id
           LEFT JOIN remote_actor remote_actor
             ON remote_actor.id = remote_status.remote_actor_id
           WHERE metric.expires_at > now()
             AND metric.score >= 1
             AND (
               (local_status.id IS NOT NULL
                AND local_status.deleted_at IS NULL
                AND local_status.visibility = 'public'
                AND local_status.in_reply_to_id IS NULL
                AND local_status.in_reply_to_remote_status_id IS NULL
                AND local_status.sensitive = false
                AND local_status.spoiler_text = ''
                AND local_account.discoverable = true
                AND local_account.limited_at IS NULL
                AND local_account.suspended_at IS NULL)
               OR
               (remote_status.id IS NOT NULL
                AND remote_status.deleted_at IS NULL
                AND remote_status.visibility = 'public'
                AND remote_status.in_reply_to IS NULL
                AND coalesce((remote_status.object->>'sensitive')::boolean, false) = false
                AND coalesce(remote_status.object->>'summary', '') = ''
                AND remote_actor.deleted_at IS NULL
                AND remote_actor.limited_at IS NULL
                AND remote_actor.suspended_at IS NULL)
             )
           ORDER BY
             metric.score DESC,
             coalesce(metric.local_status_id, metric.remote_status_id) DESC
           LIMIT $1 OFFSET $2"#,
        vec![
            i64::try_from(limit)
                .map_err(|_| DbErr::Type("trend limit exceeds bigint".to_owned()))?
                .into(),
            i64::try_from(offset)
                .map_err(|_| DbErr::Type("trend offset exceeds bigint".to_owned()))?
                .into(),
        ],
    ))
    .all(db)
    .await?;

    let local_ids = rows
        .iter()
        .filter_map(|row| row.local_status_id)
        .collect::<Vec<_>>();
    let remote_ids = rows
        .iter()
        .filter_map(|row| row.remote_status_id)
        .collect::<Vec<_>>();
    let local = local_status::Entity::find()
        .filter(local_status::Column::Id.is_in(local_ids))
        .all(db)
        .await?
        .into_iter()
        .map(|model| local_status_from_model(model).map(|status| (status.id.0, status)))
        .collect::<Result<HashMap<_, _>>>()?;
    let remote = remote_status::Entity::find()
        .filter(remote_status::Column::Id.is_in(remote_ids))
        .all(db)
        .await?
        .into_iter()
        .map(|model| remote_status_from_model(model).map(|status| (status.id.0, status)))
        .collect::<Result<HashMap<_, _>>>()?;
    rows.into_iter()
        .filter_map(|row| {
            let item = match (row.local_status_id, row.remote_status_id) {
                (Some(id), None) => local.get(&id).cloned().map(PublicTimelineItem::Local),
                (None, Some(id)) => remote.get(&id).cloned().map(PublicTimelineItem::Remote),
                _ => None,
            }?;
            Some(
                u64::try_from(row.favourites_count)
                    .and_then(|favourites_count| {
                        u64::try_from(row.reblogs_count)
                            .map(|reblogs_count| (favourites_count, reblogs_count))
                    })
                    .map_err(|_| DbErr::Type("negative cached trend count".to_owned()))
                    .map(|(favourites_count, reblogs_count)| TrendingStatus {
                        item,
                        favourites_count,
                        reblogs_count,
                    }),
            )
        })
        .collect::<std::result::Result<Vec<_>, DbErr>>()
        .map_err(Into::into)
}

fn preview_card_from_model(model: preview_card::Model) -> Result<PreviewCard> {
    Ok(PreviewCard {
        id: model.id,
        url: model.url,
        title: model.title,
        description: model.description,
        author_name: model.author_name,
        author_url: model.author_url,
        provider_name: model.provider_name,
        provider_url: model.provider_url,
        image_file_path: model.image_file_path,
        image_width: u32::try_from(model.image_width)
            .map_err(|_| DbErr::Type("negative preview width".to_owned()))?,
        image_height: u32::try_from(model.image_height)
            .map_err(|_| DbErr::Type("negative preview height".to_owned()))?,
        blurhash: model.blurhash,
        published_at: model.published_at,
    })
}

/// Load the preview card associated with one local or cached remote status.
pub async fn preview_card_for_status(
    db: &impl ConnectionTrait,
    status: StatusReference,
) -> Result<Option<PreviewCard>> {
    let (local_id, remote_id) = match status {
        StatusReference::Local(id) => (Some(id.0), None),
        StatusReference::Remote(id) => (None, Some(id.0)),
    };
    let mut association = status_preview_card::Entity::find();
    association = match local_id {
        Some(id) => association.filter(status_preview_card::Column::LocalStatusId.eq(id)),
        None => association.filter(status_preview_card::Column::LocalStatusId.is_null()),
    };
    association = match remote_id {
        Some(id) => association.filter(status_preview_card::Column::RemoteStatusId.eq(id)),
        None => association.filter(status_preview_card::Column::RemoteStatusId.is_null()),
    };
    let Some(association) = association.one(db).await? else {
        return Ok(None);
    };
    let model = preview_card::Entity::find_by_id(association.preview_card_id)
        .one(db)
        .await?;
    model.map(preview_card_from_model).transpose()
}

/// Batch-load preview cards for a mixed status collection without per-status queries.
pub async fn preview_cards_for_statuses(
    db: &impl ConnectionTrait,
    statuses: &[StatusReference],
) -> Result<HashMap<StatusReference, PreviewCard>> {
    let local_ids = statuses
        .iter()
        .filter_map(|status| match status {
            StatusReference::Local(id) => Some(id.0),
            StatusReference::Remote(_) => None,
        })
        .collect::<Vec<_>>();
    let remote_ids = statuses
        .iter()
        .filter_map(|status| match status {
            StatusReference::Local(_) => None,
            StatusReference::Remote(id) => Some(id.0),
        })
        .collect::<Vec<_>>();
    if local_ids.is_empty() && remote_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let mut condition = Condition::any();
    if !local_ids.is_empty() {
        condition = condition.add(status_preview_card::Column::LocalStatusId.is_in(local_ids));
    }
    if !remote_ids.is_empty() {
        condition = condition.add(status_preview_card::Column::RemoteStatusId.is_in(remote_ids));
    }
    let associations = status_preview_card::Entity::find()
        .filter(condition)
        .all(db)
        .await?;
    let card_ids = associations
        .iter()
        .map(|association| association.preview_card_id)
        .collect::<HashSet<_>>();
    let cards = preview_card::Entity::find()
        .filter(preview_card::Column::Id.is_in(card_ids))
        .all(db)
        .await?
        .into_iter()
        .map(|model| preview_card_from_model(model).map(|card| (card.id, card)))
        .collect::<Result<HashMap<_, _>>>()?;
    Ok(associations
        .into_iter()
        .filter_map(|association| {
            let status = match (association.local_status_id, association.remote_status_id) {
                (Some(id), None) => StatusReference::Local(StatusId(id)),
                (None, Some(id)) => StatusReference::Remote(StatusId(id)),
                _ => return None,
            };
            cards
                .get(&association.preview_card_id)
                .cloned()
                .map(|card| (status, card))
        })
        .collect())
}

/// Load a preview card by durable job identifier.
pub async fn preview_card_by_id(
    db: &impl ConnectionTrait,
    id: Uuid,
) -> Result<Option<PreviewCard>> {
    preview_card::Entity::find_by_id(id)
        .one(db)
        .await?
        .map(preview_card_from_model)
        .transpose()
}

/// Persist successfully fetched preview metadata.
pub async fn update_preview_card(
    db: &impl ConnectionTrait,
    id: Uuid,
    update: PreviewCardUpdate,
) -> Result<()> {
    let Some(model) = preview_card::Entity::find_by_id(id).one(db).await? else {
        return Ok(());
    };
    let now = OffsetDateTime::now_utc();
    let mut active = model.into_active_model();
    active.title = Set(update.title);
    active.description = Set(update.description);
    active.author_name = Set(update.author_name);
    active.author_url = Set(update.author_url);
    active.provider_name = Set(update.provider_name);
    active.provider_url = Set(update.provider_url);
    active.image_file_path = Set(update.image_file_path);
    active.image_width = Set(i32::try_from(update.image_width)
        .map_err(|_| DbErr::Type("preview width exceeds integer".to_owned()))?);
    active.image_height = Set(i32::try_from(update.image_height)
        .map_err(|_| DbErr::Type("preview height exceeds integer".to_owned()))?);
    active.blurhash = Set(update.blurhash);
    active.published_at = Set(update.published_at);
    active.fetch_state = Set(PreviewFetchState::Ready);
    active.fetched_at = Set(Some(now));
    active.updated_at = Set(now);
    active.update(db).await?;
    Ok(())
}

/// Mark a preview fetch permanently unsuccessful while retaining its compatible URL card.
pub async fn mark_preview_card_failed(db: &impl ConnectionTrait, id: Uuid) -> Result<()> {
    preview_card::Entity::update_many()
        .col_expr(
            preview_card::Column::FetchState,
            Expr::value(PreviewFetchState::Failed),
        )
        .col_expr(
            preview_card::Column::FetchedAt,
            Expr::current_timestamp().into(),
        )
        .col_expr(
            preview_card::Column::UpdatedAt,
            Expr::current_timestamp().into(),
        )
        .filter(preview_card::Column::Id.eq(id))
        .exec(db)
        .await?;
    Ok(())
}

/// Acquire a short cluster-wide lease and rate slot for one preview host.
pub async fn acquire_preview_host_lease(db: &impl ConnectionTrait, host: &str) -> Result<bool> {
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"INSERT INTO preview_fetch_host(host) VALUES ($1)
           ON CONFLICT (host) DO NOTHING"#,
        vec![host.to_owned().into()],
    ))
    .await?;
    let result = db
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"UPDATE preview_fetch_host
               SET lease_until = now() + interval '30 seconds',
                   next_fetch_at = now() + interval '1 second',
                   updated_at = now()
               WHERE host = $1
                 AND next_fetch_at <= now()
                 AND (lease_until IS NULL OR lease_until <= now())"#,
            vec![host.to_owned().into()],
        ))
        .await?;
    Ok(result.rows_affected() == 1)
}

/// Release a preview host lease without relaxing its cluster-wide request spacing.
pub async fn release_preview_host_lease(db: &impl ConnectionTrait, host: &str) -> Result<()> {
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "UPDATE preview_fetch_host SET lease_until = NULL, updated_at = now() WHERE host = $1",
        vec![host.to_owned().into()],
    ))
    .await?;
    Ok(())
}

/// Delete a bounded batch of old, unreferenced preview rows and return their storage keys.
pub async fn prune_preview_cards(
    db: &DbConnection,
    older_than: OffsetDateTime,
) -> Result<Vec<String>> {
    const CANDIDATE_LIMIT: u64 = 500;
    const DELETE_LIMIT: usize = 100;
    let txn = db.begin().await?;
    let candidates = preview_card::Entity::find()
        .filter(preview_card::Column::UpdatedAt.lt(older_than))
        .order_by_asc(preview_card::Column::UpdatedAt)
        .limit(CANDIDATE_LIMIT)
        .all(&txn)
        .await?;
    let ids = candidates.iter().map(|card| card.id).collect::<Vec<_>>();
    let referenced = if ids.is_empty() {
        HashSet::new()
    } else {
        status_preview_card::Entity::find()
            .filter(status_preview_card::Column::PreviewCardId.is_in(ids))
            .all(&txn)
            .await?
            .into_iter()
            .map(|association| association.preview_card_id)
            .collect::<HashSet<_>>()
    };
    let orphaned = candidates
        .into_iter()
        .filter(|card| !referenced.contains(&card.id))
        .take(DELETE_LIMIT)
        .collect::<Vec<_>>();
    let orphaned_ids = orphaned.iter().map(|card| card.id).collect::<Vec<_>>();
    if !orphaned_ids.is_empty() {
        preview_card::Entity::delete_many()
            .filter(preview_card::Column::Id.is_in(orphaned_ids))
            .exec(&txn)
            .await?;
    }
    txn.commit().await?;
    Ok(orphaned
        .into_iter()
        .filter_map(|card| card.image_file_path)
        .collect())
}

/// Rank preview cards from the shared link-trend cache.
pub async fn trending_links(
    db: &impl ConnectionTrait,
    limit: u64,
    offset: u64,
) -> Result<Vec<TrendingLink>> {
    #[derive(FromQueryResult)]
    struct Row {
        id: Uuid,
        url: String,
        title: String,
        description: String,
        author_name: String,
        author_url: String,
        provider_name: String,
        provider_url: String,
        image_file_path: Option<String>,
        image_width: i32,
        image_height: i32,
        blurhash: Option<String>,
        published_at: Option<OffsetDateTime>,
        day: Option<i64>,
        uses: Option<i64>,
        accounts: Option<i64>,
    }
    let rows = Row::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"WITH selected AS (
             SELECT preview_card_id, score FROM link_trend
             WHERE expires_at > now() AND score >= 1
             ORDER BY score DESC, preview_card_id
             LIMIT $1 OFFSET $2
           )
           SELECT card.id, card.url, card.title, card.description,
                  card.author_name, card.author_url, card.provider_name,
                  card.provider_url, card.image_file_path, card.image_width,
                  card.image_height, card.blurhash, card.published_at,
                  extract(epoch FROM usage.usage_day::timestamp)::bigint AS day,
                  usage.uses, usage.accounts
           FROM selected
           JOIN preview_card card ON card.id = selected.preview_card_id
           LEFT JOIN link_daily_usage usage
             ON usage.preview_card_id = card.id
            AND usage.usage_day >= (now() AT TIME ZONE 'UTC')::date - 6
           ORDER BY selected.score DESC, selected.preview_card_id, day DESC"#,
        vec![
            i64::try_from(limit)
                .map_err(|_| DbErr::Type("trend limit exceeds bigint".to_owned()))?
                .into(),
            i64::try_from(offset)
                .map_err(|_| DbErr::Type("trend offset exceeds bigint".to_owned()))?
                .into(),
        ],
    ))
    .all(db)
    .await?;
    let mut trends = Vec::<TrendingLink>::new();
    for row in rows {
        if trends.last().is_none_or(|trend| trend.card.id != row.id) {
            trends.push(TrendingLink {
                card: PreviewCard {
                    id: row.id,
                    url: row.url.clone(),
                    title: row.title.clone(),
                    description: row.description.clone(),
                    author_name: row.author_name.clone(),
                    author_url: row.author_url.clone(),
                    provider_name: row.provider_name.clone(),
                    provider_url: row.provider_url.clone(),
                    image_file_path: row.image_file_path.clone(),
                    image_width: u32::try_from(row.image_width)
                        .map_err(|_| DbErr::Type("negative preview width".to_owned()))?,
                    image_height: u32::try_from(row.image_height)
                        .map_err(|_| DbErr::Type("negative preview height".to_owned()))?,
                    blurhash: row.blurhash.clone(),
                    published_at: row.published_at,
                },
                history: Vec::new(),
            });
        }
        if let (Some(day), Some(uses), Some(accounts), Some(trend)) =
            (row.day, row.uses, row.accounts, trends.last_mut())
        {
            trend.history.push(LocalTagHistory {
                day: u64::try_from(day)
                    .map_err(|_| DbErr::Type("negative link history day".to_owned()))?,
                uses: u64::try_from(uses)
                    .map_err(|_| DbErr::Type("negative link usage count".to_owned()))?,
                accounts: u64::try_from(accounts)
                    .map_err(|_| DbErr::Type("negative link account count".to_owned()))?,
            });
        }
    }
    for trend in &mut trends {
        trend.history = fill_tag_history(mem::take(&mut trend.history));
    }
    Ok(trends)
}

/// Index at most 100 recent statuses that predate preview-card support.
pub async fn backfill_preview_cards(db: &DbConnection) -> Result<TrendRefreshOutcome> {
    #[derive(FromQueryResult)]
    struct Row {
        local_status_id: Option<Uuid>,
        remote_status_id: Option<Uuid>,
        content: String,
        published_at: OffsetDateTime,
        actor_id: Uuid,
    }
    const BATCH_SIZE: usize = 100;
    let txn = db.begin().await?;
    let rows = Row::find_by_statement(Statement::from_string(
        DatabaseBackend::Postgres,
        r#"SELECT * FROM (
             SELECT status.id AS local_status_id, NULL::uuid AS remote_status_id,
                    status.content, status.created_at AS published_at,
                    status.account_id AS actor_id
             FROM local_status status
             WHERE status.deleted_at IS NULL
               AND status.created_at >= now() - interval '8 days'
               AND NOT EXISTS (
                 SELECT 1 FROM status_preview_scan scan
                 WHERE scan.local_status_id = status.id)
             UNION ALL
             SELECT NULL::uuid, status.id, status.content, status.published_at,
                    status.remote_actor_id
             FROM remote_status status
             WHERE status.deleted_at IS NULL
               AND status.published_at >= now() - interval '8 days'
               AND NOT EXISTS (
                 SELECT 1 FROM status_preview_scan scan
                 WHERE scan.remote_status_id = status.id)
           ) candidates
           ORDER BY published_at, coalesce(local_status_id, remote_status_id)
           LIMIT 100"#
            .to_owned(),
    ))
    .all(&txn)
    .await?;
    for row in &rows {
        let (target, origin, remote_html) = match (row.local_status_id, row.remote_status_id) {
            (Some(id), None) => (
                PreviewStatusTarget::Local(StatusId(id)),
                PreviewActorOrigin::Local,
                false,
            ),
            (None, Some(id)) => (
                PreviewStatusTarget::Remote(StatusId(id)),
                PreviewActorOrigin::Remote,
                true,
            ),
            _ => continue,
        };
        Box::pin(replace_status_preview_card(
            &txn,
            target,
            &row.content,
            utc_date(row.published_at),
            origin,
            row.actor_id,
            remote_html,
        ))
        .await?;
    }
    let processed = rows.len();
    txn.commit().await?;
    Ok(TrendRefreshOutcome {
        has_more: processed == BATCH_SIZE,
        processed,
    })
}

/// Enqueue the singleton preview backfill only when an upgrade left recent statuses unscanned.
pub async fn enqueue_preview_backfill_if_needed(db: &DbConnection) -> Result<Option<JobId>> {
    #[derive(FromQueryResult)]
    struct Pending {
        pending: bool,
    }
    let txn = db
        .begin_with_config(None, Some(AccessMode::ReadOnly))
        .await?;
    let pending = Pending::find_by_statement(Statement::from_string(
        DatabaseBackend::Postgres,
        r#"SELECT EXISTS (
             SELECT 1 FROM local_status status
             WHERE status.deleted_at IS NULL
               AND status.created_at >= now() - interval '8 days'
               AND NOT EXISTS (
                 SELECT 1 FROM status_preview_scan scan
                 WHERE scan.local_status_id = status.id)
             UNION ALL
             SELECT 1 FROM remote_status status
             WHERE status.deleted_at IS NULL
               AND status.published_at >= now() - interval '8 days'
               AND NOT EXISTS (
                 SELECT 1 FROM status_preview_scan scan
                 WHERE scan.remote_status_id = status.id)
           ) AS pending"#
            .to_owned(),
    ))
    .one(&txn)
    .await?
    .is_some_and(|row| row.pending);
    txn.commit().await?;
    if !pending {
        return Ok(None);
    }
    enqueue_job(
        db,
        JobKind::PreviewCardBackfill,
        serde_json::json!({}),
        Some("preview-card-backfill"),
        OffsetDateTime::now_utc(),
    )
    .await
    .map(Some)
}

/// Return public statuses sharing an actively trending canonical article URL.
pub async fn link_timeline(
    db: &impl ConnectionTrait,
    url: &str,
    limit: u64,
    cursor: TimelineCursor,
) -> Result<TimelinePage<PublicTimelineItem>> {
    #[derive(FromQueryResult)]
    struct Row {
        local_status_id: Option<Uuid>,
        remote_status_id: Option<Uuid>,
    }
    let Some(url) = normalize_preview_url(url) else {
        return Ok(TimelinePage {
            items: Vec::new(),
            first_cursor: None,
            last_cursor: None,
            has_more: false,
        });
    };
    let rows = Row::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"SELECT association.local_status_id, association.remote_status_id
           FROM status_preview_card association
           JOIN preview_card card ON card.id = association.preview_card_id
           JOIN link_trend trend ON trend.preview_card_id = card.id
           LEFT JOIN local_status local_status
             ON local_status.id = association.local_status_id
           LEFT JOIN local_account local_account
             ON local_account.id = local_status.account_id
           LEFT JOIN remote_status remote_status
             ON remote_status.id = association.remote_status_id
           LEFT JOIN remote_actor remote_actor
             ON remote_actor.id = remote_status.remote_actor_id
           WHERE card.url = $1 AND trend.score >= 1 AND trend.expires_at > now()
             AND ($2::uuid IS NULL OR
                  coalesce(association.local_status_id, association.remote_status_id) < $2)
             AND ($3::uuid IS NULL OR
                  coalesce(association.local_status_id, association.remote_status_id) > $3)
             AND ($4::uuid IS NULL OR
                  coalesce(association.local_status_id, association.remote_status_id) > $4)
             AND (
               (local_status.id IS NOT NULL AND local_status.deleted_at IS NULL
                AND local_status.visibility = 'public'
                AND local_status.in_reply_to_id IS NULL
                AND local_status.in_reply_to_remote_status_id IS NULL
                AND local_status.sensitive = false
                AND local_status.spoiler_text = ''
                AND local_account.discoverable = true
                AND local_account.limited_at IS NULL
                AND local_account.suspended_at IS NULL)
               OR
               (remote_status.id IS NOT NULL AND remote_status.deleted_at IS NULL
                AND remote_status.visibility = 'public'
                AND remote_status.in_reply_to IS NULL
                AND coalesce(
                    (remote_status.object->>'sensitive')::boolean, false) = false
                AND coalesce(remote_status.object->>'summary', '') = ''
                AND remote_actor.deleted_at IS NULL
                AND remote_actor.limited_at IS NULL
                AND remote_actor.suspended_at IS NULL)
             )
           ORDER BY coalesce(
             association.local_status_id, association.remote_status_id) DESC
           LIMIT $5"#,
        vec![
            url.into(),
            cursor.max_id.map(|id| id.0).into(),
            cursor.since_id.map(|id| id.0).into(),
            cursor.min_id.map(|id| id.0).into(),
            i64::try_from(limit.saturating_add(1))
                .map_err(|_| DbErr::Type("timeline limit exceeds bigint".to_owned()))?
                .into(),
        ],
    ))
    .all(db)
    .await?;
    let has_more = rows.len() > limit as usize;
    let rows = rows.into_iter().take(limit as usize).collect::<Vec<_>>();
    let local_ids = rows
        .iter()
        .filter_map(|row| row.local_status_id)
        .collect::<Vec<_>>();
    let remote_ids = rows
        .iter()
        .filter_map(|row| row.remote_status_id)
        .collect::<Vec<_>>();
    let local = local_status::Entity::find()
        .filter(local_status::Column::Id.is_in(local_ids))
        .all(db)
        .await?
        .into_iter()
        .map(|model| local_status_from_model(model).map(|status| (status.id.0, status)))
        .collect::<Result<HashMap<_, _>>>()?;
    let remote = remote_status::Entity::find()
        .filter(remote_status::Column::Id.is_in(remote_ids))
        .all(db)
        .await?
        .into_iter()
        .map(|model| remote_status_from_model(model).map(|status| (status.id.0, status)))
        .collect::<Result<HashMap<_, _>>>()?;
    let items = rows
        .iter()
        .filter_map(|row| match (row.local_status_id, row.remote_status_id) {
            (Some(id), None) => local.get(&id).cloned().map(PublicTimelineItem::Local),
            (None, Some(id)) => remote.get(&id).cloned().map(PublicTimelineItem::Remote),
            _ => None,
        })
        .collect::<Vec<_>>();
    Ok(TimelinePage {
        first_cursor: items.first().map(|item| match item {
            PublicTimelineItem::Local(status) => status.id.0,
            PublicTimelineItem::Remote(status) => status.id.0,
        }),
        last_cursor: items.last().map(|item| match item {
            PublicTimelineItem::Local(status) => status.id.0,
            PublicTimelineItem::Remote(status) => status.id.0,
        }),
        items,
        has_more,
    })
}

/// Outcome of one bounded Rust-owned trend refresh batch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TrendRefreshOutcome {
    /// More dirty targets remain and should be processed immediately.
    pub has_more: bool,
    /// Number of targets scored by this worker.
    pub processed: usize,
}

#[derive(Debug, FromQueryResult)]
struct DirtyTrendRow {
    kind: String,
    target_id: Uuid,
}

#[derive(Debug, FromQueryResult)]
struct TrendRefreshScheduleRow {
    interval_milliseconds: i64,
    next_run_at: OffsetDateTime,
}

/// Initialize the shared trend schedule or verify this instance uses its cadence.
pub async fn configure_trend_refresh_schedule(db: &DbConnection, interval: Duration) -> Result<()> {
    let interval_milliseconds = trend_interval_milliseconds(interval)?;
    let txn = db.begin().await?;
    txn.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"INSERT INTO trend_refresh_schedule(
               id, interval_milliseconds, next_run_at, updated_at)
           VALUES (1, $1, now(), now())
           ON CONFLICT (id) DO NOTHING"#,
        vec![interval_milliseconds.into()],
    ))
    .await?;
    let schedule = TrendRefreshScheduleRow::find_by_statement(Statement::from_string(
        DatabaseBackend::Postgres,
        r#"SELECT interval_milliseconds, next_run_at
           FROM trend_refresh_schedule WHERE id = 1"#
            .to_owned(),
    ))
    .one(&txn)
    .await?
    .ok_or_else(|| {
        RoostyError::Configuration("trend refresh schedule could not be initialized".to_owned())
    })?;
    if schedule.interval_milliseconds != interval_milliseconds {
        return Err(RoostyError::Configuration(format!(
            "ROOSTY_TRENDS_REFRESH_INTERVAL conflicts with the shared database schedule: \
             configured {interval_milliseconds}ms, stored {}ms",
            schedule.interval_milliseconds
        )));
    }
    txn.commit().await?;
    Ok(())
}

/// Claim one due schedule without waiting, advance it, and enqueue its durable job atomically.
pub async fn enqueue_due_trend_refresh(db: &DbConnection) -> Result<Option<JobId>> {
    let txn = db.begin().await?;
    let schedule = TrendRefreshScheduleRow::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"SELECT interval_milliseconds, next_run_at
           FROM trend_refresh_schedule
           WHERE id = 1
             AND next_run_at <= now()
             AND NOT EXISTS (
               SELECT 1 FROM job
               WHERE kind = $1 AND completed_at IS NULL
             )
           FOR UPDATE SKIP LOCKED"#,
        vec![JobKind::TrendMaintenance.as_str().to_owned().into()],
    ))
    .one(&txn)
    .await?;
    let Some(schedule) = schedule else {
        txn.commit().await?;
        return Ok(None);
    };
    let now = OffsetDateTime::now_utc();
    let next_run_at =
        next_trend_refresh_at(now, schedule.interval_milliseconds).ok_or_else(|| {
            RoostyError::Configuration("trend refresh schedule timestamp overflowed".to_owned())
        })?;
    txn.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"UPDATE trend_refresh_schedule
           SET next_run_at = $1, updated_at = $2
           WHERE id = 1"#,
        vec![next_run_at.into(), now.into()],
    ))
    .await?;
    let scheduled_milliseconds = schedule
        .next_run_at
        .unix_timestamp_nanos()
        .div_euclid(1_000_000);
    let job_id = enqueue_job_in_transaction(
        &txn,
        NewJob {
            kind: JobKind::TrendMaintenance,
            payload: serde_json::json!({}),
            deduplication_key: Some(format!("trend-refresh:{scheduled_milliseconds}")),
            run_after: now,
        },
    )
    .await?;
    txn.commit().await?;
    Ok(Some(job_id))
}

fn trend_interval_milliseconds(interval: Duration) -> Result<i64> {
    i64::try_from(interval.whole_milliseconds()).map_err(|_| {
        RoostyError::Configuration("ROOSTY_TRENDS_REFRESH_INTERVAL is too large".to_owned())
    })
}

fn next_trend_refresh_at(
    now: OffsetDateTime,
    interval_milliseconds: i64,
) -> Option<OffsetDateTime> {
    let now_milliseconds = now.unix_timestamp_nanos().div_euclid(1_000_000);
    let interval_milliseconds = i128::from(interval_milliseconds);
    let next_milliseconds = now_milliseconds
        .div_euclid(interval_milliseconds)
        .checked_add(1)?
        .checked_mul(interval_milliseconds)?;
    OffsetDateTime::from_unix_timestamp_nanos(next_milliseconds.checked_mul(1_000_000)?).ok()
}

/// Refresh at most 100 trend targets, returning whether a continuation is needed.
pub async fn maintain_trends(db: &DbConnection) -> Result<TrendRefreshOutcome> {
    const BATCH_SIZE: usize = 100;
    let txn = db.begin().await?;
    txn.execute_unprepared(
        r#"
        DELETE FROM tag_daily_actor_usage
        WHERE usage_day < (now() AT TIME ZONE 'UTC')::date - 8;
        DELETE FROM tag_daily_usage
        WHERE usage_day < (now() AT TIME ZONE 'UTC')::date - 8;
        DELETE FROM link_daily_usage
        WHERE usage_day < (now() AT TIME ZONE 'UTC')::date - 8;
        DELETE FROM tag_trend WHERE expires_at <= now();
        DELETE FROM link_trend WHERE expires_at <= now();
        INSERT INTO trend_dirty(kind, target_id)
        SELECT CASE WHEN local_status_id IS NULL
                    THEN 'remote_status' ELSE 'local_status' END,
               coalesce(local_status_id, remote_status_id)
        FROM status_trend_metric
        WHERE score >= 1
        ON CONFLICT (kind, target_id) DO NOTHING;
        INSERT INTO trend_dirty(kind, target_id)
        SELECT 'tag', tag_id FROM tag_trend WHERE score >= 1
        ON CONFLICT (kind, target_id) DO NOTHING;
        INSERT INTO trend_dirty(kind, target_id)
        SELECT 'link', preview_card_id FROM link_trend WHERE score >= 1
        ON CONFLICT (kind, target_id) DO NOTHING;
        "#,
    )
    .await?;
    let claimed = DirtyTrendRow::find_by_statement(Statement::from_string(
        DatabaseBackend::Postgres,
        format!(
            r#"SELECT kind, target_id FROM trend_dirty
               ORDER BY touched_at, kind, target_id
               LIMIT {BATCH_SIZE}
               FOR UPDATE SKIP LOCKED"#
        ),
    ))
    .all(&txn)
    .await?;
    let now = OffsetDateTime::now_utc();
    for row in &claimed {
        match row.kind.as_str() {
            "local_status" => {
                refresh_status_trend(&txn, TrendTarget::LocalStatus(StatusId(row.target_id)), now)
                    .await?;
            }
            "remote_status" => {
                refresh_status_trend(
                    &txn,
                    TrendTarget::RemoteStatus(StatusId(row.target_id)),
                    now,
                )
                .await?;
            }
            "tag" => refresh_tag_trend(&txn, row.target_id, now).await?,
            "link" => refresh_link_trend(&txn, row.target_id, now).await?,
            _ => {
                return Err(RoostyError::InvalidInput(
                    "stored trend target kind is invalid".to_owned(),
                ));
            }
        }
        txn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "DELETE FROM trend_dirty WHERE kind = $1 AND target_id = $2",
            vec![row.kind.clone().into(), row.target_id.into()],
        ))
        .await?;
    }
    let has_more = DirtyTrendRow::find_by_statement(Statement::from_string(
        DatabaseBackend::Postgres,
        "SELECT kind, target_id FROM trend_dirty LIMIT 1".to_owned(),
    ))
    .one(&txn)
    .await?
    .is_some();
    txn.commit().await?;
    Ok(TrendRefreshOutcome {
        has_more,
        processed: claimed.len(),
    })
}

async fn refresh_status_trend(
    txn: &DatabaseTransaction,
    target: TrendTarget,
    now: OffsetDateTime,
) -> Result<()> {
    #[derive(Debug, FromQueryResult)]
    struct Eligibility {
        published_at: OffsetDateTime,
        eligible: bool,
    }
    let (kind, id) = target.persisted();
    let eligibility_sql = match target {
        TrendTarget::LocalStatus(_) => {
            r#"SELECT status.created_at AS published_at,
                      status.deleted_at IS NULL
                      AND status.visibility = 'public'
                      AND status.in_reply_to_id IS NULL
                      AND status.in_reply_to_remote_status_id IS NULL
                      AND status.sensitive = false
                      AND status.spoiler_text = ''
                      AND account.discoverable = true
                      AND account.limited_at IS NULL
                      AND account.suspended_at IS NULL AS eligible
               FROM local_status status
               JOIN local_account account ON account.id = status.account_id
               WHERE status.id = $1"#
        }
        TrendTarget::RemoteStatus(_) => {
            r#"SELECT status.published_at,
                      status.deleted_at IS NULL
                      AND status.visibility = 'public'
                      AND status.in_reply_to IS NULL
                      AND coalesce((status.object->>'sensitive')::boolean, false) = false
                      AND coalesce(status.object->>'summary', '') = ''
                      AND actor.deleted_at IS NULL
                      AND actor.limited_at IS NULL
                      AND actor.suspended_at IS NULL AS eligible
               FROM remote_status status
               JOIN remote_actor actor ON actor.id = status.remote_actor_id
               WHERE status.id = $1"#
        }
        TrendTarget::Tag(_) => unreachable!("validated status target"),
    };
    let eligibility = Eligibility::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        eligibility_sql,
        vec![id.into()],
    ))
    .one(txn)
    .await?;
    let Some(eligibility) = eligibility else {
        return Ok(());
    };
    #[derive(Debug, FromQueryResult)]
    struct Counts {
        favourites_count: i64,
        reblogs_count: i64,
    }
    let counts_sql = match target {
        TrendTarget::LocalStatus(_) => {
            r#"SELECT
                 (SELECT count(*) FROM local_status_favourite WHERE status_id = $1)
                 + (SELECT count(*) FROM remote_status_favourite
                    WHERE local_status_id = $1) AS favourites_count,
                 (SELECT count(*) FROM local_status_reblog WHERE status_id = $1)
                 + (SELECT count(*) FROM remote_status_reblog
                    WHERE local_status_id = $1) AS reblogs_count"#
        }
        TrendTarget::RemoteStatus(_) => {
            r#"SELECT
                 (SELECT count(*) FROM local_remote_status_favourite
                    WHERE remote_status_id = $1) AS favourites_count,
                 (SELECT count(*) FROM local_remote_status_reblog
                    WHERE remote_status_id = $1)
                 + (SELECT count(*) FROM remote_status_reblog
                    WHERE remote_status_id = $1) AS reblogs_count"#
        }
        TrendTarget::Tag(_) => {
            return Err(RoostyError::InvalidInput(
                "tag is not a status trend target".to_owned(),
            ));
        }
    };
    let counts = Counts::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        counts_sql,
        vec![id.into()],
    ))
    .one(txn)
    .await?;
    let Some(counts) = counts else {
        return Ok(());
    };
    let interactions = counts.favourites_count + counts.reblogs_count;
    let age_seconds = (now - eligibility.published_at).as_seconds_f64().max(0.0);
    let score = status_trend_score(interactions, age_seconds, eligibility.eligible);
    let expires_at = status_trend_expiry(eligibility.published_at, interactions);
    txn.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"UPDATE status_trend_metric
           SET favourites_count = $3, reblogs_count = $4,
               published_at = $5, score = $6, expires_at = $7, updated_at = $8
           WHERE ($1 = 'local_status' AND local_status_id = $2)
              OR ($1 = 'remote_status' AND remote_status_id = $2)"#,
        vec![
            kind.to_owned().into(),
            id.into(),
            counts.favourites_count.into(),
            counts.reblogs_count.into(),
            eligibility.published_at.into(),
            score.into(),
            expires_at.into(),
            now.into(),
        ],
    ))
    .await?;
    Ok(())
}

fn status_trend_score(interactions: i64, age_seconds: f64, eligible: bool) -> f64 {
    if !eligible || interactions < 5 {
        return 0.0;
    }
    ((interactions - 1) as f64).powi(2) * 0.5_f64.powf(age_seconds / 3_600.0)
}

fn status_trend_expiry(published_at: OffsetDateTime, interactions: i64) -> Option<OffsetDateTime> {
    if interactions < 5 {
        return None;
    }
    let seconds = 3_600.0 * ((((interactions - 1) as f64).powi(2) / 0.3).ln() / 2.0_f64.ln());
    Some(published_at + Duration::seconds_f64(seconds))
}

async fn refresh_tag_trend(
    txn: &DatabaseTransaction,
    tag_id: Uuid,
    now: OffsetDateTime,
) -> Result<()> {
    #[derive(Debug, FromQueryResult)]
    struct Usage {
        observed: i64,
        expected: i64,
        peak_score: Option<f64>,
        peak_at: Option<OffsetDateTime>,
    }
    txn.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"INSERT INTO tag_daily_usage(tag_id, usage_day, uses, accounts, updated_at)
           SELECT tag_id, usage_day, sum(uses), count(*), $2
           FROM tag_daily_actor_usage
           WHERE tag_id = $1
           GROUP BY tag_id, usage_day
           ON CONFLICT (tag_id, usage_day) DO UPDATE SET
             uses = EXCLUDED.uses, accounts = EXCLUDED.accounts,
             updated_at = EXCLUDED.updated_at"#,
        vec![tag_id.into(), now.into()],
    ))
    .await?;
    txn.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"DELETE FROM tag_daily_usage usage
           WHERE tag_id = $1 AND NOT EXISTS (
             SELECT 1 FROM tag_daily_actor_usage actor
             WHERE actor.tag_id = usage.tag_id
               AND actor.usage_day = usage.usage_day)"#,
        vec![tag_id.into()],
    ))
    .await?;
    let usage = Usage::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"SELECT
             coalesce(max(usage.accounts) FILTER (
               WHERE usage.usage_day = ($2 AT TIME ZONE 'UTC')::date), 0) AS observed,
             coalesce(max(usage.accounts) FILTER (
               WHERE usage.usage_day = ($2 AT TIME ZONE 'UTC')::date - 1), 1) AS expected,
             trend.peak_score, trend.peak_at
           FROM tag_daily_usage usage
           LEFT JOIN tag_trend trend ON trend.tag_id = usage.tag_id
           WHERE usage.tag_id = $1
           GROUP BY trend.peak_score, trend.peak_at"#,
        vec![tag_id.into(), now.into()],
    ))
    .one(txn)
    .await?;
    let Some(usage) = usage else {
        txn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "DELETE FROM tag_trend WHERE tag_id = $1",
            vec![tag_id.into()],
        ))
        .await?;
        return Ok(());
    };
    let raw = tag_raw_score(usage.observed, usage.expected);
    let (peak, peak_at) = choose_tag_peak(raw, usage.peak_score, usage.peak_at, now);
    if peak <= 0.0 {
        txn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "DELETE FROM tag_trend WHERE tag_id = $1",
            vec![tag_id.into()],
        ))
        .await?;
        return Ok(());
    }
    let score = peak * 0.5_f64.powf((now - peak_at).as_seconds_f64().max(0.0) / 14_400.0);
    let expires_at = peak_at + Duration::seconds_f64(14_400.0 * peak.ln() / 2.0_f64.ln());
    txn.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"INSERT INTO tag_trend(tag_id, score, peak_score, peak_at, expires_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, $6)
           ON CONFLICT (tag_id) DO UPDATE SET score = EXCLUDED.score,
             peak_score = EXCLUDED.peak_score, peak_at = EXCLUDED.peak_at,
             expires_at = EXCLUDED.expires_at, updated_at = EXCLUDED.updated_at"#,
        vec![
            tag_id.into(),
            score.into(),
            peak.into(),
            peak_at.into(),
            expires_at.into(),
            now.into(),
        ],
    ))
    .await?;
    Ok(())
}

fn tag_raw_score(observed: i64, expected: i64) -> f64 {
    let expected = expected.max(1);
    if observed < 5 || expected > observed {
        0.0
    } else {
        ((observed - expected) as f64).powi(2) / expected as f64
    }
}

fn choose_tag_peak(
    raw: f64,
    old_peak: Option<f64>,
    old_peak_at: Option<OffsetDateTime>,
    now: OffsetDateTime,
) -> (f64, OffsetDateTime) {
    match (old_peak, old_peak_at) {
        (Some(peak), Some(at)) if at >= now - Duration::days(2) && raw <= peak => (peak, at),
        _ => (raw, now),
    }
}

async fn refresh_link_trend(
    txn: &DatabaseTransaction,
    card_id: Uuid,
    now: OffsetDateTime,
) -> Result<()> {
    #[derive(FromQueryResult)]
    struct Usage {
        observed: i64,
        expected: i64,
        peak_score: Option<f64>,
        peak_at: Option<OffsetDateTime>,
    }
    txn.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"INSERT INTO link_daily_usage(
               preview_card_id, usage_day, uses, accounts, updated_at)
           SELECT association.preview_card_id, association.usage_day,
                  count(*), count(DISTINCT
                    (association.actor_origin, association.actor_id)), $2
           FROM status_preview_card association
           LEFT JOIN local_status local_status
             ON local_status.id = association.local_status_id
           LEFT JOIN local_account local_account
             ON local_account.id = local_status.account_id
           LEFT JOIN remote_status remote_status
             ON remote_status.id = association.remote_status_id
           LEFT JOIN remote_actor remote_actor
             ON remote_actor.id = remote_status.remote_actor_id
           WHERE association.preview_card_id = $1
             AND association.usage_day >= ($2 AT TIME ZONE 'UTC')::date - 8
             AND (
               (local_status.id IS NOT NULL
                AND local_status.deleted_at IS NULL
                AND local_status.visibility = 'public'
                AND local_status.in_reply_to_id IS NULL
                AND local_status.in_reply_to_remote_status_id IS NULL
                AND local_status.sensitive = false
                AND local_status.spoiler_text = ''
                AND local_account.discoverable = true
                AND local_account.limited_at IS NULL
                AND local_account.suspended_at IS NULL)
               OR
               (remote_status.id IS NOT NULL
                AND remote_status.deleted_at IS NULL
                AND remote_status.visibility = 'public'
                AND remote_status.in_reply_to IS NULL
                AND coalesce((remote_status.object->>'sensitive')::boolean, false) = false
                AND coalesce(remote_status.object->>'summary', '') = ''
                AND remote_actor.deleted_at IS NULL
                AND remote_actor.limited_at IS NULL
                AND remote_actor.suspended_at IS NULL)
             )
           GROUP BY association.preview_card_id, association.usage_day
           ON CONFLICT (preview_card_id, usage_day) DO UPDATE SET
             uses = EXCLUDED.uses, accounts = EXCLUDED.accounts,
             updated_at = EXCLUDED.updated_at"#,
        vec![card_id.into(), now.into()],
    ))
    .await?;
    txn.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"DELETE FROM link_daily_usage usage
           WHERE preview_card_id = $1
             AND NOT EXISTS (
               SELECT 1 FROM status_preview_card association
               WHERE association.preview_card_id = usage.preview_card_id
                 AND association.usage_day = usage.usage_day)"#,
        vec![card_id.into()],
    ))
    .await?;
    let usage = Usage::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"SELECT
             coalesce(max(usage.accounts) FILTER (
               WHERE usage.usage_day = ($2 AT TIME ZONE 'UTC')::date), 0) AS observed,
             coalesce(max(usage.accounts) FILTER (
               WHERE usage.usage_day = ($2 AT TIME ZONE 'UTC')::date - 1), 1) AS expected,
             trend.peak_score, trend.peak_at
           FROM link_daily_usage usage
           LEFT JOIN link_trend trend
             ON trend.preview_card_id = usage.preview_card_id
           WHERE usage.preview_card_id = $1
           GROUP BY trend.peak_score, trend.peak_at"#,
        vec![card_id.into(), now.into()],
    ))
    .one(txn)
    .await?;
    let Some(usage) = usage else {
        txn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "DELETE FROM link_trend WHERE preview_card_id = $1",
            vec![card_id.into()],
        ))
        .await?;
        return Ok(());
    };
    let raw = tag_raw_score(usage.observed, usage.expected);
    let (peak, peak_at) = choose_tag_peak(raw, usage.peak_score, usage.peak_at, now);
    if peak <= 0.0 {
        txn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "DELETE FROM link_trend WHERE preview_card_id = $1",
            vec![card_id.into()],
        ))
        .await?;
        return Ok(());
    }
    let score = peak * 0.5_f64.powf((now - peak_at).as_seconds_f64().max(0.0) / 14_400.0);
    let expires_at = peak_at + Duration::seconds_f64(14_400.0 * peak.ln() / 2.0_f64.ln());
    txn.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"INSERT INTO link_trend(
             preview_card_id, score, peak_score, peak_at, expires_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, $6)
           ON CONFLICT (preview_card_id) DO UPDATE SET score = EXCLUDED.score,
             peak_score = EXCLUDED.peak_score, peak_at = EXCLUDED.peak_at,
             expires_at = EXCLUDED.expires_at, updated_at = EXCLUDED.updated_at"#,
        vec![
            card_id.into(),
            score.into(),
            peak.into(),
            peak_at.into(),
            expires_at.into(),
            now.into(),
        ],
    ))
    .await?;
    Ok(())
}

/// List public local and cached remote statuses containing a hashtag.
pub async fn tag_timeline(
    db: &impl ConnectionTrait,
    tag: &str,
    options: TagTimelineOptions,
    limit: u64,
    cursor: TimelineCursor,
) -> Result<TimelinePage<PublicTimelineItem>> {
    let Some(primary) = find_local_tag_by_name(db, tag).await? else {
        return Ok(TimelinePage {
            items: Vec::new(),
            first_cursor: None,
            last_cursor: None,
            has_more: false,
        });
    };
    let mut all_tag_ids = Vec::new();
    for tag in &options.all {
        if let Some(tag) = find_local_tag_by_name(db, tag).await? {
            all_tag_ids.push(tag.id);
        } else {
            return Ok(TimelinePage {
                items: Vec::new(),
                first_cursor: None,
                last_cursor: None,
                has_more: false,
            });
        }
    }
    let any_tags = local_tags_by_names(db, &options.any).await?;
    if !options.any.is_empty() && any_tags.is_empty() {
        return Ok(TimelinePage {
            items: Vec::new(),
            first_cursor: None,
            last_cursor: None,
            has_more: false,
        });
    }
    let none_tags = local_tags_by_names(db, &options.none).await?;
    let any_tag_ids = any_tags.iter().map(|tag| tag.id).collect::<Vec<_>>();
    let none_tag_ids = none_tags.iter().map(|tag| tag.id).collect::<Vec<_>>();
    let hidden_local_ids = if let Some(viewer) = options.viewer {
        hidden_local_account_ids_for_account(db, viewer)
            .await?
            .into_iter()
            .map(|id| id.0)
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let hidden_remote_ids = if let Some(viewer) = options.viewer {
        hidden_remote_actor_ids_for_account(db, viewer)
            .await?
            .into_iter()
            .map(|id| id.0)
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    let mut items = Vec::new();
    if options.origin != PublicTimelineOrigin::Remote {
        let mut query = local_status::Entity::find()
            .filter(local_status::Column::Visibility.eq(StatusVisibility::Public))
            .filter(local_status::Column::DeletedAt.is_null())
            .filter(local_status::Column::Id.in_subquery(status_tag_subquery(primary.id)))
            .filter(
                local_status::Column::AccountId.in_subquery(
                    Query::select()
                        .column(local_account::Column::Id)
                        .from(local_account::Entity)
                        .and_where(local_account::Column::LimitedAt.is_null())
                        .and_where(local_account::Column::SuspendedAt.is_null())
                        .to_owned(),
                ),
            );
        for tag_id in &all_tag_ids {
            query =
                query.filter(local_status::Column::Id.in_subquery(status_tag_subquery(*tag_id)));
        }
        if !any_tag_ids.is_empty() {
            query = query.filter(
                local_status::Column::Id.in_subquery(status_tags_subquery(any_tag_ids.clone())),
            );
        }
        if !none_tag_ids.is_empty() {
            query = query.filter(
                local_status::Column::Id
                    .not_in_subquery(status_tags_subquery(none_tag_ids.clone())),
            );
        }
        if options.only_media {
            query = query.filter(local_status::Column::Id.in_subquery(media_status_subquery()));
        }
        if !hidden_local_ids.is_empty() {
            query = query.filter(local_status::Column::AccountId.is_not_in(hidden_local_ids));
        }
        let statuses = apply_timeline_cursor(query, cursor)
            .order_by_desc(local_status::Column::Id)
            .limit(page_query_limit(limit))
            .all(db)
            .await?;
        items.extend(
            statuses
                .into_iter()
                .map(|status| local_status_from_model(status).map(PublicTimelineItem::Local))
                .collect::<Result<Vec<_>>>()?,
        );
    }

    if options.origin != PublicTimelineOrigin::Local && !options.allowed_remote_domains.is_empty() {
        let mut actor_condition = Condition::all()
            .add(remote_actor::Column::DeletedAt.is_null())
            .add(remote_actor::Column::LimitedAt.is_null())
            .add(remote_actor::Column::SuspendedAt.is_null());
        if !options
            .allowed_remote_domains
            .iter()
            .any(|domain| domain == "*")
        {
            actor_condition = actor_condition
                .add(remote_actor::Column::Domain.is_in(options.allowed_remote_domains.clone()));
        }
        for domain in &options.blocked_remote_domains {
            actor_condition = actor_condition
                .add(remote_actor::Column::Domain.ne(domain.clone()))
                .add(remote_actor::Column::Domain.not_like(format!("%.{domain}")));
        }
        let allowed_actors = Query::select()
            .column(remote_actor::Column::Id)
            .from(remote_actor::Entity)
            .and_where(actor_condition.into())
            .to_owned();
        let mut query = remote_status::Entity::find()
            .filter(remote_status::Column::Visibility.eq(StatusVisibility::Public))
            .filter(remote_status::Column::DeletedAt.is_null())
            .filter(remote_status::Column::RemoteActorId.in_subquery(allowed_actors))
            .filter(remote_status::Column::Id.in_subquery(remote_status_tag_subquery(primary.id)));
        for tag_id in &all_tag_ids {
            query = query
                .filter(remote_status::Column::Id.in_subquery(remote_status_tag_subquery(*tag_id)));
        }
        if !any_tag_ids.is_empty() {
            query = query.filter(
                remote_status::Column::Id.in_subquery(remote_status_tags_subquery(any_tag_ids)),
            );
        }
        if !none_tag_ids.is_empty() {
            query = query.filter(
                remote_status::Column::Id
                    .not_in_subquery(remote_status_tags_subquery(none_tag_ids)),
            );
        }
        if options.only_media {
            query = query.filter(
                remote_status::Column::Id.in_subquery(
                    Query::select()
                        .column(remote_media_attachment::Column::RemoteStatusId)
                        .from(remote_media_attachment::Entity)
                        .to_owned(),
                ),
            );
        }
        if !hidden_remote_ids.is_empty() {
            query = query.filter(remote_status::Column::RemoteActorId.is_not_in(hidden_remote_ids));
        }
        if let Some(max_id) = cursor.max_id {
            query = query.filter(remote_status::Column::Id.lt(max_id.0));
        }
        if let Some(since_id) = cursor.since_id {
            query = query.filter(remote_status::Column::Id.gt(since_id.0));
        }
        if let Some(min_id) = cursor.min_id {
            query = query.filter(remote_status::Column::Id.gt(min_id.0));
        }
        let statuses = query
            .order_by_desc(remote_status::Column::Id)
            .limit(page_query_limit(limit))
            .all(db)
            .await?;
        items.extend(
            statuses
                .into_iter()
                .map(|status| remote_status_from_model(status).map(PublicTimelineItem::Remote))
                .collect::<Result<Vec<_>>>()?,
        );
    }

    items.sort_by_key(|item| {
        Reverse(match item {
            PublicTimelineItem::Local(status) => status.id.0,
            PublicTimelineItem::Remote(status) => status.id.0,
        })
    });
    let (items, has_more) = trim_to_page(items, limit);
    let item_id = |item: &PublicTimelineItem| match item {
        PublicTimelineItem::Local(status) => status.id.0,
        PublicTimelineItem::Remote(status) => status.id.0,
    };
    Ok(TimelinePage {
        first_cursor: items.first().map(item_id),
        last_cursor: items.last().map(item_id),
        items,
        has_more,
    })
}

async fn find_or_create_local_tag<C>(
    db: &C,
    name: &str,
    now: OffsetDateTime,
) -> Result<local_tag::Model>
where
    C: ConnectionTrait,
{
    let name = normalize_tag_name(name);
    if let Some(tag) = local_tag::Entity::find()
        .filter(local_tag::Column::Name.eq(&name))
        .one(db)
        .await?
    {
        return Ok(tag);
    }

    local_tag::Entity::insert(local_tag::ActiveModel {
        id: Set(Uuid::now_v7()),
        name: Set(name.clone()),
        created_at: Set(now),
        updated_at: Set(now),
    })
    .on_conflict(
        OnConflict::column(local_tag::Column::Name)
            .do_nothing()
            .to_owned(),
    )
    .exec(db)
    .await?;
    local_tag::Entity::find()
        .filter(local_tag::Column::Name.eq(&name))
        .one(db)
        .await?
        .ok_or_else(|| RoostyError::InvalidInput(format!("hashtag {name} disappeared")))
}

async fn local_tags_by_names(db: &impl ConnectionTrait, names: &[String]) -> Result<Vec<LocalTag>> {
    let mut tags = Vec::new();
    for name in names {
        if let Some(tag) = find_local_tag_by_name(db, name).await? {
            tags.push(tag);
        }
    }

    Ok(tags)
}

fn status_tag_subquery(tag_id: Uuid) -> sea_orm::sea_query::SelectStatement {
    status_tags_subquery(vec![tag_id])
}

fn status_tags_subquery(tag_ids: Vec<Uuid>) -> sea_orm::sea_query::SelectStatement {
    Query::select()
        .column(local_status_tag::Column::StatusId)
        .from(local_status_tag::Entity)
        .and_where(local_status_tag::Column::TagId.is_in(tag_ids))
        .to_owned()
}

fn remote_status_tag_subquery(tag_id: Uuid) -> sea_orm::sea_query::SelectStatement {
    remote_status_tags_subquery(vec![tag_id])
}

fn remote_status_tags_subquery(tag_ids: Vec<Uuid>) -> sea_orm::sea_query::SelectStatement {
    Query::select()
        .column(remote_status_tag::Column::RemoteStatusId)
        .from(remote_status_tag::Entity)
        .and_where(remote_status_tag::Column::TagId.is_in(tag_ids))
        .to_owned()
}

fn media_status_subquery() -> sea_orm::sea_query::SelectStatement {
    Query::select()
        .column(local_media_attachment::Column::StatusId)
        .from(local_media_attachment::Entity)
        .and_where(local_media_attachment::Column::StatusId.is_not_null())
        .to_owned()
}

fn normalize_tag_name(name: &str) -> String {
    name.trim().trim_start_matches('#').to_lowercase()
}

/// Normalize and validate a Mastodon-compatible featured hashtag name.
pub fn normalize_featured_tag_name(name: &str) -> Option<String> {
    let name = normalize_tag_name(name);
    if name.is_empty()
        || name.chars().count() > 100
        || !name
            .chars()
            .all(|character| character.is_alphanumeric() || character == '_')
        || !name.chars().any(char::is_alphabetic)
    {
        return None;
    }
    Some(name)
}

/// List local featured tags and their visible-status statistics in one aggregate query.
pub async fn local_featured_tags(
    db: &impl ConnectionTrait,
    account_id: AccountId,
) -> Result<Vec<FeaturedTag>> {
    FeaturedTag::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"SELECT ft.id, t.name, NULL::text AS href,
                  count(s.id)::bigint AS statuses_count,
                  max(s.created_at) AS last_status_at, ft.created_at
           FROM local_featured_tag ft
           JOIN local_tag t ON t.id = ft.tag_id
           LEFT JOIN local_status_tag st ON st.tag_id = ft.tag_id
           LEFT JOIN local_status s ON s.id = st.status_id
                AND s.account_id = ft.account_id
                AND s.deleted_at IS NULL
                AND s.visibility IN ('public', 'unlisted')
           WHERE ft.account_id = $1
           GROUP BY ft.id, t.name, ft.created_at
           ORDER BY count(s.id) DESC, ft.created_at DESC, ft.id DESC"#,
        vec![account_id.0.into()],
    ))
    .all(db)
    .await
    .map_err(Into::into)
}

/// List cached remote featured tags with locally known status statistics in one query.
pub async fn remote_featured_tags(
    db: &impl ConnectionTrait,
    actor_id: AccountId,
) -> Result<Vec<FeaturedTag>> {
    FeaturedTag::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"SELECT ft.id, t.name, ft.href,
                  count(s.id)::bigint AS statuses_count,
                  max(s.published_at) AS last_status_at, ft.created_at
           FROM remote_featured_tag ft
           JOIN local_tag t ON t.id = ft.tag_id
           LEFT JOIN remote_status_tag st ON st.tag_id = ft.tag_id
           LEFT JOIN remote_status s ON s.id = st.remote_status_id
                AND s.remote_actor_id = ft.remote_actor_id
                AND s.deleted_at IS NULL
                AND s.visibility IN ('public', 'unlisted')
           WHERE ft.remote_actor_id = $1
           GROUP BY ft.id, t.name, ft.href, ft.position, ft.created_at
           ORDER BY ft.position ASC, ft.id ASC"#,
        vec![actor_id.0.into()],
    ))
    .all(db)
    .await
    .map_err(Into::into)
}

/// Create a local featured tag idempotently while enforcing the limit across processes.
pub async fn feature_local_tag(
    txn: &DatabaseTransaction,
    account_id: AccountId,
    name: &str,
    limit: u64,
) -> Result<FeatureTagResult> {
    txn.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        vec![format!("local-featured-tags:{}", account_id.0).into()],
    ))
    .await?;
    let now = OffsetDateTime::now_utc();
    let tag = find_or_create_local_tag(txn, name, now).await?;
    let mut created = false;
    if local_featured_tag::Entity::find()
        .filter(local_featured_tag::Column::AccountId.eq(account_id.0))
        .filter(local_featured_tag::Column::TagId.eq(tag.id))
        .one(txn)
        .await?
        .is_none()
    {
        if local_featured_tag::Entity::find()
            .filter(local_featured_tag::Column::AccountId.eq(account_id.0))
            .count(txn)
            .await?
            >= limit
        {
            return Ok(FeatureTagResult::LimitReached);
        }
        local_featured_tag::ActiveModel {
            id: Set(Uuid::now_v7()),
            account_id: Set(account_id.0),
            tag_id: Set(tag.id),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(txn)
        .await?;
        created = true;
    }
    let featured = local_featured_tags(txn, account_id)
        .await?
        .into_iter()
        .find(|featured| featured.name == tag.name)
        .ok_or_else(|| DbErr::RecordNotFound("featured tag disappeared".to_owned()))?;
    Ok(FeatureTagResult::Featured {
        tag: featured,
        created,
    })
}

/// Remove a local featured tag owned by the account and return its prior projection.
pub async fn unfeature_local_tag(
    txn: &DatabaseTransaction,
    account_id: AccountId,
    featured_tag_id: Uuid,
) -> Result<Option<FeaturedTag>> {
    txn.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        vec![format!("local-featured-tags:{}", account_id.0).into()],
    ))
    .await?;
    let featured = local_featured_tags(txn, account_id)
        .await?
        .into_iter()
        .find(|featured| featured.id == featured_tag_id);
    if featured.is_some() {
        local_featured_tag::Entity::delete_many()
            .filter(local_featured_tag::Column::Id.eq(featured_tag_id))
            .filter(local_featured_tag::Column::AccountId.eq(account_id.0))
            .exec(txn)
            .await?;
    }
    Ok(featured)
}

/// Suggest recently used, not-yet-featured local tags without per-tag reads.
pub async fn suggested_featured_tags(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    limit: u64,
) -> Result<Vec<LocalTag>> {
    LocalTag::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"SELECT t.id, t.name, t.created_at, t.updated_at
           FROM local_tag t
           JOIN local_status_tag st ON st.tag_id = t.id
           JOIN local_status s ON s.id = st.status_id
           LEFT JOIN local_featured_tag ft
             ON ft.tag_id = t.id AND ft.account_id = s.account_id
           WHERE s.account_id = $1 AND s.deleted_at IS NULL
             AND s.visibility IN ('public', 'unlisted') AND ft.id IS NULL
           GROUP BY t.id, t.name, t.created_at, t.updated_at
           ORDER BY max(s.created_at) DESC, t.name ASC
           LIMIT $2"#,
        vec![account_id.0.into(), limit.into()],
    ))
    .all(db)
    .await
    .map_err(Into::into)
}

/// Atomically replace a remote actor's bounded featured-tag cache.
pub async fn replace_remote_featured_tags(
    txn: &DatabaseTransaction,
    actor_id: AccountId,
    tags: &[RemoteFeaturedTagInput],
) -> Result<()> {
    txn.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        vec![format!("remote-featured:{}", actor_id.0).into()],
    ))
    .await?;
    remote_featured_tag::Entity::delete_many()
        .filter(remote_featured_tag::Column::RemoteActorId.eq(actor_id.0))
        .exec(txn)
        .await?;
    let now = OffsetDateTime::now_utc();
    for (position, input) in tags.iter().enumerate() {
        let tag = find_or_create_local_tag(txn, &input.name, now).await?;
        remote_featured_tag::ActiveModel {
            id: Set(Uuid::now_v7()),
            remote_actor_id: Set(actor_id.0),
            tag_id: Set(tag.id),
            display_name: Set(input.display_name.clone()),
            href: Set(input.href.clone()),
            position: Set(position as i32),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(txn)
        .await?;
    }
    Ok(())
}

/// Apply one signed featured hashtag Add or Remove under the refresh reconciliation lock.
pub async fn apply_remote_featured_tag_activity(
    txn: &DatabaseTransaction,
    actor_id: AccountId,
    input: &RemoteFeaturedTagInput,
    feature: bool,
    limit: u64,
) -> Result<()> {
    txn.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        vec![format!("remote-featured:{}", actor_id.0).into()],
    ))
    .await?;
    let now = OffsetDateTime::now_utc();
    let tag = find_or_create_local_tag(txn, &input.name, now).await?;
    if feature {
        if let Some(existing) = remote_featured_tag::Entity::find()
            .filter(remote_featured_tag::Column::RemoteActorId.eq(actor_id.0))
            .filter(remote_featured_tag::Column::TagId.eq(tag.id))
            .one(txn)
            .await?
        {
            let mut active = existing.into_active_model();
            active.display_name = Set(input.display_name.clone());
            active.href = Set(input.href.clone());
            active.updated_at = Set(now);
            active.update(txn).await?;
        } else {
            remote_featured_tag::ActiveModel {
                id: Set(Uuid::now_v7()),
                remote_actor_id: Set(actor_id.0),
                tag_id: Set(tag.id),
                display_name: Set(input.display_name.clone()),
                href: Set(input.href.clone()),
                position: Set(0),
                created_at: Set(now),
                updated_at: Set(now),
            }
            .insert(txn)
            .await?;
        }
        let rows = remote_featured_tag::Entity::find()
            .filter(remote_featured_tag::Column::RemoteActorId.eq(actor_id.0))
            .order_by_desc(remote_featured_tag::Column::UpdatedAt)
            .order_by_desc(remote_featured_tag::Column::Id)
            .all(txn)
            .await?;
        for row in rows.into_iter().skip(limit as usize) {
            remote_featured_tag::Entity::delete_by_id(row.id)
                .exec(txn)
                .await?;
        }
    } else {
        remote_featured_tag::Entity::delete_many()
            .filter(remote_featured_tag::Column::RemoteActorId.eq(actor_id.0))
            .filter(remote_featured_tag::Column::TagId.eq(tag.id))
            .exec(txn)
            .await?;
    }
    Ok(())
}

async fn local_status_snapshot(
    txn: &DatabaseTransaction,
    status: &local_status::Model,
    created_at: OffsetDateTime,
) -> Result<()> {
    let edit_id = Uuid::now_v7();
    let mut local_mention_ids = local_status_local_mention::Entity::find()
        .filter(local_status_local_mention::Column::StatusId.eq(status.id))
        .filter(local_status_local_mention::Column::Active.eq(true))
        .all(txn)
        .await?
        .into_iter()
        .map(|row| row.account_id)
        .collect::<Vec<_>>();
    let mut remote_mention_ids = local_status_remote_mention::Entity::find()
        .filter(local_status_remote_mention::Column::StatusId.eq(status.id))
        .all(txn)
        .await?
        .into_iter()
        .map(|row| row.remote_actor_id)
        .collect::<Vec<_>>();
    let tag_rows = local_status_tag::Entity::find()
        .filter(local_status_tag::Column::StatusId.eq(status.id))
        .all(txn)
        .await?;
    let mut tag_names = Vec::with_capacity(tag_rows.len());
    for row in tag_rows {
        if let Some(tag) = local_tag::Entity::find_by_id(row.tag_id).one(txn).await? {
            tag_names.push(tag.name);
        }
    }
    local_mention_ids.sort();
    remote_mention_ids.sort();
    tag_names.sort();
    let poll_options = find_poll_for_status(txn, PollStatus::Local(StatusId(status.id)))
        .await?
        .map(|poll| {
            poll.options
                .into_iter()
                .map(|option| option.title)
                .collect::<Vec<_>>()
        });
    local_status_edit::ActiveModel {
        id: Set(edit_id),
        local_status_id: Set(status.id),
        content: Set(status.content.clone()),
        spoiler_text: Set(status.spoiler_text.clone()),
        sensitive: Set(status.sensitive),
        local_mention_ids: Set(serde_json::json!(local_mention_ids)),
        remote_mention_ids: Set(serde_json::json!(remote_mention_ids)),
        tag_names: Set(serde_json::json!(tag_names)),
        poll_options: Set(poll_options.map(|options| serde_json::json!(options))),
        created_at: Set(created_at),
    }
    .insert(txn)
    .await?;
    let media = local_media_attachment::Entity::find()
        .filter(local_media_attachment::Column::StatusId.eq(status.id))
        .order_by_asc(local_media_attachment::Column::StatusOrder)
        .all(txn)
        .await?;
    for item in media {
        local_status_edit_media::ActiveModel {
            id: Set(Uuid::now_v7()),
            local_status_edit_id: Set(edit_id),
            local_media_attachment_id: Set(item.id),
            status_order: Set(item.status_order),
            content_type: Set(item.content_type),
            file_path: Set(item.file_path),
            preview_file_path: Set(item.preview_file_path),
            description: Set(item.description),
            focus_x: Set(item.focus_x),
            focus_y: Set(item.focus_y),
            width: Set(item.width),
            height: Set(item.height),
            preview_width: Set(item.preview_width),
            preview_height: Set(item.preview_height),
            blurhash: Set(item.blurhash),
        }
        .insert(txn)
        .await?;
    }
    Ok(())
}

/// Update an owned local status and its attached media metadata.
pub async fn update_owned_local_status(
    txn: &sea_orm::DatabaseTransaction,
    status_id: StatusId,
    account_id: AccountId,
    mut update: LocalStatusUpdate,
    media_ids: Option<&[Uuid]>,
    media_attributes: &[LocalStatusMediaAttributeUpdate],
    metadata: LocalStatusMetadata,
) -> Result<Option<LocalStatusUpdateResult>> {
    let Some(status) = local_status::Entity::find_by_id(status_id.0)
        .filter(local_status::Column::AccountId.eq(account_id.0))
        .filter(local_status::Column::DeletedAt.is_null())
        .lock_exclusive()
        .one(txn)
        .await?
    else {
        return Ok(None);
    };
    let poll = update.poll.take();

    let current_media = local_media_attachment::Entity::find()
        .filter(local_media_attachment::Column::StatusId.eq(status_id.0))
        .order_by_asc(local_media_attachment::Column::StatusOrder)
        .all(txn)
        .await?;
    let desired_media_ids = media_ids
        .map(<[Uuid]>::to_vec)
        .unwrap_or_else(|| current_media.iter().map(|media| media.id).collect());
    let scalar_changed = update
        .content
        .as_ref()
        .is_some_and(|value| value != &status.content)
        || update
            .sensitive
            .is_some_and(|value| value != status.sensitive)
        || update
            .spoiler_text
            .as_ref()
            .is_some_and(|value| value != &status.spoiler_text)
        || update
            .language
            .as_ref()
            .is_some_and(|value| value != &status.language);
    let media_set_changed = desired_media_ids
        != current_media
            .iter()
            .map(|media| media.id)
            .collect::<Vec<_>>();
    let media_attributes_changed = media_attributes.iter().any(|attribute| {
        current_media
            .iter()
            .find(|media| media.id == attribute.media_id)
            .is_none_or(|media| {
                attribute
                    .description
                    .as_ref()
                    .is_some_and(|description| description != &media.description)
                    || attribute
                        .focus
                        .is_some_and(|(x, y)| media.focus_x != Some(x) || media.focus_y != Some(y))
            })
    });
    let current_poll = find_poll_for_status(txn, PollStatus::Local(status_id)).await?;
    let poll_changed = current_poll.is_some() || poll.is_some();
    let mut desired_tags = metadata
        .tag_names
        .iter()
        .map(|name| normalize_tag_name(name))
        .collect::<Vec<_>>();
    desired_tags.sort();
    desired_tags.dedup();
    let current_tag_rows = local_status_tag::Entity::find()
        .filter(local_status_tag::Column::StatusId.eq(status_id.0))
        .all(txn)
        .await?;
    let current_tag_ids = current_tag_rows
        .iter()
        .map(|row| row.tag_id)
        .collect::<Vec<_>>();
    let mut current_tags = Vec::with_capacity(current_tag_rows.len());
    for row in current_tag_rows {
        if let Some(tag) = local_tag::Entity::find_by_id(row.tag_id).one(txn).await? {
            current_tags.push(tag.name);
        }
    }
    current_tags.sort();
    let mut desired_remote = metadata
        .remote_actor_ids
        .iter()
        .map(|id| id.0)
        .collect::<Vec<_>>();
    desired_remote.sort();
    desired_remote.dedup();
    let mut current_remote = local_status_remote_mention::Entity::find()
        .filter(local_status_remote_mention::Column::StatusId.eq(status_id.0))
        .all(txn)
        .await?
        .into_iter()
        .map(|row| row.remote_actor_id)
        .collect::<Vec<_>>();
    current_remote.sort();
    let metadata_changed = desired_tags != current_tags || desired_remote != current_remote;
    if !scalar_changed
        && !media_set_changed
        && !media_attributes_changed
        && !metadata_changed
        && !poll_changed
    {
        return Ok(Some(LocalStatusUpdateResult::Unchanged(
            local_status_from_model(status)?,
        )));
    }

    if local_status_edit::Entity::find()
        .filter(local_status_edit::Column::LocalStatusId.eq(status_id.0))
        .one(txn)
        .await?
        .is_none()
    {
        let previous_timestamp = if status.updated_at == status.created_at {
            status.created_at
        } else {
            status.updated_at
        };
        local_status_snapshot(txn, &status, previous_timestamp).await?;
    }

    if let Some(media_ids) = media_ids {
        for media_id in media_ids {
            let Some(media) = local_media_attachment::Entity::find_by_id(*media_id)
                .one(txn)
                .await?
            else {
                return Err(RoostyError::InvalidInput(
                    "media attachment not found".to_owned(),
                ));
            };
            let available = media
                .status_id
                .is_none_or(|existing| existing == status_id.0);
            if media.account_id != account_id.0 || !available {
                return Err(RoostyError::InvalidInput(
                    "media attachment is not available".to_owned(),
                ));
            }
        }

        let keep = media_ids.to_vec();
        let current = local_media_attachment::Entity::find()
            .filter(local_media_attachment::Column::StatusId.eq(status_id.0))
            .all(txn)
            .await?;
        for media in current {
            if !keep.contains(&media.id) {
                let mut active = media.into_active_model();
                active.status_id = Set(None);
                active.status_order = Set(0);
                active.updated_at = Set(OffsetDateTime::now_utc());
                active.update(txn).await?;
            }
        }

        for (index, media_id) in media_ids.iter().enumerate() {
            let Some(media) = local_media_attachment::Entity::find_by_id(*media_id)
                .one(txn)
                .await?
            else {
                return Err(RoostyError::InvalidInput(
                    "media attachment not found".to_owned(),
                ));
            };
            let mut active = media.into_active_model();
            active.status_id = Set(Some(status_id.0));
            active.status_order = Set(index as i32);
            active.updated_at = Set(OffsetDateTime::now_utc());
            active.update(txn).await?;
        }
    }

    for attribute in media_attributes {
        let Some(media) = local_media_attachment::Entity::find_by_id(attribute.media_id)
            .filter(local_media_attachment::Column::AccountId.eq(account_id.0))
            .filter(local_media_attachment::Column::StatusId.eq(status_id.0))
            .one(txn)
            .await?
        else {
            return Err(RoostyError::InvalidInput(
                "media attachment is not available".to_owned(),
            ));
        };
        let mut active = media.into_active_model();
        if let Some(description) = &attribute.description {
            active.description = Set(description.clone());
        }
        if let Some((focus_x, focus_y)) = attribute.focus {
            active.focus_x = Set(Some(focus_x));
            active.focus_y = Set(Some(focus_y));
        }
        active.updated_at = Set(OffsetDateTime::now_utc());
        active.update(txn).await?;
    }

    let mut active = status.into_active_model();
    set_if_some(&mut active.content, update.content);
    set_if_some(&mut active.sensitive, update.sensitive);
    set_if_some(&mut active.spoiler_text, update.spoiler_text);
    set_if_some(&mut active.language, update.language);
    let revision_timestamp = OffsetDateTime::now_utc();
    active.updated_at = Set(revision_timestamp);
    let status = active.update(txn).await?;
    replace_local_poll(txn, status_id, poll.as_ref()).await?;
    mark_trend_dirty(txn, "local_status", status_id.0).await?;
    Box::pin(replace_status_preview_card(
        txn,
        PreviewStatusTarget::Local(status_id),
        &status.content,
        utc_date(status.created_at),
        PreviewActorOrigin::Local,
        status.account_id,
        false,
    ))
    .await?;
    refresh_status_search_document(txn, StatusReference::Local(status_id)).await?;

    if status.visibility == StatusVisibility::Public {
        adjust_tag_usage(
            txn,
            &current_tag_ids,
            utc_date(status.created_at),
            "local",
            status.account_id,
            -1,
        )
        .await?;
    }
    local_status_tag::Entity::delete_many()
        .filter(local_status_tag::Column::StatusId.eq(status_id.0))
        .exec(txn)
        .await?;
    let LocalStatusMetadata {
        scheduled_status_id: _,
        mut tag_names,
        remote_actor_ids,
        local_recipient_ids,
        local_mention_ids,
    } = metadata;
    tag_names.sort();
    tag_names.dedup();
    let now = OffsetDateTime::now_utc();
    let mut new_tag_ids = Vec::with_capacity(tag_names.len());
    for name in tag_names {
        let tag = find_or_create_local_tag(txn, &name, now).await?;
        new_tag_ids.push(tag.id);
        local_status_tag::ActiveModel {
            status_id: Set(status_id.0),
            tag_id: Set(tag.id),
            created_at: Set(now),
        }
        .insert(txn)
        .await?;
    }
    if status.visibility == StatusVisibility::Public {
        adjust_tag_usage(
            txn,
            &new_tag_ids,
            utc_date(status.created_at),
            "local",
            status.account_id,
            1,
        )
        .await?;
    }
    local_status_remote_mention::Entity::delete_many()
        .filter(local_status_remote_mention::Column::StatusId.eq(status_id.0))
        .exec(txn)
        .await?;
    let mut remote_actor_ids = remote_actor_ids
        .into_iter()
        .map(|id| id.0)
        .collect::<Vec<_>>();
    remote_actor_ids.sort();
    remote_actor_ids.dedup();
    for remote_actor_id in remote_actor_ids {
        local_status_remote_mention::ActiveModel {
            status_id: Set(status_id.0),
            remote_actor_id: Set(remote_actor_id),
            created_at: Set(now),
        }
        .insert(txn)
        .await?;
    }
    if status.visibility == StatusVisibility::Direct {
        local_status_local_recipient::Entity::delete_many()
            .filter(local_status_local_recipient::Column::StatusId.eq(status_id.0))
            .exec(txn)
            .await?;
        let mut recipient_ids = local_recipient_ids
            .into_iter()
            .map(|id| id.0)
            .collect::<Vec<_>>();
        recipient_ids.sort();
        recipient_ids.dedup();
        for account_id in recipient_ids {
            local_status_local_recipient::ActiveModel {
                status_id: Set(status_id.0),
                account_id: Set(account_id),
                created_at: Set(now),
            }
            .insert(txn)
            .await?;
        }
    }
    replace_local_status_local_mentions(txn, status_id, &local_mention_ids).await?;

    local_status_snapshot(txn, &status, revision_timestamp).await?;
    Ok(Some(LocalStatusUpdateResult::Updated(
        local_status_from_model(status)?,
    )))
}

/// Replace the active local mentions of a local status while retaining inactive history.
pub async fn replace_local_status_local_mentions(
    db: &impl ConnectionTrait,
    status_id: StatusId,
    account_ids: &[AccountId],
) -> Result<()> {
    let now = OffsetDateTime::now_utc();
    local_status_local_mention::Entity::update_many()
        .col_expr(local_status_local_mention::Column::Active, false.into())
        .col_expr(local_status_local_mention::Column::UpdatedAt, now.into())
        .filter(local_status_local_mention::Column::StatusId.eq(status_id.0))
        .exec(db)
        .await?;
    let mut account_ids = account_ids.iter().map(|id| id.0).collect::<Vec<_>>();
    account_ids.sort();
    account_ids.dedup();
    for account_id in account_ids {
        local_status_local_mention::Entity::insert(local_status_local_mention::ActiveModel {
            status_id: Set(status_id.0),
            account_id: Set(account_id),
            active: Set(true),
            created_at: Set(now),
            updated_at: Set(now),
        })
        .on_conflict(
            OnConflict::columns([
                local_status_local_mention::Column::StatusId,
                local_status_local_mention::Column::AccountId,
            ])
            .update_columns([
                local_status_local_mention::Column::Active,
                local_status_local_mention::Column::UpdatedAt,
            ])
            .to_owned(),
        )
        .exec(db)
        .await?;
    }
    Ok(())
}

/// Return the accounts currently mentioned by a local status.
pub async fn active_local_mentions_for_local_status(
    db: &impl ConnectionTrait,
    status_id: StatusId,
) -> Result<Vec<AccountId>> {
    Ok(local_status_local_mention::Entity::find()
        .filter(local_status_local_mention::Column::StatusId.eq(status_id.0))
        .filter(local_status_local_mention::Column::Active.eq(true))
        .all(db)
        .await?
        .into_iter()
        .map(|mention| AccountId(mention.account_id))
        .collect())
}

/// Replace the active local mentions of a cached remote status.
pub async fn replace_remote_status_local_mentions(
    db: &impl ConnectionTrait,
    status_id: StatusId,
    account_ids: &[AccountId],
) -> Result<()> {
    let now = OffsetDateTime::now_utc();
    remote_status_local_mention::Entity::update_many()
        .col_expr(remote_status_local_mention::Column::Active, false.into())
        .col_expr(remote_status_local_mention::Column::UpdatedAt, now.into())
        .filter(remote_status_local_mention::Column::RemoteStatusId.eq(status_id.0))
        .exec(db)
        .await?;
    let mut account_ids = account_ids.iter().map(|id| id.0).collect::<Vec<_>>();
    account_ids.sort();
    account_ids.dedup();
    for account_id in account_ids {
        remote_status_local_mention::Entity::insert(remote_status_local_mention::ActiveModel {
            remote_status_id: Set(status_id.0),
            account_id: Set(account_id),
            active: Set(true),
            created_at: Set(now),
            updated_at: Set(now),
        })
        .on_conflict(
            OnConflict::columns([
                remote_status_local_mention::Column::RemoteStatusId,
                remote_status_local_mention::Column::AccountId,
            ])
            .update_columns([
                remote_status_local_mention::Column::Active,
                remote_status_local_mention::Column::UpdatedAt,
            ])
            .to_owned(),
        )
        .exec(db)
        .await?;
    }
    Ok(())
}

/// Return the accounts currently mentioned by a cached remote status.
pub async fn active_local_mentions_for_remote_status(
    db: &impl ConnectionTrait,
    status_id: StatusId,
) -> Result<Vec<AccountId>> {
    Ok(remote_status_local_mention::Entity::find()
        .filter(remote_status_local_mention::Column::RemoteStatusId.eq(status_id.0))
        .filter(remote_status_local_mention::Column::Active.eq(true))
        .all(db)
        .await?
        .into_iter()
        .map(|mention| AccountId(mention.account_id))
        .collect())
}

/// Create local media metadata after the uploaded file has been stored.
pub async fn create_local_media_attachment(
    db: &impl ConnectionTrait,
    media: NewLocalMediaAttachment,
) -> Result<LocalMediaAttachment> {
    let now = OffsetDateTime::now_utc();
    let model = local_media_attachment::ActiveModel {
        id: Set(Uuid::now_v7()),
        account_id: Set(media.account_id.0),
        status_id: Set(None),
        scheduled_status_id: Set(None),
        status_order: Set(0),
        content_type: Set(media.content_type),
        original_filename: Set(media.original_filename),
        file_path: Set(media.file_path),
        preview_file_path: Set(media.preview_file_path),
        file_size: Set(media.file_size),
        description: Set(media.description),
        focus_x: Set(media.focus_x),
        focus_y: Set(media.focus_y),
        width: Set(media.width),
        height: Set(media.height),
        preview_width: Set(media.preview_width),
        preview_height: Set(media.preview_height),
        blurhash: Set(media.blurhash),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(db)
    .await?;

    Ok(local_media_attachment_from_model(model))
}

/// Store a schedule, reserve its media, and enqueue publication atomically.
pub async fn create_scheduled_status(
    db: &impl ConnectionTrait,
    input: NewScheduledStatus,
    total_limit: u64,
    daily_limit: u64,
) -> Result<(ScheduleStatusResult, Option<ScheduledStatus>)> {
    local_account::Entity::find_by_id(input.account_id.0)
        .lock_exclusive()
        .one(db)
        .await?
        .ok_or_else(|| RoostyError::InvalidInput("account does not exist".to_owned()))?;

    let total = scheduled_status::Entity::find()
        .filter(scheduled_status::Column::AccountId.eq(input.account_id.0))
        .count(db)
        .await?;
    if total >= total_limit {
        return Ok((ScheduleStatusResult::TotalLimitReached, None));
    }
    let day = input.scheduled_at.date();
    let day_start = day.midnight().assume_utc();
    let day_end = day_start + Duration::days(1);
    let daily = scheduled_status::Entity::find()
        .filter(scheduled_status::Column::AccountId.eq(input.account_id.0))
        .filter(scheduled_status::Column::ScheduledAt.gte(day_start))
        .filter(scheduled_status::Column::ScheduledAt.lt(day_end))
        .count(db)
        .await?;
    if daily >= daily_limit {
        return Ok((ScheduleStatusResult::DailyLimitReached, None));
    }

    for media_id in &input.media_ids {
        let available = local_media_attachment::Entity::find_by_id(*media_id)
            .filter(local_media_attachment::Column::AccountId.eq(input.account_id.0))
            .filter(local_media_attachment::Column::StatusId.is_null())
            .filter(local_media_attachment::Column::ScheduledStatusId.is_null())
            .lock_exclusive()
            .one(db)
            .await?;
        if available.is_none() {
            return Err(RoostyError::InvalidInput(
                "media attachment is not available".to_owned(),
            ));
        }
    }

    let id = input.id.unwrap_or_else(Uuid::now_v7);
    let publication_status_id = Uuid::now_v7();
    let now = OffsetDateTime::now_utc();
    let model = scheduled_status::ActiveModel {
        id: Set(id),
        account_id: Set(input.account_id.0),
        publication_status_id: Set(publication_status_id),
        content: Set(input.content),
        visibility: Set(input.visibility),
        sensitive: Set(input.sensitive),
        spoiler_text: Set(input.spoiler_text),
        language: Set(input.language),
        in_reply_to_id: Set(input.in_reply_to_id.map(|id| id.0)),
        in_reply_to_remote_status_id: Set(input.in_reply_to_remote_status_id.map(|id| id.0)),
        quoted_status_id: Set(input.quoted_status_id.map(|id| id.0)),
        quote_approval_policy: Set(input.quote_approval_policy),
        scheduled_at: Set(input.scheduled_at),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(db)
    .await?;
    if let Some(poll) = &input.poll {
        create_scheduled_poll(db, id, poll).await?;
    }
    for (position, media_id) in input.media_ids.iter().enumerate() {
        let media = local_media_attachment::Entity::find_by_id(*media_id)
            .lock_exclusive()
            .one(db)
            .await?
            .ok_or_else(|| RoostyError::InvalidInput("media attachment disappeared".to_owned()))?;
        let mut active = media.into_active_model();
        active.scheduled_status_id = Set(Some(id));
        active.status_order = Set(position as i32);
        active.updated_at = Set(now);
        active.update(db).await?;
    }
    enqueue_job_in_transaction(
        db,
        NewJob {
            kind: JobKind::ScheduledStatusPublish,
            payload: serde_json::json!({ "scheduled_status_id": id }),
            deduplication_key: Some(id.to_string()),
            run_after: input.scheduled_at,
        },
    )
    .await?;
    Ok((
        ScheduleStatusResult::Created,
        Some(scheduled_status_from_model(model)),
    ))
}

/// Find one schedule when it belongs to the requesting account.
pub async fn find_scheduled_status(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    id: Uuid,
) -> Result<Option<ScheduledStatus>> {
    Ok(scheduled_status::Entity::find_by_id(id)
        .filter(scheduled_status::Column::AccountId.eq(account_id.0))
        .one(db)
        .await?
        .map(scheduled_status_from_model))
}

/// Find a schedule by identifier for worker publication.
pub async fn find_scheduled_status_by_id(
    db: &impl ConnectionTrait,
    id: Uuid,
) -> Result<Option<ScheduledStatus>> {
    Ok(scheduled_status::Entity::find_by_id(id)
        .one(db)
        .await?
        .map(scheduled_status_from_model))
}

/// List one account's schedules in opaque UUIDv7 cursor order.
pub async fn scheduled_statuses(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    limit: u64,
    cursor: TimelineCursor,
) -> Result<TimelinePage<ScheduledStatus>> {
    let mut query = scheduled_status::Entity::find()
        .filter(scheduled_status::Column::AccountId.eq(account_id.0));
    if let Some(max_id) = cursor.max_id {
        query = query.filter(scheduled_status::Column::Id.lt(max_id.0));
    }
    if let Some(since_id) = cursor.since_id {
        query = query.filter(scheduled_status::Column::Id.gt(since_id.0));
    }
    if let Some(min_id) = cursor.min_id {
        query = query.filter(scheduled_status::Column::Id.gt(min_id.0));
    }
    let mut items = query
        .order_by_desc(scheduled_status::Column::Id)
        .limit(page_query_limit(limit))
        .all(db)
        .await?
        .into_iter()
        .map(scheduled_status_from_model)
        .collect::<Vec<_>>();
    if cursor.min_id.is_some() {
        items.sort_by_key(|status| Reverse(status.id));
    }
    let (items, has_more) = trim_to_page(items, limit);
    Ok(TimelinePage {
        first_cursor: items.first().map(|status| status.id),
        last_cursor: items.last().map(|status| status.id),
        items,
        has_more,
    })
}

/// Move a schedule and its active durable job under an account lock.
pub async fn reschedule_status(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    id: Uuid,
    scheduled_at: OffsetDateTime,
    daily_limit: u64,
) -> Result<Option<ScheduledStatus>> {
    local_account::Entity::find_by_id(account_id.0)
        .lock_exclusive()
        .one(db)
        .await?;
    let Some(model) = scheduled_status::Entity::find_by_id(id)
        .filter(scheduled_status::Column::AccountId.eq(account_id.0))
        .lock_exclusive()
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    let day = scheduled_at.date();
    let day_start = day.midnight().assume_utc();
    let day_end = day_start + Duration::days(1);
    let daily = scheduled_status::Entity::find()
        .filter(scheduled_status::Column::AccountId.eq(account_id.0))
        .filter(scheduled_status::Column::Id.ne(id))
        .filter(scheduled_status::Column::ScheduledAt.gte(day_start))
        .filter(scheduled_status::Column::ScheduledAt.lt(day_end))
        .count(db)
        .await?;
    if daily >= daily_limit {
        return Err(RoostyError::InvalidInput(
            "daily scheduled status limit reached".to_owned(),
        ));
    }
    let mut active = model.into_active_model();
    active.scheduled_at = Set(scheduled_at);
    active.updated_at = Set(OffsetDateTime::now_utc());
    let updated = active.update(db).await?;
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "UPDATE job SET run_after = $1 WHERE kind = $2 AND deduplication_key = $3 AND completed_at IS NULL",
        vec![
            scheduled_at.into(),
            JobKind::ScheduledStatusPublish.as_str().to_owned().into(),
            id.to_string().into(),
        ],
    ))
    .await?;
    Ok(Some(scheduled_status_from_model(updated)))
}

/// Cancel an owned schedule, release media, and retire its active job.
pub async fn cancel_scheduled_status(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    id: Uuid,
) -> Result<bool> {
    let Some(model) = scheduled_status::Entity::find_by_id(id)
        .filter(scheduled_status::Column::AccountId.eq(account_id.0))
        .lock_exclusive()
        .one(db)
        .await?
    else {
        return Ok(false);
    };
    model.into_active_model().delete(db).await?;
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "UPDATE job SET completed_at = now(), locked_at = NULL, locked_by = NULL, claim_id = NULL WHERE kind = $1 AND deduplication_key = $2 AND completed_at IS NULL",
        vec![
            JobKind::ScheduledStatusPublish.as_str().to_owned().into(),
            id.to_string().into(),
        ],
    ))
    .await?;
    Ok(true)
}

/// Remove a schedule as part of the transaction that published its status.
pub async fn delete_scheduled_status_in_transaction(
    txn: &DatabaseTransaction,
    id: Uuid,
) -> Result<()> {
    scheduled_status::Entity::delete_by_id(id).exec(txn).await?;
    Ok(())
}

/// Return reserved media in client order.
pub async fn media_attachments_for_scheduled_status(
    db: &impl ConnectionTrait,
    id: Uuid,
) -> Result<Vec<LocalMediaAttachment>> {
    Ok(local_media_attachment::Entity::find()
        .filter(local_media_attachment::Column::ScheduledStatusId.eq(id))
        .order_by_asc(local_media_attachment::Column::StatusOrder)
        .all(db)
        .await?
        .into_iter()
        .map(local_media_attachment_from_model)
        .collect())
}

/// Find a media attachment owned by a local account.
pub async fn find_owned_media_attachment(
    db: &DbConnection,
    account_id: AccountId,
    media_id: Uuid,
) -> Result<Option<LocalMediaAttachment>> {
    let media = local_media_attachment::Entity::find_by_id(media_id)
        .filter(local_media_attachment::Column::AccountId.eq(account_id.0))
        .one(db)
        .await?;

    Ok(media.map(local_media_attachment_from_model))
}

/// Find an unattached media attachment owned by a local account.
pub async fn find_owned_unattached_media_attachment(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    media_id: Uuid,
) -> Result<Option<LocalMediaAttachment>> {
    let media = local_media_attachment::Entity::find_by_id(media_id)
        .filter(local_media_attachment::Column::AccountId.eq(account_id.0))
        .filter(local_media_attachment::Column::StatusId.is_null())
        .filter(local_media_attachment::Column::ScheduledStatusId.is_null())
        .one(db)
        .await?;

    Ok(media.map(local_media_attachment_from_model))
}

/// Update mutable fields on an unattached media attachment owned by a local account.
pub async fn update_owned_unattached_media_attachment(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    media_id: Uuid,
    update: LocalMediaAttachmentUpdate,
) -> Result<Option<LocalMediaAttachment>> {
    let Some(media) = local_media_attachment::Entity::find_by_id(media_id)
        .filter(local_media_attachment::Column::AccountId.eq(account_id.0))
        .filter(local_media_attachment::Column::StatusId.is_null())
        .filter(local_media_attachment::Column::ScheduledStatusId.is_null())
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    let mut active = media.into_active_model();
    if let Some(description) = update.description {
        active.description = Set(description);
    }
    if let Some((focus_x, focus_y)) = update.focus {
        active.focus_x = Set(Some(focus_x));
        active.focus_y = Set(Some(focus_y));
    }
    if let Some(preview) = update.preview {
        active.preview_file_path = Set(Some(preview.preview_file_path));
        active.preview_width = Set(Some(preview.preview_width));
        active.preview_height = Set(Some(preview.preview_height));
        active.blurhash = Set(Some(preview.blurhash));
    }
    active.updated_at = Set(OffsetDateTime::now_utc());

    Ok(Some(local_media_attachment_from_model(
        active.update(db).await?,
    )))
}

/// Delete an unattached media attachment owned by a local account.
pub async fn delete_owned_unattached_media_attachment(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    media_id: Uuid,
) -> Result<Option<LocalMediaAttachment>> {
    let Some(media) = local_media_attachment::Entity::find_by_id(media_id)
        .filter(local_media_attachment::Column::AccountId.eq(account_id.0))
        .filter(local_media_attachment::Column::StatusId.is_null())
        .filter(local_media_attachment::Column::ScheduledStatusId.is_null())
        .filter(
            local_media_attachment::Column::Id.not_in_subquery(
                Query::select()
                    .column(local_status_edit_media::Column::LocalMediaAttachmentId)
                    .from(local_status_edit_media::Entity)
                    .to_owned(),
            ),
        )
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    let value = local_media_attachment_from_model(media.clone());
    media.into_active_model().delete(db).await?;

    Ok(Some(value))
}

/// List media attachments for a local status in client-supplied order.
pub async fn local_media_attachments_for_status(
    db: &impl ConnectionTrait,
    status_id: StatusId,
) -> Result<Vec<LocalMediaAttachment>> {
    let media = local_media_attachment::Entity::find()
        .filter(local_media_attachment::Column::StatusId.eq(status_id.0))
        .order_by_asc(local_media_attachment::Column::StatusOrder)
        .all(db)
        .await?;

    Ok(media
        .into_iter()
        .map(local_media_attachment_from_model)
        .collect())
}

/// Load every stored local revision oldest first.
pub async fn local_status_edits(
    db: &impl ConnectionTrait,
    status_id: StatusId,
) -> Result<Vec<LocalStatusEdit>> {
    let edits = local_status_edit::Entity::find()
        .filter(local_status_edit::Column::LocalStatusId.eq(status_id.0))
        .order_by_asc(local_status_edit::Column::CreatedAt)
        .order_by_asc(local_status_edit::Column::Id)
        .all(db)
        .await?;
    let mut result = Vec::with_capacity(edits.len());
    for edit in edits {
        let media = local_status_edit_media::Entity::find()
            .filter(local_status_edit_media::Column::LocalStatusEditId.eq(edit.id))
            .order_by_asc(local_status_edit_media::Column::StatusOrder)
            .all(db)
            .await?
            .into_iter()
            .map(|item| StatusEditMedia {
                id: item.local_media_attachment_id,
                content_type: Some(item.content_type),
                file_path: Some(item.file_path),
                preview_file_path: item.preview_file_path,
                remote_url: None,
                description: item.description,
                focus_x: item.focus_x,
                focus_y: item.focus_y,
                width: item.width,
                height: item.height,
                preview_width: item.preview_width,
                preview_height: item.preview_height,
                blurhash: item.blurhash,
            })
            .collect();
        let local_ids = serde_json::from_value::<Vec<Uuid>>(edit.local_mention_ids)
            .map_err(|error| RoostyError::InvalidInput(error.to_string()))?;
        let remote_ids = serde_json::from_value::<Vec<Uuid>>(edit.remote_mention_ids)
            .map_err(|error| RoostyError::InvalidInput(error.to_string()))?;
        result.push(LocalStatusEdit {
            content: edit.content,
            spoiler_text: edit.spoiler_text,
            sensitive: edit.sensitive,
            local_mention_ids: local_ids.into_iter().map(AccountId).collect(),
            remote_mention_ids: remote_ids.into_iter().map(AccountId).collect(),
            tag_names: serde_json::from_value(edit.tag_names)
                .map_err(|error| RoostyError::InvalidInput(error.to_string()))?,
            poll_options: edit
                .poll_options
                .map(serde_json::from_value)
                .transpose()
                .map_err(|error| RoostyError::InvalidInput(error.to_string()))?,
            created_at: edit.created_at,
            media,
        });
    }
    Ok(result)
}

/// Load every stored cached-remote revision oldest first.
pub async fn remote_status_edits(
    db: &impl ConnectionTrait,
    status_id: StatusId,
) -> Result<Vec<RemoteStatusEdit>> {
    let edits = remote_status_edit::Entity::find()
        .filter(remote_status_edit::Column::RemoteStatusId.eq(status_id.0))
        .order_by_asc(remote_status_edit::Column::CreatedAt)
        .order_by_asc(remote_status_edit::Column::Id)
        .all(db)
        .await?;
    let mut result = Vec::with_capacity(edits.len());
    for edit in edits {
        let media = remote_status_edit_media::Entity::find()
            .filter(remote_status_edit_media::Column::RemoteStatusEditId.eq(edit.id))
            .order_by_asc(remote_status_edit_media::Column::StatusOrder)
            .all(db)
            .await?
            .into_iter()
            .map(|item| StatusEditMedia {
                id: item.source_attachment_id.unwrap_or(item.id),
                content_type: item.content_type,
                file_path: item.file_path,
                preview_file_path: item.preview_file_path,
                remote_url: Some(item.remote_url),
                description: item.description,
                focus_x: None,
                focus_y: None,
                width: item.width,
                height: item.height,
                preview_width: item.preview_width,
                preview_height: item.preview_height,
                blurhash: item.blurhash,
            })
            .collect();
        result.push(RemoteStatusEdit {
            content: edit.content,
            spoiler_text: edit.spoiler_text,
            sensitive: edit.sensitive,
            object: edit.object,
            poll_options: edit
                .poll_options
                .map(serde_json::from_value)
                .transpose()
                .map_err(|error| RoostyError::InvalidInput(error.to_string()))?,
            created_at: edit.created_at,
            media,
        });
    }
    Ok(result)
}

/// Return whether a local status has at least one media attachment.
pub async fn local_status_has_media(
    db: &impl ConnectionTrait,
    status_id: StatusId,
) -> Result<bool> {
    Ok(local_media_attachment::Entity::find()
        .filter(local_media_attachment::Column::StatusId.eq(status_id.0))
        .count(db)
        .await?
        > 0)
}

/// Find a local status by id, excluding soft-deleted statuses.
pub async fn find_local_status_by_id(
    db: &impl ConnectionTrait,
    status_id: StatusId,
) -> Result<Option<LocalStatus>> {
    let status = local_status::Entity::find_by_id(status_id.0)
        .filter(local_status::Column::DeletedAt.is_null())
        .one(db)
        .await?;

    status.map(local_status_from_model).transpose()
}

/// Pin an owned public or unlisted status while enforcing a cross-process account limit.
pub async fn pin_local_status(
    txn: &DatabaseTransaction,
    status_id: StatusId,
    account_id: AccountId,
    limit: u64,
) -> Result<PinStatusResult> {
    txn.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        vec![format!("local-status-pins:{}", account_id.0).into()],
    ))
    .await?;
    let Some(status) = local_status::Entity::find_by_id(status_id.0)
        .filter(local_status::Column::DeletedAt.is_null())
        .lock_exclusive()
        .one(txn)
        .await?
    else {
        return Ok(PinStatusResult::NotFound);
    };
    if status.account_id != account_id.0 {
        return Ok(PinStatusResult::NotOwned);
    }
    if !matches!(
        status.visibility,
        StatusVisibility::Public | StatusVisibility::Unlisted
    ) {
        return Ok(PinStatusResult::UnsupportedVisibility);
    }
    if local_status_pin::Entity::find()
        .filter(local_status_pin::Column::StatusId.eq(status_id.0))
        .one(txn)
        .await?
        .is_some()
    {
        return Ok(PinStatusResult::AlreadyPinned);
    }
    if local_status_pin::Entity::find()
        .filter(local_status_pin::Column::AccountId.eq(account_id.0))
        .count(txn)
        .await?
        >= limit
    {
        return Ok(PinStatusResult::LimitReached);
    }
    local_status_pin::ActiveModel {
        id: Set(Uuid::now_v7()),
        account_id: Set(account_id.0),
        status_id: Set(status_id.0),
        ..Default::default()
    }
    .insert(txn)
    .await?;
    Ok(PinStatusResult::Pinned)
}

/// Remove an owned local status pin, returning whether one existed.
pub async fn unpin_local_status(
    txn: &DatabaseTransaction,
    status_id: StatusId,
    account_id: AccountId,
) -> Result<PinStatusResult> {
    txn.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        vec![format!("local-status-pins:{}", account_id.0).into()],
    ))
    .await?;
    let Some(status) = local_status::Entity::find_by_id(status_id.0)
        .filter(local_status::Column::DeletedAt.is_null())
        .one(txn)
        .await?
    else {
        return Ok(PinStatusResult::NotFound);
    };
    if status.account_id != account_id.0 {
        return Ok(PinStatusResult::NotOwned);
    }
    let result = local_status_pin::Entity::delete_many()
        .filter(local_status_pin::Column::StatusId.eq(status_id.0))
        .filter(local_status_pin::Column::AccountId.eq(account_id.0))
        .exec(txn)
        .await?;
    Ok(if result.rows_affected == 0 {
        PinStatusResult::AlreadyUnpinned
    } else {
        PinStatusResult::Unpinned
    })
}

/// Return whether one local status is currently pinned by its author.
pub async fn is_local_status_pinned(
    db: &impl ConnectionTrait,
    status_id: StatusId,
) -> Result<bool> {
    Ok(local_status_pin::Entity::find()
        .filter(local_status_pin::Column::StatusId.eq(status_id.0))
        .one(db)
        .await?
        .is_some())
}

/// Return whether one cached remote status is in its author's featured collection.
pub async fn is_remote_status_pinned(
    db: &impl ConnectionTrait,
    status_id: StatusId,
) -> Result<bool> {
    Ok(remote_status_pin::Entity::find()
        .filter(remote_status_pin::Column::RemoteStatusId.eq(status_id.0))
        .one(db)
        .await?
        .is_some())
}

/// Fetch the pinned subset of a local status batch in one query.
pub async fn pinned_local_status_ids(
    db: &impl ConnectionTrait,
    status_ids: &[StatusId],
) -> Result<HashSet<StatusId>> {
    if status_ids.is_empty() {
        return Ok(HashSet::new());
    }
    Ok(local_status_pin::Entity::find()
        .filter(
            local_status_pin::Column::StatusId
                .is_in(status_ids.iter().map(|id| id.0).collect::<Vec<_>>()),
        )
        .all(db)
        .await?
        .into_iter()
        .map(|pin| StatusId(pin.status_id))
        .collect())
}

/// Fetch the pinned subset of a cached remote status batch in one query.
pub async fn pinned_remote_status_ids(
    db: &impl ConnectionTrait,
    status_ids: &[StatusId],
) -> Result<HashSet<StatusId>> {
    if status_ids.is_empty() {
        return Ok(HashSet::new());
    }
    Ok(remote_status_pin::Entity::find()
        .filter(
            remote_status_pin::Column::RemoteStatusId
                .is_in(status_ids.iter().map(|id| id.0).collect::<Vec<_>>()),
        )
        .all(db)
        .await?
        .into_iter()
        .map(|pin| StatusId(pin.remote_status_id))
        .collect())
}

/// List local pins newest-first using pin identities as Mastodon cursors.
pub async fn pinned_local_statuses_by_account(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    limit: u64,
    cursor: TimelineCursor,
) -> Result<TimelinePage<LocalStatus>> {
    let mut query = local_status_pin::Entity::find()
        .filter(local_status_pin::Column::AccountId.eq(account_id.0));
    if let Some(max_id) = cursor.max_id {
        query = query.filter(local_status_pin::Column::Id.lt(max_id.0));
    }
    if let Some(since_id) = cursor.since_id {
        query = query.filter(local_status_pin::Column::Id.gt(since_id.0));
    }
    if let Some(min_id) = cursor.min_id {
        query = query.filter(local_status_pin::Column::Id.gt(min_id.0));
    }
    let pins = query
        .order_by_desc(local_status_pin::Column::PinnedAt)
        .order_by_desc(local_status_pin::Column::Id)
        .limit(page_query_limit(limit))
        .all(db)
        .await?;
    let (pins, has_more) = trim_to_page(pins, limit);
    let first_cursor = pins.first().map(|pin| pin.id);
    let last_cursor = pins.last().map(|pin| pin.id);
    let status_ids = pins.iter().map(|pin| pin.status_id).collect::<Vec<_>>();
    let statuses = local_status::Entity::find()
        .filter(local_status::Column::Id.is_in(status_ids))
        .filter(local_status::Column::DeletedAt.is_null())
        .all(db)
        .await?
        .into_iter()
        .map(|status| local_status_from_model(status).map(|status| (status.id.0, status)))
        .collect::<Result<HashMap<_, _>>>()?;
    let items = pins
        .into_iter()
        .filter_map(|pin| statuses.get(&pin.status_id).cloned())
        .collect();
    Ok(TimelinePage {
        items,
        first_cursor,
        last_cursor,
        has_more,
    })
}

/// List cached remote pins newest-first using cache-row identities as cursors.
pub async fn pinned_remote_statuses_by_account(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    limit: u64,
    cursor: TimelineCursor,
) -> Result<TimelinePage<RemoteStatus>> {
    let mut query = remote_status_pin::Entity::find()
        .filter(remote_status_pin::Column::RemoteActorId.eq(account_id.0));
    if let Some(max_id) = cursor.max_id {
        query = query.filter(remote_status_pin::Column::Id.lt(max_id.0));
    }
    if let Some(since_id) = cursor.since_id {
        query = query.filter(remote_status_pin::Column::Id.gt(since_id.0));
    }
    if let Some(min_id) = cursor.min_id {
        query = query.filter(remote_status_pin::Column::Id.gt(min_id.0));
    }
    let pins = query
        .order_by_desc(remote_status_pin::Column::PinnedAt)
        .order_by_desc(remote_status_pin::Column::Id)
        .limit(page_query_limit(limit))
        .all(db)
        .await?;
    let (pins, has_more) = trim_to_page(pins, limit);
    let first_cursor = pins.first().map(|pin| pin.id);
    let last_cursor = pins.last().map(|pin| pin.id);
    let status_ids = pins
        .iter()
        .map(|pin| pin.remote_status_id)
        .collect::<Vec<_>>();
    let statuses = remote_status::Entity::find()
        .filter(remote_status::Column::Id.is_in(status_ids))
        .filter(remote_status::Column::DeletedAt.is_null())
        .all(db)
        .await?
        .into_iter()
        .map(|status| remote_status_from_model(status).map(|status| (status.id.0, status)))
        .collect::<Result<HashMap<_, _>>>()?;
    let items = pins
        .into_iter()
        .filter_map(|pin| statuses.get(&pin.remote_status_id).cloned())
        .collect();
    Ok(TimelinePage {
        items,
        first_cursor,
        last_cursor,
        has_more,
    })
}

/// Atomically replace one actor's featured cache under its cross-process advisory lock.
pub async fn replace_remote_status_pins(
    txn: &DatabaseTransaction,
    actor_id: AccountId,
    status_ids: &[StatusId],
) -> Result<()> {
    txn.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        vec![format!("remote-featured:{}", actor_id.0).into()],
    ))
    .await?;
    remote_status_pin::Entity::delete_many()
        .filter(remote_status_pin::Column::RemoteActorId.eq(actor_id.0))
        .exec(txn)
        .await?;
    let now = OffsetDateTime::now_utc();
    for (position, status_id) in status_ids.iter().enumerate() {
        remote_status_pin::ActiveModel {
            id: Set(Uuid::now_v7()),
            remote_actor_id: Set(actor_id.0),
            remote_status_id: Set(status_id.0),
            pinned_at: Set(now - Duration::microseconds(position as i64)),
        }
        .insert(txn)
        .await?;
    }
    Ok(())
}

/// Apply one signed featured Add or Remove under the refresh reconciliation lock.
pub async fn apply_remote_status_pin_activity(
    txn: &DatabaseTransaction,
    actor_id: AccountId,
    status_id: StatusId,
    pin: bool,
) -> Result<()> {
    txn.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        vec![format!("remote-featured:{}", actor_id.0).into()],
    ))
    .await?;
    if pin {
        remote_status_pin::Entity::insert(remote_status_pin::ActiveModel {
            id: Set(Uuid::now_v7()),
            remote_actor_id: Set(actor_id.0),
            remote_status_id: Set(status_id.0),
            ..Default::default()
        })
        .on_conflict(
            OnConflict::column(remote_status_pin::Column::RemoteStatusId)
                .do_nothing()
                .to_owned(),
        )
        .do_nothing()
        .exec(txn)
        .await?;
        let pins = remote_status_pin::Entity::find()
            .filter(remote_status_pin::Column::RemoteActorId.eq(actor_id.0))
            .order_by_desc(remote_status_pin::Column::PinnedAt)
            .order_by_desc(remote_status_pin::Column::Id)
            .all(txn)
            .await?;
        let stale = pins
            .into_iter()
            .skip(20)
            .map(|pin| pin.id)
            .collect::<Vec<_>>();
        if !stale.is_empty() {
            remote_status_pin::Entity::delete_many()
                .filter(remote_status_pin::Column::Id.is_in(stale))
                .exec(txn)
                .await?;
        }
    } else {
        remote_status_pin::Entity::delete_many()
            .filter(remote_status_pin::Column::RemoteActorId.eq(actor_id.0))
            .filter(remote_status_pin::Column::RemoteStatusId.eq(status_id.0))
            .exec(txn)
            .await?;
    }
    Ok(())
}

/// Attach one immutable quote target to a newly-created local status.
pub async fn create_local_status_quote(
    txn: &DatabaseTransaction,
    quoting_status_id: StatusId,
    quoted_status: StatusReference,
    quoted_activitypub_id: &str,
    state: QuoteState,
    quote_request_id: Option<&str>,
    authorization_id: Option<&str>,
) -> Result<StatusQuote> {
    local_status::Entity::find_by_id(quoting_status_id.0)
        .lock_exclusive()
        .one(txn)
        .await?
        .ok_or_else(|| RoostyError::InvalidInput("quoting status does not exist".to_owned()))?;
    let (quoted_local_status_id, quoted_remote_status_id) = match quoted_status {
        StatusReference::Local(id) => (Some(id.0), None),
        StatusReference::Remote(id) => (None, Some(id.0)),
    };
    let model = status_quote::ActiveModel {
        id: Set(Uuid::now_v7()),
        local_quoting_status_id: Set(Some(quoting_status_id.0)),
        remote_quoting_status_id: Set(None),
        quoted_local_status_id: Set(quoted_local_status_id),
        quoted_remote_status_id: Set(quoted_remote_status_id),
        quoted_activitypub_id: Set(quoted_activitypub_id.to_owned()),
        state: Set(state),
        quote_request_id: Set(quote_request_id.map(ToOwned::to_owned)),
        authorization_id: Set(authorization_id.map(ToOwned::to_owned)),
        ..Default::default()
    }
    .insert(txn)
    .await?;
    status_quote_from_model(model)
}

/// Attach or refresh a quote discovered on a verified remote status.
pub async fn upsert_remote_status_quote(
    txn: &DatabaseTransaction,
    quoting_status_id: StatusId,
    quoted_status: StatusReference,
    quoted_activitypub_id: &str,
    state: QuoteState,
    authorization_id: Option<&str>,
) -> Result<StatusQuote> {
    remote_status::Entity::find_by_id(quoting_status_id.0)
        .lock_exclusive()
        .one(txn)
        .await?
        .ok_or_else(|| {
            RoostyError::InvalidInput("remote quoting status does not exist".to_owned())
        })?;
    if let Some(existing) = status_quote::Entity::find()
        .filter(status_quote::Column::RemoteQuotingStatusId.eq(quoting_status_id.0))
        .lock_exclusive()
        .one(txn)
        .await?
    {
        if existing.quoted_activitypub_id != quoted_activitypub_id {
            return Err(RoostyError::InvalidInput(
                "a remote quote target cannot change".to_owned(),
            ));
        }
        let mut active = existing.into_active_model();
        active.state = Set(state);
        active.authorization_id = Set(authorization_id.map(ToOwned::to_owned));
        active.updated_at = Set(OffsetDateTime::now_utc());
        return status_quote_from_model(active.update(txn).await?);
    }
    let (quoted_local_status_id, quoted_remote_status_id) = match quoted_status {
        StatusReference::Local(id) => (Some(id.0), None),
        StatusReference::Remote(id) => (None, Some(id.0)),
    };
    status_quote_from_model(
        status_quote::ActiveModel {
            id: Set(Uuid::now_v7()),
            local_quoting_status_id: Set(None),
            remote_quoting_status_id: Set(Some(quoting_status_id.0)),
            quoted_local_status_id: Set(quoted_local_status_id),
            quoted_remote_status_id: Set(quoted_remote_status_id),
            quoted_activitypub_id: Set(quoted_activitypub_id.to_owned()),
            state: Set(state),
            quote_request_id: Set(None),
            authorization_id: Set(authorization_id.map(ToOwned::to_owned)),
            ..Default::default()
        }
        .insert(txn)
        .await?,
    )
}

/// Create an idempotent quote notification caused by a verified remote actor.
pub async fn notify_remote_actor_quote(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    remote_actor_id: AccountId,
    remote_status_id: StatusId,
) -> Result<Option<LocalNotification>> {
    if !remote_account_allows_notification(db, account_id, remote_actor_id).await? {
        return Ok(None);
    }
    if let Some(existing) = local_notification::Entity::find()
        .filter(local_notification::Column::AccountId.eq(account_id.0))
        .filter(local_notification::Column::NotificationType.eq(LocalNotificationType::Quote))
        .filter(local_notification::Column::RemoteActorId.eq(Some(remote_actor_id.0)))
        .filter(local_notification::Column::RemoteStatusId.eq(Some(remote_status_id.0)))
        .one(db)
        .await?
    {
        return Ok((!existing.filtered).then(|| local_notification_from_model(existing)));
    }
    let action = remote_notification_policy_action(
        db,
        account_id,
        remote_actor_id,
        LocalNotificationType::Quote,
        Some(remote_status_id),
    )
    .await?;
    if action == NotificationPolicyAction::Drop {
        return Ok(None);
    }
    let request_id = if action == NotificationPolicyAction::Filter {
        Some(
            upsert_notification_request(
                db,
                account_id,
                NotificationActor::Remote(remote_actor_id),
                remote_status_id,
            )
            .await?,
        )
    } else {
        None
    };
    let model = local_notification::ActiveModel {
        id: Set(Uuid::now_v7()),
        account_id: Set(account_id.0),
        notification_type: Set(LocalNotificationType::Quote),
        actor_account_id: Set(None),
        remote_actor_id: Set(Some(remote_actor_id.0)),
        status_id: Set(None),
        remote_status_id: Set(Some(remote_status_id.0)),
        group_id: Set(None),
        filtered: Set(request_id.is_some()),
        notification_request_id: Set(request_id),
        report_id: Set(None),
        created_at: Set(OffsetDateTime::now_utc()),
        dismissed_at: Set(None),
    }
    .insert(db)
    .await?;
    Ok(request_id
        .is_none()
        .then(|| local_notification_from_model(model)))
}

/// Create a policy-aware quote notification caused by a local account.
pub async fn notify_local_status_quote(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    actor_account_id: AccountId,
    status_id: StatusId,
) -> Result<Option<LocalNotification>> {
    if account_id == actor_account_id
        || !local_account_allows_notification(db, account_id, actor_account_id).await?
    {
        return Ok(None);
    }
    if let Some(existing) = local_notification::Entity::find()
        .filter(local_notification::Column::AccountId.eq(account_id.0))
        .filter(local_notification::Column::NotificationType.eq(LocalNotificationType::Quote))
        .filter(local_notification::Column::ActorAccountId.eq(actor_account_id.0))
        .filter(local_notification::Column::StatusId.eq(status_id.0))
        .one(db)
        .await?
    {
        return Ok((!existing.filtered).then(|| local_notification_from_model(existing)));
    }
    let action = local_notification_policy_action(
        db,
        account_id,
        actor_account_id,
        LocalNotificationType::Quote,
        Some(status_id),
    )
    .await?;
    if action == NotificationPolicyAction::Drop {
        return Ok(None);
    }
    let request_id = if action == NotificationPolicyAction::Filter {
        Some(
            upsert_notification_request(
                db,
                account_id,
                NotificationActor::Local(actor_account_id),
                status_id,
            )
            .await?,
        )
    } else {
        None
    };
    let model = local_notification::ActiveModel {
        id: Set(Uuid::now_v7()),
        account_id: Set(account_id.0),
        notification_type: Set(LocalNotificationType::Quote),
        actor_account_id: Set(Some(actor_account_id.0)),
        remote_actor_id: Set(None),
        status_id: Set(Some(status_id.0)),
        remote_status_id: Set(None),
        group_id: Set(None),
        filtered: Set(request_id.is_some()),
        notification_request_id: Set(request_id),
        report_id: Set(None),
        created_at: Set(OffsetDateTime::now_utc()),
        dismissed_at: Set(None),
    }
    .insert(db)
    .await?;
    Ok(request_id
        .is_none()
        .then(|| local_notification_from_model(model)))
}

/// Load the quote attached to a local or cached-remote status.
pub async fn quote_for_status(
    db: &impl ConnectionTrait,
    status: StatusReference,
) -> Result<Option<StatusQuote>> {
    let query = status_quote::Entity::find();
    let query = match status {
        StatusReference::Local(id) => {
            query.filter(status_quote::Column::LocalQuotingStatusId.eq(id.0))
        }
        StatusReference::Remote(id) => {
            query.filter(status_quote::Column::RemoteQuotingStatusId.eq(id.0))
        }
    };
    query
        .one(db)
        .await?
        .map(status_quote_from_model)
        .transpose()
}

/// Resolve a pending local quote by its durable QuoteRequest identity.
pub async fn quote_by_request_id(
    db: &impl ConnectionTrait,
    request_id: &str,
) -> Result<Option<StatusQuote>> {
    status_quote::Entity::find()
        .filter(status_quote::Column::QuoteRequestId.eq(request_id))
        .one(db)
        .await?
        .map(status_quote_from_model)
        .transpose()
}

pub async fn quote_by_authorization_id(
    db: &impl ConnectionTrait,
    authorization_id: &str,
) -> Result<Option<StatusQuote>> {
    status_quote::Entity::find()
        .filter(status_quote::Column::AuthorizationId.eq(authorization_id))
        .one(db)
        .await?
        .map(status_quote_from_model)
        .transpose()
}

/// Apply an idempotent response to a pending QuoteRequest under a row lock.
pub async fn transition_quote_request(
    txn: &DatabaseTransaction,
    request_id: &str,
    state: QuoteState,
    authorization_id: Option<&str>,
) -> Result<Option<StatusQuote>> {
    let Some(model) = status_quote::Entity::find()
        .filter(status_quote::Column::QuoteRequestId.eq(request_id))
        .lock_exclusive()
        .one(txn)
        .await?
    else {
        return Ok(None);
    };
    if model.state != QuoteState::Pending {
        return status_quote_from_model(model).map(Some);
    }
    let mut active = model.into_active_model();
    active.state = Set(state);
    active.authorization_id = Set(authorization_id.map(ToOwned::to_owned));
    active.updated_at = Set(OffsetDateTime::now_utc());
    status_quote_from_model(active.update(txn).await?).map(Some)
}

pub async fn revoke_quote_authorization(
    txn: &DatabaseTransaction,
    authorization_id: &str,
) -> Result<Option<StatusQuote>> {
    let Some(model) = status_quote::Entity::find()
        .filter(status_quote::Column::AuthorizationId.eq(authorization_id))
        .lock_exclusive()
        .one(txn)
        .await?
    else {
        return Ok(None);
    };
    if model.state == QuoteState::Revoked {
        return status_quote_from_model(model).map(Some);
    }
    let mut active = model.into_active_model();
    active.state = Set(QuoteState::Revoked);
    active.updated_at = Set(OffsetDateTime::now_utc());
    status_quote_from_model(active.update(txn).await?).map(Some)
}

/// Count currently accepted quotes of a status.
pub async fn count_accepted_quotes(
    db: &impl ConnectionTrait,
    status: StatusReference,
) -> Result<u64> {
    let query =
        status_quote::Entity::find().filter(status_quote::Column::State.eq(QuoteState::Accepted));
    let query = match status {
        StatusReference::Local(id) => {
            query.filter(status_quote::Column::QuotedLocalStatusId.eq(id.0))
        }
        StatusReference::Remote(id) => {
            query.filter(status_quote::Column::QuotedRemoteStatusId.eq(id.0))
        }
    };
    Ok(query.count(db).await?)
}

/// List accepted quoting statuses newest-first using UUIDv7 cursors.
pub async fn accepted_quotes_for_status(
    db: &impl ConnectionTrait,
    status: StatusReference,
    max_id: Option<Uuid>,
    since_id: Option<Uuid>,
    limit: u64,
) -> Result<Vec<StatusQuote>> {
    let query =
        status_quote::Entity::find().filter(status_quote::Column::State.eq(QuoteState::Accepted));
    let mut query = match status {
        StatusReference::Local(id) => {
            query.filter(status_quote::Column::QuotedLocalStatusId.eq(id.0))
        }
        StatusReference::Remote(id) => {
            query.filter(status_quote::Column::QuotedRemoteStatusId.eq(id.0))
        }
    };
    if let Some(id) = max_id {
        query = query.filter(status_quote::Column::Id.lt(id));
    }
    if let Some(id) = since_id {
        query = query.filter(status_quote::Column::Id.gt(id));
    }
    query
        .order_by_desc(status_quote::Column::Id)
        .limit(limit)
        .all(db)
        .await?
        .into_iter()
        .map(status_quote_from_model)
        .collect()
}

/// Revoke an accepted quote while holding both affected rows for update.
pub async fn revoke_status_quote(
    txn: &DatabaseTransaction,
    quoted_status: StatusReference,
    quoting_status: StatusReference,
) -> Result<Option<StatusQuote>> {
    let query = status_quote::Entity::find();
    let query = match quoting_status {
        StatusReference::Local(id) => {
            query.filter(status_quote::Column::LocalQuotingStatusId.eq(id.0))
        }
        StatusReference::Remote(id) => {
            query.filter(status_quote::Column::RemoteQuotingStatusId.eq(id.0))
        }
    };
    let Some(model) = query.lock_exclusive().one(txn).await? else {
        return Ok(None);
    };
    let target_matches = match quoted_status {
        StatusReference::Local(id) => model.quoted_local_status_id == Some(id.0),
        StatusReference::Remote(id) => model.quoted_remote_status_id == Some(id.0),
    };
    if !target_matches {
        return Ok(None);
    }
    if model.state == QuoteState::Revoked {
        return status_quote_from_model(model).map(Some);
    }
    let mut active = model.into_active_model();
    active.state = Set(QuoteState::Revoked);
    active.updated_at = Set(OffsetDateTime::now_utc());
    status_quote_from_model(active.update(txn).await?).map(Some)
}

/// Mark every quote of a soft-deleted target unavailable inside the delete transaction.
pub async fn mark_quotes_target_deleted(
    txn: &DatabaseTransaction,
    target: StatusReference,
) -> Result<()> {
    let filter = match target {
        StatusReference::Local(id) => status_quote::Column::QuotedLocalStatusId.eq(id.0),
        StatusReference::Remote(id) => status_quote::Column::QuotedRemoteStatusId.eq(id.0),
    };
    status_quote::Entity::update_many()
        .col_expr(
            status_quote::Column::State,
            sea_orm::sea_query::Expr::value("deleted"),
        )
        .col_expr(
            status_quote::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(OffsetDateTime::now_utc()),
        )
        .filter(filter)
        .filter(status_quote::Column::State.eq(QuoteState::Accepted))
        .exec(txn)
        .await?;
    Ok(())
}

/// Notify local authors whose accepted quote references an edited local status.
pub async fn notify_local_quoted_status_update(
    txn: &DatabaseTransaction,
    edited_status: &LocalStatus,
) -> Result<Vec<LocalNotification>> {
    let quotes = status_quote::Entity::find()
        .filter(status_quote::Column::QuotedLocalStatusId.eq(edited_status.id.0))
        .filter(status_quote::Column::State.eq(QuoteState::Accepted))
        .all(txn)
        .await?;
    let mut notifications = Vec::new();
    for quote in quotes {
        let Some(quoting_id) = quote.local_quoting_status_id else {
            continue;
        };
        let Some(quoting) = local_status::Entity::find_by_id(quoting_id)
            .one(txn)
            .await?
        else {
            continue;
        };
        if quoting.account_id == edited_status.account_id.0 {
            continue;
        }
        notifications.push(
            notify_local_account(
                txn,
                AccountId(quoting.account_id),
                LocalNotificationType::QuotedUpdate,
                edited_status.account_id,
                Some(StatusId(quoting_id)),
            )
            .await?,
        );
    }
    Ok(notifications)
}

/// Change the policy on an owned status while serializing concurrent updates.
pub async fn update_local_status_quote_policy(
    txn: &DatabaseTransaction,
    status_id: StatusId,
    account_id: AccountId,
    policy: QuoteApprovalPolicy,
) -> Result<Option<LocalStatus>> {
    let Some(model) = local_status::Entity::find_by_id(status_id.0)
        .filter(local_status::Column::AccountId.eq(account_id.0))
        .filter(local_status::Column::DeletedAt.is_null())
        .lock_exclusive()
        .one(txn)
        .await?
    else {
        return Ok(None);
    };
    let effective = if matches!(
        model.visibility,
        StatusVisibility::Private | StatusVisibility::Direct
    ) {
        QuoteApprovalPolicy::Nobody
    } else {
        policy
    };
    if model.quote_approval_policy == effective {
        return local_status_from_model(model).map(Some);
    }
    let mut active = model.into_active_model();
    active.quote_approval_policy = Set(effective);
    active.updated_at = Set(OffsetDateTime::now_utc());
    local_status_from_model(active.update(txn).await?).map(Some)
}

/// List an actor's public statuses for its ActivityPub outbox.
pub async fn public_local_statuses_by_account(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    limit: u64,
) -> Result<Vec<LocalStatus>> {
    let statuses = local_status::Entity::find()
        .filter(local_status::Column::AccountId.eq(account_id.0))
        .filter(local_status::Column::Visibility.eq(StatusVisibility::Public))
        .filter(local_status::Column::DeletedAt.is_null())
        .order_by_desc(local_status::Column::CreatedAt)
        .limit(limit)
        .all(db)
        .await?;
    statuses.into_iter().map(local_status_from_model).collect()
}

/// Count an actor's public statuses for its ActivityPub outbox metadata.
pub async fn count_public_local_statuses_by_account(
    db: &impl ConnectionTrait,
    account_id: AccountId,
) -> Result<u64> {
    Ok(local_status::Entity::find()
        .filter(local_status::Column::AccountId.eq(account_id.0))
        .filter(local_status::Column::Visibility.eq(StatusVisibility::Public))
        .filter(local_status::Column::DeletedAt.is_null())
        .count(db)
        .await?)
}

/// Count active statuses authored by a local account.
pub async fn count_local_statuses_by_account(
    db: &impl ConnectionTrait,
    account_id: AccountId,
) -> Result<u64> {
    Ok(local_status::Entity::find()
        .filter(local_status::Column::AccountId.eq(account_id.0))
        .filter(local_status::Column::DeletedAt.is_null())
        .count(db)
        .await?)
}

/// Return the latest active status timestamp for a local account.
pub async fn last_local_status_at(
    db: &impl ConnectionTrait,
    account_id: AccountId,
) -> Result<Option<OffsetDateTime>> {
    let status = local_status::Entity::find()
        .filter(local_status::Column::AccountId.eq(account_id.0))
        .filter(local_status::Column::DeletedAt.is_null())
        .order_by_desc(local_status::Column::CreatedAt)
        .one(db)
        .await?;

    Ok(status.map(|status| status.created_at))
}

/// Advance the stored directory activity timestamp after creating a local status.
async fn update_local_account_last_status_at(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    created_at: OffsetDateTime,
) -> Result<()> {
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "UPDATE local_account
            SET last_status_at = greatest(last_status_at, $2)
          WHERE id = $1",
        vec![account_id.0.into(), created_at.into()],
    ))
    .await?;
    Ok(())
}

/// Recompute local directory activity after a status is removed.
async fn refresh_local_account_last_status_at(
    db: &impl ConnectionTrait,
    account_id: AccountId,
) -> Result<()> {
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "UPDATE local_account
            SET last_status_at = (
                SELECT max(created_at)
                  FROM local_status
                 WHERE account_id = $1 AND deleted_at IS NULL
            )
          WHERE id = $1",
        vec![account_id.0.into()],
    ))
    .await?;
    Ok(())
}

/// Recompute cached remote directory activity after a Note changes lifecycle state.
async fn refresh_remote_actor_last_status_at(
    db: &impl ConnectionTrait,
    actor_id: AccountId,
) -> Result<()> {
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "UPDATE remote_actor
            SET last_status_at = (
                SELECT max(published_at)
                  FROM remote_status
                 WHERE remote_actor_id = $1 AND deleted_at IS NULL
            )
          WHERE id = $1",
        vec![actor_id.0.into()],
    ))
    .await?;
    Ok(())
}

/// Count active local replies to a status.
pub async fn count_local_replies(db: &DbConnection, status_id: StatusId) -> Result<u64> {
    Ok(local_status::Entity::find()
        .filter(local_status::Column::InReplyToId.eq(status_id.0))
        .filter(local_status::Column::DeletedAt.is_null())
        .count(db)
        .await?)
}

/// Count active cached local and remote direct replies to one status.
pub async fn count_status_context_replies(
    db: &impl ConnectionTrait,
    parent: StatusContextParent,
) -> Result<u64> {
    let count = match parent {
        StatusContextParent::Local(status_id) => {
            local_status::Entity::find()
                .filter(local_status::Column::InReplyToId.eq(status_id.0))
                .filter(local_status::Column::DeletedAt.is_null())
                .count(db)
                .await?
                + remote_status::Entity::find()
                    .filter(remote_status::Column::InReplyToLocalStatusId.eq(status_id.0))
                    .filter(remote_status::Column::DeletedAt.is_null())
                    .count(db)
                    .await?
        }
        StatusContextParent::Remote(status_id) => {
            local_status::Entity::find()
                .filter(local_status::Column::InReplyToRemoteStatusId.eq(status_id.0))
                .filter(local_status::Column::DeletedAt.is_null())
                .count(db)
                .await?
                + remote_status::Entity::find()
                    .filter(remote_status::Column::InReplyToRemoteStatusId.eq(status_id.0))
                    .filter(remote_status::Column::DeletedAt.is_null())
                    .count(db)
                    .await?
        }
    };
    Ok(count)
}

/// List active cached local and remote direct replies, oldest first.
pub async fn status_context_replies(
    db: &impl ConnectionTrait,
    parent: StatusContextParent,
) -> Result<Vec<StatusContextItem>> {
    let (locals, remotes) = match parent {
        StatusContextParent::Local(status_id) => (
            local_status::Entity::find()
                .filter(local_status::Column::InReplyToId.eq(status_id.0))
                .filter(local_status::Column::DeletedAt.is_null())
                .all(db)
                .await?,
            remote_status::Entity::find()
                .filter(remote_status::Column::InReplyToLocalStatusId.eq(status_id.0))
                .filter(remote_status::Column::DeletedAt.is_null())
                .all(db)
                .await?,
        ),
        StatusContextParent::Remote(status_id) => (
            local_status::Entity::find()
                .filter(local_status::Column::InReplyToRemoteStatusId.eq(status_id.0))
                .filter(local_status::Column::DeletedAt.is_null())
                .all(db)
                .await?,
            remote_status::Entity::find()
                .filter(remote_status::Column::InReplyToRemoteStatusId.eq(status_id.0))
                .filter(remote_status::Column::DeletedAt.is_null())
                .all(db)
                .await?,
        ),
    };
    let mut replies = locals
        .into_iter()
        .map(local_status_from_model)
        .map(|status| status.map(StatusContextItem::Local))
        .chain(
            remotes
                .into_iter()
                .map(remote_status_from_model)
                .map(|status| status.map(StatusContextItem::Remote)),
        )
        .collect::<Result<Vec<_>>>()?;
    replies.sort_by_key(|reply| reply.id().0);
    Ok(replies)
}

/// List active direct replies to a local status, oldest first.
pub async fn local_replies_to_status(
    db: &DbConnection,
    status_id: StatusId,
) -> Result<Vec<LocalStatus>> {
    let statuses = local_status::Entity::find()
        .filter(local_status::Column::InReplyToId.eq(status_id.0))
        .filter(local_status::Column::DeletedAt.is_null())
        .order_by_asc(local_status::Column::Id)
        .all(db)
        .await?;

    statuses.into_iter().map(local_status_from_model).collect()
}

/// Attach a direct status to a local conversation and update participant views.
pub async fn attach_direct_status_to_conversation(
    txn: &DatabaseTransaction,
    status_id: StatusId,
    author_id: AccountId,
    parent_id: Option<StatusId>,
    parent_remote_status_id: Option<StatusId>,
    participant_ids: &[AccountId],
    remote_participants: &[RemoteConversationParticipant],
) -> Result<Uuid> {
    let now = OffsetDateTime::now_utc();
    let parent_conversation_id = match parent_id {
        Some(parent_id) => local_status::Entity::find_by_id(parent_id.0)
            .one(txn)
            .await?
            .and_then(|status| status.conversation_id),
        None => match parent_remote_status_id {
            Some(parent_id) => remote_status::Entity::find_by_id(parent_id.0)
                .one(txn)
                .await?
                .and_then(|status| status.conversation_id),
            None => None,
        },
    };
    let conversation_id = match parent_conversation_id {
        Some(conversation_id) => conversation_id,
        None => {
            local_conversation::ActiveModel {
                id: Set(Uuid::now_v7()),
                last_status_id: Set(Some(status_id.0)),
                last_remote_status_id: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
            }
            .insert(txn)
            .await?
            .id
        }
    };

    let mut status = local_status::Entity::find_by_id(status_id.0)
        .one(txn)
        .await?
        .ok_or_else(|| RoostyError::InvalidInput("conversation status not found".to_owned()))?
        .into_active_model();
    status.conversation_id = Set(Some(conversation_id));
    status.updated_at = Set(now);
    status.update(txn).await?;

    let mut conversation = local_conversation::Entity::find_by_id(conversation_id)
        .one(txn)
        .await?
        .ok_or_else(|| RoostyError::InvalidInput("conversation not found".to_owned()))?
        .into_active_model();
    conversation.last_status_id = Set(Some(status_id.0));
    conversation.updated_at = Set(now);
    conversation.update(txn).await?;

    let existing_participants = local_conversation_account::Entity::find()
        .filter(local_conversation_account::Column::ConversationId.eq(conversation_id))
        .all(txn)
        .await?;
    let mut account_ids = existing_participants
        .iter()
        .filter(|participant| participant.account_id == author_id.0)
        .map(|participant| AccountId(participant.account_id))
        .chain(std::iter::once(author_id))
        .chain(participant_ids.iter().copied())
        .collect::<Vec<_>>();
    account_ids.sort_by_key(|account_id| account_id.0);
    account_ids.dedup();

    for account_id in account_ids {
        let unread = account_id != author_id;
        let existing = existing_participants
            .iter()
            .find(|participant| participant.account_id == account_id.0);
        match existing {
            Some(participant) => {
                let mut active = participant.clone().into_active_model();
                active.cursor_id = Set(Uuid::now_v7());
                active.unread = Set(unread);
                active.hidden_at = Set(None);
                active.last_status_id = Set(Some(status_id.0));
                active.last_remote_status_id = Set(None);
                active.updated_at = Set(now);
                active.update(txn).await?;
            }
            None => {
                local_conversation_account::ActiveModel {
                    id: Set(Uuid::now_v7()),
                    cursor_id: Set(Uuid::now_v7()),
                    conversation_id: Set(conversation_id),
                    account_id: Set(account_id.0),
                    unread: Set(unread),
                    hidden_at: Set(None),
                    last_status_id: Set(Some(status_id.0)),
                    last_remote_status_id: Set(None),
                    created_at: Set(now),
                    updated_at: Set(now),
                }
                .insert(txn)
                .await?;
            }
        }
    }

    upsert_remote_conversation_participants(txn, conversation_id, remote_participants).await?;
    Ok(conversation_id)
}

/// Add newly addressed accounts to an existing direct conversation without changing
/// established views; the caller subsequently recalculates each account's last status.
pub async fn sync_edited_direct_status_conversation(
    txn: &DatabaseTransaction,
    status_id: StatusId,
    author_id: AccountId,
    participant_ids: &[AccountId],
    remote_participants: &[RemoteConversationParticipant],
) -> Result<Uuid> {
    let conversation_id = local_status::Entity::find_by_id(status_id.0)
        .one(txn)
        .await?
        .and_then(|status| status.conversation_id)
        .ok_or_else(|| RoostyError::InvalidInput("conversation status not found".to_owned()))?;
    let now = OffsetDateTime::now_utc();
    let existing = local_conversation_account::Entity::find()
        .filter(local_conversation_account::Column::ConversationId.eq(conversation_id))
        .all(txn)
        .await?;
    let mut account_ids = std::iter::once(author_id)
        .chain(participant_ids.iter().copied())
        .collect::<Vec<_>>();
    account_ids.sort_by_key(|account_id| account_id.0);
    account_ids.dedup();
    for account_id in account_ids {
        if existing
            .iter()
            .any(|participant| participant.account_id == account_id.0)
        {
            continue;
        }
        local_conversation_account::ActiveModel {
            id: Set(Uuid::now_v7()),
            cursor_id: Set(Uuid::now_v7()),
            conversation_id: Set(conversation_id),
            account_id: Set(account_id.0),
            unread: Set(account_id != author_id),
            hidden_at: Set(None),
            last_status_id: Set(None),
            last_remote_status_id: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(txn)
        .await?;
    }
    upsert_remote_conversation_participants(txn, conversation_id, remote_participants).await?;
    Ok(conversation_id)
}

/// Attach a cached direct Note to a conversation visible to its local recipients.
pub async fn attach_remote_direct_status_to_conversation(
    txn: &DatabaseTransaction,
    status_id: StatusId,
    parent_local_status_id: Option<StatusId>,
    parent_remote_status_id: Option<StatusId>,
    local_recipient_ids: &[AccountId],
    remote_participants: &[RemoteConversationParticipant],
    mark_unread: bool,
) -> Result<DirectConversationRefresh> {
    let now = OffsetDateTime::now_utc();
    remote_status_local_recipient::Entity::delete_many()
        .filter(remote_status_local_recipient::Column::RemoteStatusId.eq(status_id.0))
        .exec(txn)
        .await?;
    remote_status_remote_recipient::Entity::delete_many()
        .filter(remote_status_remote_recipient::Column::RemoteStatusId.eq(status_id.0))
        .exec(txn)
        .await?;
    for account_id in local_recipient_ids {
        remote_status_local_recipient::ActiveModel {
            remote_status_id: Set(status_id.0),
            account_id: Set(account_id.0),
            created_at: Set(now),
        }
        .insert(txn)
        .await?;
    }
    for participant in remote_participants {
        remote_status_remote_recipient::ActiveModel {
            remote_status_id: Set(status_id.0),
            activitypub_id: Set(participant.activitypub_id.clone()),
            remote_actor_id: Set(participant.remote_actor_id.map(|id| id.0)),
            mention_name: Set(participant.mention_name.clone()),
            created_at: Set(now),
        }
        .insert(txn)
        .await?;
    }
    let parent_conversation_id = match parent_local_status_id {
        Some(parent_id) => local_status::Entity::find_by_id(parent_id.0)
            .one(txn)
            .await?
            .and_then(|status| status.conversation_id),
        None => match parent_remote_status_id {
            Some(parent_id) => remote_status::Entity::find_by_id(parent_id.0)
                .one(txn)
                .await?
                .and_then(|status| status.conversation_id),
            None => None,
        },
    };
    let conversation_id = match parent_conversation_id {
        Some(id) => id,
        None => {
            local_conversation::ActiveModel {
                id: Set(Uuid::now_v7()),
                last_status_id: Set(None),
                last_remote_status_id: Set(Some(status_id.0)),
                created_at: Set(now),
                updated_at: Set(now),
            }
            .insert(txn)
            .await?
            .id
        }
    };

    let mut status = remote_status::Entity::find_by_id(status_id.0)
        .one(txn)
        .await?
        .ok_or_else(|| {
            RoostyError::InvalidInput("remote conversation status not found".to_owned())
        })?
        .into_active_model();
    status.conversation_id = Set(Some(conversation_id));
    status.update(txn).await?;

    let mut conversation = local_conversation::Entity::find_by_id(conversation_id)
        .one(txn)
        .await?
        .ok_or_else(|| RoostyError::InvalidInput("conversation not found".to_owned()))?
        .into_active_model();
    if mark_unread {
        conversation.last_status_id = Set(None);
        conversation.last_remote_status_id = Set(Some(status_id.0));
        conversation.updated_at = Set(now);
        conversation.update(txn).await?;
    }

    let existing = local_conversation_account::Entity::find()
        .filter(local_conversation_account::Column::ConversationId.eq(conversation_id))
        .all(txn)
        .await?;
    for account_id in local_recipient_ids {
        match existing.iter().find(|row| row.account_id == account_id.0) {
            Some(row) => {
                let mut active = row.clone().into_active_model();
                if mark_unread {
                    active.cursor_id = Set(Uuid::now_v7());
                    active.unread = Set(true);
                    active.hidden_at = Set(None);
                    active.last_status_id = Set(None);
                    active.last_remote_status_id = Set(Some(status_id.0));
                }
                active.updated_at = Set(now);
                active.update(txn).await?;
            }
            None => {
                local_conversation_account::ActiveModel {
                    id: Set(Uuid::now_v7()),
                    cursor_id: Set(Uuid::now_v7()),
                    conversation_id: Set(conversation_id),
                    account_id: Set(account_id.0),
                    unread: Set(mark_unread),
                    hidden_at: Set(None),
                    last_status_id: Set(None),
                    last_remote_status_id: Set(Some(status_id.0)),
                    created_at: Set(now),
                    updated_at: Set(now),
                }
                .insert(txn)
                .await?;
            }
        }
    }
    upsert_remote_conversation_participants(txn, conversation_id, remote_participants).await?;
    repair_direct_conversation_after_delete(txn, Some(conversation_id))
        .await?
        .ok_or_else(|| RoostyError::InvalidInput("conversation refresh is missing".to_owned()))
}

/// Replace explicit local recipients for a cached non-public Note.
pub async fn replace_remote_status_local_recipients(
    txn: &DatabaseTransaction,
    status_id: StatusId,
    account_ids: &[AccountId],
) -> Result<()> {
    remote_status_local_recipient::Entity::delete_many()
        .filter(remote_status_local_recipient::Column::RemoteStatusId.eq(status_id.0))
        .exec(txn)
        .await?;
    remote_status_remote_recipient::Entity::delete_many()
        .filter(remote_status_remote_recipient::Column::RemoteStatusId.eq(status_id.0))
        .exec(txn)
        .await?;
    let now = OffsetDateTime::now_utc();
    for account_id in account_ids {
        remote_status_local_recipient::ActiveModel {
            remote_status_id: Set(status_id.0),
            account_id: Set(account_id.0),
            created_at: Set(now),
        }
        .insert(txn)
        .await?;
    }
    Ok(())
}

/// Persist remote participants without resolving uncached ActivityPub actors.
pub async fn upsert_remote_conversation_participants(
    txn: &DatabaseTransaction,
    conversation_id: Uuid,
    participants: &[RemoteConversationParticipant],
) -> Result<()> {
    let now = OffsetDateTime::now_utc();
    for participant in participants {
        let existing = local_conversation_remote_participant::Entity::find()
            .filter(
                local_conversation_remote_participant::Column::ConversationId.eq(conversation_id),
            )
            .filter(
                local_conversation_remote_participant::Column::ActivitypubId
                    .eq(&participant.activitypub_id),
            )
            .one(txn)
            .await?;
        match existing {
            Some(row) => {
                let mut active = row.into_active_model();
                if participant.remote_actor_id.is_some() {
                    active.remote_actor_id = Set(participant.remote_actor_id.map(|id| id.0));
                }
                if participant.mention_name.is_some() {
                    active.mention_name = Set(participant.mention_name.clone());
                }
                active.updated_at = Set(now);
                active.update(txn).await?;
            }
            None => {
                local_conversation_remote_participant::ActiveModel {
                    id: Set(Uuid::now_v7()),
                    conversation_id: Set(conversation_id),
                    activitypub_id: Set(participant.activitypub_id.clone()),
                    remote_actor_id: Set(participant.remote_actor_id.map(|id| id.0)),
                    mention_name: Set(participant.mention_name.clone()),
                    created_at: Set(now),
                    updated_at: Set(now),
                }
                .insert(txn)
                .await?;
            }
        }
    }
    Ok(())
}

/// List remote and unresolved direct-conversation participants.
pub async fn remote_conversation_participants(
    db: &DbConnection,
    conversation_id: Uuid,
) -> Result<Vec<RemoteConversationParticipant>> {
    Ok(local_conversation_remote_participant::Entity::find()
        .filter(local_conversation_remote_participant::Column::ConversationId.eq(conversation_id))
        .all(db)
        .await?
        .into_iter()
        .map(|participant| RemoteConversationParticipant {
            activitypub_id: participant.activitypub_id,
            remote_actor_id: participant.remote_actor_id.map(AccountId),
            mention_name: participant.mention_name,
        })
        .collect())
}

/// Recompute a direct conversation's latest message after either kind of direct-status deletion.
pub async fn repair_direct_conversation_after_delete(
    txn: &DatabaseTransaction,
    conversation_id: Option<Uuid>,
) -> Result<Option<DirectConversationRefresh>> {
    let Some(conversation_id) = conversation_id else {
        return Ok(None);
    };

    // One shared latest status is retained for Mastodon conversation projection.
    // Per-account views below are deliberately calculated separately because direct
    // recipients may differ from one status to the next.
    txn.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"
            WITH candidates AS (
                SELECT id AS local_status_id, NULL::uuid AS remote_status_id, created_at AS occurred_at, 0 AS kind
                FROM local_status
                WHERE conversation_id = $1 AND visibility = 'direct' AND deleted_at IS NULL
                UNION ALL
                SELECT NULL::uuid, id, published_at, 1
                FROM remote_status
                WHERE conversation_id = $1 AND visibility = 'direct' AND deleted_at IS NULL
            ), latest AS (
                SELECT local_status_id, remote_status_id
                FROM candidates
                ORDER BY occurred_at DESC, kind ASC
                LIMIT 1
            )
            UPDATE local_conversation
            SET last_status_id = (SELECT local_status_id FROM latest),
                last_remote_status_id = (SELECT remote_status_id FROM latest),
                updated_at = NOW()
            WHERE id = $1
        "#,
        vec![conversation_id.into()],
    )).await?;

    // Rank every visible status per view in SQL. This avoids loading all statuses
    // and issuing recipient lookups once for every account/status pair.
    let updated_account_ids = txn.query_all(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"
            WITH candidates AS (
                SELECT view.id AS view_id, status.id AS local_status_id,
                       NULL::uuid AS remote_status_id, status.created_at AS occurred_at, 0 AS kind
                FROM local_conversation_account AS view
                JOIN local_status AS status ON status.conversation_id = view.conversation_id
                WHERE view.conversation_id = $1
                  AND status.visibility = 'direct'
                  AND status.deleted_at IS NULL
                  AND (status.account_id = view.account_id OR EXISTS (
                      SELECT 1 FROM local_status_local_recipient AS recipient
                      WHERE recipient.status_id = status.id AND recipient.account_id = view.account_id
                  ))
                UNION ALL
                SELECT view.id, NULL::uuid, status.id, status.published_at, 1
                FROM local_conversation_account AS view
                JOIN remote_status AS status ON status.conversation_id = view.conversation_id
                WHERE view.conversation_id = $1
                  AND status.visibility = 'direct'
                  AND status.deleted_at IS NULL
                  AND EXISTS (
                      SELECT 1 FROM remote_status_local_recipient AS recipient
                      WHERE recipient.remote_status_id = status.id AND recipient.account_id = view.account_id
                  )
            ), ranked AS (
                SELECT *, ROW_NUMBER() OVER (
                    PARTITION BY view_id ORDER BY occurred_at DESC, kind ASC
                ) AS position
                FROM candidates
            ), latest AS (
                SELECT view_id, local_status_id, remote_status_id
                FROM ranked WHERE position = 1
            )
            UPDATE local_conversation_account AS view
            SET last_status_id = latest.local_status_id,
                last_remote_status_id = latest.remote_status_id,
                updated_at = NOW()
            FROM latest
            WHERE view.id = latest.view_id
              AND (view.last_status_id, view.last_remote_status_id)
                  IS DISTINCT FROM (latest.local_status_id, latest.remote_status_id)
            RETURNING view.account_id
        "#,
        vec![conversation_id.into()],
    )).await?
        .into_iter()
        .map(|row| row.try_get::<Uuid>("", "account_id").map(AccountId))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let removed_account_ids = txn.query_all(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"
            DELETE FROM local_conversation_account AS view
            WHERE view.conversation_id = $1
              AND NOT EXISTS (
                  SELECT 1
                  FROM local_status AS status
                  WHERE status.conversation_id = view.conversation_id
                    AND status.visibility = 'direct'
                    AND status.deleted_at IS NULL
                    AND (status.account_id = view.account_id OR EXISTS (
                        SELECT 1 FROM local_status_local_recipient AS recipient
                        WHERE recipient.status_id = status.id AND recipient.account_id = view.account_id
                    ))
                  UNION ALL
                  SELECT 1
                  FROM remote_status AS status
                  WHERE status.conversation_id = view.conversation_id
                    AND status.visibility = 'direct'
                    AND status.deleted_at IS NULL
                    AND EXISTS (
                        SELECT 1 FROM remote_status_local_recipient AS recipient
                        WHERE recipient.remote_status_id = status.id AND recipient.account_id = view.account_id
                    )
              )
            RETURNING view.account_id
        "#,
        vec![conversation_id.into()],
    )).await?
        .into_iter()
        .map(|row| row.try_get::<Uuid>("", "account_id").map(AccountId))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(Some(DirectConversationRefresh {
        conversation_id,
        updated_account_ids,
        removed_account_ids,
    }))
}

/// Return the conversation containing a cached remote status, including a just-deleted status.
pub async fn remote_status_conversation_id(
    txn: &DatabaseTransaction,
    status_id: StatusId,
) -> Result<Option<Uuid>> {
    Ok(remote_status::Entity::find_by_id(status_id.0)
        .one(txn)
        .await?
        .and_then(|status| status.conversation_id))
}

/// Return whether an account participates in a status's direct conversation.
pub async fn local_status_visible_to_account(
    db: &impl ConnectionTrait,
    status: &LocalStatus,
    account_id: AccountId,
) -> Result<bool> {
    if matches!(
        status.visibility,
        StatusVisibility::Public | StatusVisibility::Unlisted
    ) || status.account_id == account_id
    {
        return Ok(true);
    }
    if status.visibility == StatusVisibility::Private {
        let follows = local_follow::Entity::find()
            .filter(local_follow::Column::FollowerAccountId.eq(account_id.0))
            .filter(local_follow::Column::FollowedAccountId.eq(status.account_id.0))
            .one(db)
            .await?
            .is_some();
        if follows {
            return Ok(true);
        }
    } else if status.visibility != StatusVisibility::Direct {
        return Ok(false);
    }
    Ok(local_status_local_recipient::Entity::find()
        .filter(local_status_local_recipient::Column::StatusId.eq(status.id.0))
        .filter(local_status_local_recipient::Column::AccountId.eq(account_id.0))
        .one(db)
        .await?
        .is_some())
}

/// Return whether a local account participates in a cached remote direct Note's conversation.
pub async fn remote_status_visible_to_account(
    db: &impl ConnectionTrait,
    status: &RemoteStatus,
    account_id: AccountId,
) -> Result<bool> {
    if find_local_remote_account_block(db, account_id, status.remote_actor_id)
        .await?
        .is_some()
    {
        return Ok(false);
    }
    if matches!(
        status.visibility,
        StatusVisibility::Public | StatusVisibility::Unlisted
    ) {
        return Ok(true);
    }
    if status.visibility == StatusVisibility::Private {
        let follows = remote_following::Entity::find()
            .filter(remote_following::Column::LocalAccountId.eq(account_id.0))
            .filter(remote_following::Column::RemoteActorId.eq(status.remote_actor_id.0))
            .filter(remote_following::Column::State.eq(RemoteFollowState::Accepted))
            .filter(remote_following::Column::DeactivatedAt.is_null())
            .one(db)
            .await?
            .is_some();
        if follows {
            return Ok(true);
        }
    } else if status.visibility != StatusVisibility::Direct {
        return Ok(false);
    }
    Ok(remote_status_local_recipient::Entity::find()
        .filter(remote_status_local_recipient::Column::RemoteStatusId.eq(status.id.0))
        .filter(remote_status_local_recipient::Column::AccountId.eq(account_id.0))
        .one(db)
        .await?
        .is_some())
}

/// List local accounts explicitly addressed by a cached non-public Note.
pub async fn remote_status_local_recipients(
    db: &impl ConnectionTrait,
    status_id: StatusId,
) -> Result<Vec<AccountId>> {
    Ok(remote_status_local_recipient::Entity::find()
        .filter(remote_status_local_recipient::Column::RemoteStatusId.eq(status_id.0))
        .all(db)
        .await?
        .into_iter()
        .map(|recipient| AccountId(recipient.account_id))
        .collect())
}

/// List local accounts explicitly addressed by a local non-public status.
pub async fn local_status_local_recipients(
    db: &impl ConnectionTrait,
    status_id: StatusId,
) -> Result<Vec<AccountId>> {
    Ok(local_status_local_recipient::Entity::find()
        .filter(local_status_local_recipient::Column::StatusId.eq(status_id.0))
        .all(db)
        .await?
        .into_iter()
        .map(|recipient| AccountId(recipient.account_id))
        .collect())
}

/// Return the exact audience of the direct status selected for a conversation view.
pub async fn direct_status_participants_for_view(
    db: &impl ConnectionTrait,
    view: &LocalConversationAccount,
) -> Result<DirectStatusParticipants> {
    let mut participants = DirectStatusParticipants::default();
    if let Some(status_id) = view.last_status_id {
        if let Some(status) = find_local_status_by_id(db, status_id).await?
            && let Some(author) = find_local_account_by_id(db, status.account_id).await?
        {
            participants.local_accounts.push(author);
        }
        for row in local_status_local_recipient::Entity::find()
            .filter(local_status_local_recipient::Column::StatusId.eq(status_id.0))
            .all(db)
            .await?
        {
            if let Some(account) = find_local_account_by_id(db, AccountId(row.account_id)).await? {
                participants.local_accounts.push(account);
            }
        }
        participants.remote_accounts =
            remote_conversation_participants_for_local_status(db, status_id).await?;
    }
    if let Some(status_id) = view.last_remote_status_id {
        if let Some(status) = find_remote_status_by_id(db, status_id).await?
            && let Some(author) = find_remote_actor_by_id(db, status.remote_actor_id).await?
        {
            participants
                .remote_accounts
                .push(RemoteConversationParticipant {
                    activitypub_id: author.activitypub_id,
                    remote_actor_id: Some(author.id),
                    mention_name: Some(format!("@{}@{}", author.username, author.domain)),
                });
        }
        for row in remote_status_local_recipient::Entity::find()
            .filter(remote_status_local_recipient::Column::RemoteStatusId.eq(status_id.0))
            .all(db)
            .await?
        {
            if let Some(account) = find_local_account_by_id(db, AccountId(row.account_id)).await? {
                participants.local_accounts.push(account);
            }
        }
        participants.remote_accounts.extend(
            remote_status_remote_recipient::Entity::find()
                .filter(remote_status_remote_recipient::Column::RemoteStatusId.eq(status_id.0))
                .all(db)
                .await?
                .into_iter()
                .map(|row| RemoteConversationParticipant {
                    activitypub_id: row.activitypub_id,
                    remote_actor_id: row.remote_actor_id.map(AccountId),
                    mention_name: row.mention_name,
                })
                .collect::<Vec<_>>(),
        );
    }
    Ok(participants)
}

async fn remote_conversation_participants_for_local_status(
    db: &impl ConnectionTrait,
    status_id: StatusId,
) -> Result<Vec<RemoteConversationParticipant>> {
    Ok(remote_mentions_for_local_status(db, status_id)
        .await?
        .into_iter()
        .map(|actor| RemoteConversationParticipant {
            activitypub_id: actor.activitypub_id,
            remote_actor_id: Some(actor.id),
            mention_name: Some(format!("@{}@{}", actor.username, actor.domain)),
        })
        .collect())
}

/// List visible local direct conversations for an account.
pub async fn local_conversations_for_account(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    limit: u64,
    cursor: CollectionCursor,
) -> Result<CollectionPage<LocalConversationView>> {
    let rows = local_conversation_account::Entity::find()
        .filter(local_conversation_account::Column::AccountId.eq(account_id.0))
        .filter(local_conversation_account::Column::HiddenAt.is_null())
        .apply_collection_cursor(cursor)
        .order_by_desc(local_conversation_account::Column::CursorId)
        .limit(page_query_limit(limit))
        .all(db)
        .await?;
    let (rows, has_more) = trim_to_page(rows, limit);
    let first_cursor = rows.first().map(|row| row.cursor_id);
    let last_cursor = rows.last().map(|row| row.cursor_id);
    let mut items = Vec::with_capacity(rows.len());

    for row in rows {
        let Some(conversation) = local_conversation::Entity::find_by_id(row.conversation_id)
            .one(db)
            .await?
        else {
            continue;
        };
        items.push(LocalConversationView {
            conversation: local_conversation_from_model(conversation),
            account: local_conversation_account_from_model(row),
        });
    }

    Ok(CollectionPage {
        items,
        first_cursor,
        last_cursor,
        has_more,
    })
}

/// Find one visible local conversation owned by an account.
pub async fn find_local_conversation_for_account(
    db: &DbConnection,
    account_id: AccountId,
    conversation_account_id: Uuid,
) -> Result<Option<LocalConversationView>> {
    let Some(row) = local_conversation_account::Entity::find_by_id(conversation_account_id)
        .filter(local_conversation_account::Column::AccountId.eq(account_id.0))
        .filter(local_conversation_account::Column::HiddenAt.is_null())
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    let Some(conversation) = local_conversation::Entity::find_by_id(row.conversation_id)
        .one(db)
        .await?
    else {
        return Ok(None);
    };

    Ok(Some(LocalConversationView {
        conversation: local_conversation_from_model(conversation),
        account: local_conversation_account_from_model(row),
    }))
}

/// List visible account-specific views for one local conversation.
pub async fn local_conversation_views(
    db: &impl ConnectionTrait,
    conversation_id: Uuid,
) -> Result<Vec<LocalConversationView>> {
    let Some(conversation) = local_conversation::Entity::find_by_id(conversation_id)
        .one(db)
        .await?
    else {
        return Ok(Vec::new());
    };
    let rows = local_conversation_account::Entity::find()
        .filter(local_conversation_account::Column::ConversationId.eq(conversation_id))
        .filter(local_conversation_account::Column::HiddenAt.is_null())
        .all(db)
        .await?;
    let conversation = local_conversation_from_model(conversation);

    Ok(rows
        .into_iter()
        .map(|row| LocalConversationView {
            conversation: conversation.clone(),
            account: local_conversation_account_from_model(row),
        })
        .collect())
}

/// List accounts whose conversation view currently presents a given local status.
pub async fn local_conversation_accounts_for_last_status(
    db: &impl ConnectionTrait,
    conversation_id: Uuid,
    status_id: StatusId,
) -> Result<Vec<AccountId>> {
    Ok(local_conversation_account::Entity::find()
        .filter(local_conversation_account::Column::ConversationId.eq(conversation_id))
        .filter(local_conversation_account::Column::LastStatusId.eq(status_id.0))
        .all(db)
        .await?
        .into_iter()
        .map(|view| AccountId(view.account_id))
        .collect())
}

/// List accounts whose conversation view currently presents a given remote status.
pub async fn local_conversation_accounts_for_last_remote_status(
    db: &impl ConnectionTrait,
    conversation_id: Uuid,
    status_id: StatusId,
) -> Result<Vec<AccountId>> {
    Ok(local_conversation_account::Entity::find()
        .filter(local_conversation_account::Column::ConversationId.eq(conversation_id))
        .filter(local_conversation_account::Column::LastRemoteStatusId.eq(status_id.0))
        .all(db)
        .await?
        .into_iter()
        .map(|view| AccountId(view.account_id))
        .collect())
}

/// Mark a local conversation as read for one account.
pub async fn mark_local_conversation_read(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    conversation_account_id: Uuid,
) -> Result<Option<LocalConversationView>> {
    let Some(row) =
        find_local_conversation_account_model(db, account_id, conversation_account_id).await?
    else {
        return Ok(None);
    };
    let mut active = row.into_active_model();
    active.unread = Set(false);
    active.updated_at = Set(OffsetDateTime::now_utc());
    let row = active.update(db).await?;

    let Some(conversation) = local_conversation::Entity::find_by_id(row.conversation_id)
        .one(db)
        .await?
    else {
        return Ok(None);
    };

    Ok(Some(LocalConversationView {
        conversation: local_conversation_from_model(conversation),
        account: local_conversation_account_from_model(row),
    }))
}

/// Hide a local conversation for one account.
pub async fn hide_local_conversation(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    conversation_account_id: Uuid,
) -> Result<bool> {
    let Some(row) =
        find_local_conversation_account_model(db, account_id, conversation_account_id).await?
    else {
        return Ok(false);
    };
    let mut active = row.into_active_model();
    active.hidden_at = Set(Some(OffsetDateTime::now_utc()));
    active.updated_at = Set(OffsetDateTime::now_utc());
    active.update(db).await?;

    Ok(true)
}

/// List local accounts participating in a conversation.
pub async fn local_conversation_participants(
    db: &DbConnection,
    conversation_id: Uuid,
) -> Result<Vec<LocalAccount>> {
    let rows = local_conversation_account::Entity::find()
        .filter(local_conversation_account::Column::ConversationId.eq(conversation_id))
        .all(db)
        .await?;
    let account_ids = rows
        .into_iter()
        .map(|row| AccountId(row.account_id))
        .collect::<Vec<_>>();

    local_accounts_by_id(db, account_ids).await
}

async fn find_local_conversation_account_model(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    conversation_account_id: Uuid,
) -> Result<Option<local_conversation_account::Model>> {
    Ok(
        local_conversation_account::Entity::find_by_id(conversation_account_id)
            .filter(local_conversation_account::Column::AccountId.eq(account_id.0))
            .filter(local_conversation_account::Column::HiddenAt.is_null())
            .one(db)
            .await?,
    )
}

/// Mark a local status as favourited by an account.
pub async fn favourite_local_status(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    status_id: StatusId,
) -> Result<()> {
    if local_status_favourite::Entity::find_by_id((account_id.0, status_id.0))
        .one(db)
        .await?
        .is_none()
    {
        local_status_favourite::ActiveModel {
            id: Set(Uuid::now_v7()),
            account_id: Set(account_id.0),
            status_id: Set(status_id.0),
            created_at: Set(OffsetDateTime::now_utc()),
        }
        .insert(db)
        .await?;
        adjust_status_trend(db, TrendTarget::LocalStatus(status_id), 1, 0).await?;
    }
    Ok(())
}

/// Store a remote actor's favourite of a local status idempotently.
pub async fn favourite_local_status_by_remote_actor(
    db: &DbConnection,
    remote_actor_id: AccountId,
    status_id: StatusId,
    activity_id: &str,
) -> Result<bool> {
    let txn = db.begin().await?;
    let existing = remote_status_favourite::Entity::find()
        .filter(remote_status_favourite::Column::RemoteActorId.eq(remote_actor_id.0))
        .filter(remote_status_favourite::Column::LocalStatusId.eq(status_id.0))
        .one(db)
        .await?;
    if existing.is_some() {
        txn.commit().await?;
        return Ok(false);
    }
    remote_status_favourite::ActiveModel {
        id: Set(Uuid::now_v7()),
        remote_actor_id: Set(remote_actor_id.0),
        local_status_id: Set(status_id.0),
        activity_id: Set(activity_id.to_owned()),
        created_at: Set(OffsetDateTime::now_utc()),
    }
    .insert(db)
    .await?;
    adjust_status_trend(db, TrendTarget::LocalStatus(status_id), 1, 0).await?;
    txn.commit().await?;
    Ok(true)
}

/// Apply an inbound Like and its idempotency record in one transaction.
///
/// When the Like is newly applied, the returned notification has been
/// committed with it and may safely be streamed by the caller.
pub async fn process_remote_like(
    txn: &sea_orm::DatabaseTransaction,
    remote_actor_id: AccountId,
    status_id: StatusId,
    activity_id: &str,
    recipient_account_id: AccountId,
) -> Result<Option<LocalNotification>> {
    if local_remote_accounts_are_blocked(txn, recipient_account_id, remote_actor_id).await? {
        return Ok(None);
    }
    let inserted = remote_status_favourite::Entity::insert(remote_status_favourite::ActiveModel {
        id: Set(Uuid::now_v7()),
        remote_actor_id: Set(remote_actor_id.0),
        local_status_id: Set(status_id.0),
        activity_id: Set(activity_id.to_owned()),
        created_at: Set(OffsetDateTime::now_utc()),
    })
    .on_conflict_do_nothing()
    .exec(txn)
    .await?;
    let newly_inserted = matches!(inserted, TryInsertResult::Inserted(_));
    if newly_inserted {
        adjust_status_trend(txn, TrendTarget::LocalStatus(status_id), 1, 0).await?;
    }
    let notification = if newly_inserted
        && remote_account_allows_notification(txn, recipient_account_id, remote_actor_id).await?
    {
        notify_remote_actor_favourite(txn, recipient_account_id, remote_actor_id, status_id).await?
    } else {
        None
    };
    Ok(notification)
}

/// Remove a remote actor's favourite identified by its original Like activity.
pub async fn unfavourite_local_status_by_remote_actor(
    db: &DbConnection,
    remote_actor_id: AccountId,
    activity_id: &str,
) -> Result<bool> {
    let txn = db.begin().await?;
    let model = remote_status_favourite::Entity::find()
        .filter(remote_status_favourite::Column::RemoteActorId.eq(remote_actor_id.0))
        .filter(remote_status_favourite::Column::ActivityId.eq(activity_id))
        .one(db)
        .await?;
    if let Some(model) = model {
        let status_id = StatusId(model.local_status_id);
        model.into_active_model().delete(db).await?;
        adjust_status_trend(db, TrendTarget::LocalStatus(status_id), -1, 0).await?;
        txn.commit().await?;
        return Ok(true);
    }
    txn.commit().await?;
    Ok(false)
}

/// Record an inbound Undo(Like) and remove its original Like atomically.
pub async fn process_remote_undo_like(
    txn: &sea_orm::DatabaseTransaction,
    remote_actor_id: AccountId,
    original_activity_id: &str,
) -> Result<bool> {
    let model = remote_status_favourite::Entity::find()
        .filter(remote_status_favourite::Column::RemoteActorId.eq(remote_actor_id.0))
        .filter(remote_status_favourite::Column::ActivityId.eq(original_activity_id))
        .one(txn)
        .await?;
    if let Some(model) = model {
        let status_id = StatusId(model.local_status_id);
        model.into_active_model().delete(txn).await?;
        adjust_status_trend(txn, TrendTarget::LocalStatus(status_id), -1, 0).await?;
    }
    Ok(true)
}

/// Store one local account's favourite of a cached remote Note.
pub async fn favourite_remote_status(
    db: &DbConnection,
    local_account_id: AccountId,
    remote_status_id: StatusId,
    activity_id: &str,
) -> Result<()> {
    let txn = db.begin().await?;
    if local_remote_status_favourite::Entity::find()
        .filter(local_remote_status_favourite::Column::LocalAccountId.eq(local_account_id.0))
        .filter(local_remote_status_favourite::Column::RemoteStatusId.eq(remote_status_id.0))
        .one(db)
        .await?
        .is_none()
    {
        local_remote_status_favourite::ActiveModel {
            id: Set(Uuid::now_v7()),
            local_account_id: Set(local_account_id.0),
            remote_status_id: Set(remote_status_id.0),
            activity_id: Set(activity_id.to_owned()),
            created_at: Set(OffsetDateTime::now_utc()),
        }
        .insert(db)
        .await?;
        adjust_status_trend(db, TrendTarget::RemoteStatus(remote_status_id), 1, 0).await?;
    }
    txn.commit().await?;
    Ok(())
}

/// Store a remote-status favourite and its Like delivery job in `txn`.
pub async fn favourite_remote_status_with_job(
    txn: &impl ConnectionTrait,
    local_account_id: AccountId,
    remote_status_id: StatusId,
    activity_id: &str,
    job: NewJob,
) -> Result<()> {
    let inserted =
        local_remote_status_favourite::Entity::insert(local_remote_status_favourite::ActiveModel {
            id: Set(Uuid::now_v7()),
            local_account_id: Set(local_account_id.0),
            remote_status_id: Set(remote_status_id.0),
            activity_id: Set(activity_id.to_owned()),
            created_at: Set(OffsetDateTime::now_utc()),
        })
        .on_conflict_do_nothing()
        .exec(txn)
        .await?;
    if matches!(inserted, TryInsertResult::Inserted(_)) {
        adjust_status_trend(txn, TrendTarget::RemoteStatus(remote_status_id), 1, 0).await?;
    }
    enqueue_job_in_transaction(txn, job).await?;
    Ok(())
}

/// Remove and return a local favourite of a cached remote Note for Undo delivery.
pub async fn unfavourite_remote_status(
    db: &DbConnection,
    local_account_id: AccountId,
    remote_status_id: StatusId,
) -> Result<Option<LocalRemoteStatusFavourite>> {
    let txn = db.begin().await?;
    let favourite = local_remote_status_favourite::Entity::find()
        .filter(local_remote_status_favourite::Column::LocalAccountId.eq(local_account_id.0))
        .filter(local_remote_status_favourite::Column::RemoteStatusId.eq(remote_status_id.0))
        .one(db)
        .await?;
    let Some(favourite) = favourite else {
        txn.commit().await?;
        return Ok(None);
    };
    let result = LocalRemoteStatusFavourite {
        local_account_id: AccountId(favourite.local_account_id),
        remote_status_id: StatusId(favourite.remote_status_id),
        activity_id: favourite.activity_id.clone(),
    };
    favourite.into_active_model().delete(db).await?;
    adjust_status_trend(db, TrendTarget::RemoteStatus(remote_status_id), -1, 0).await?;
    txn.commit().await?;
    Ok(Some(result))
}

/// Find a local favourite of a cached remote Note without changing it.
pub async fn find_remote_status_favourite(
    db: &impl ConnectionTrait,
    local_account_id: AccountId,
    remote_status_id: StatusId,
) -> Result<Option<LocalRemoteStatusFavourite>> {
    Ok(local_remote_status_favourite::Entity::find()
        .filter(local_remote_status_favourite::Column::LocalAccountId.eq(local_account_id.0))
        .filter(local_remote_status_favourite::Column::RemoteStatusId.eq(remote_status_id.0))
        .one(db)
        .await?
        .map(|favourite| LocalRemoteStatusFavourite {
            local_account_id: AccountId(favourite.local_account_id),
            remote_status_id: StatusId(favourite.remote_status_id),
            activity_id: favourite.activity_id,
        }))
}

/// Remove a remote-status favourite and insert its Undo delivery job in `txn`.
pub async fn unfavourite_remote_status_with_job(
    txn: &impl ConnectionTrait,
    local_account_id: AccountId,
    remote_status_id: StatusId,
    job: NewJob,
) -> Result<Option<LocalRemoteStatusFavourite>> {
    let favourite = local_remote_status_favourite::Entity::find()
        .filter(local_remote_status_favourite::Column::LocalAccountId.eq(local_account_id.0))
        .filter(local_remote_status_favourite::Column::RemoteStatusId.eq(remote_status_id.0))
        .one(txn)
        .await?;
    let Some(favourite) = favourite else {
        return Ok(None);
    };
    let result = LocalRemoteStatusFavourite {
        local_account_id: AccountId(favourite.local_account_id),
        remote_status_id: StatusId(favourite.remote_status_id),
        activity_id: favourite.activity_id.clone(),
    };
    favourite.into_active_model().delete(txn).await?;
    adjust_status_trend(txn, TrendTarget::RemoteStatus(remote_status_id), -1, 0).await?;
    enqueue_job_in_transaction(txn, job).await?;
    Ok(Some(result))
}

/// Remove a local account's favourite from a status when it exists.
pub async fn unfavourite_local_status(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    status_id: StatusId,
) -> Result<()> {
    if let Some(model) = local_status_favourite::Entity::find_by_id((account_id.0, status_id.0))
        .one(db)
        .await?
    {
        model.into_active_model().delete(db).await?;
        adjust_status_trend(db, TrendTarget::LocalStatus(status_id), -1, 0).await?;
    }
    Ok(())
}

/// Count active local favourites on a status.
pub async fn count_local_favourites(db: &impl ConnectionTrait, status_id: StatusId) -> Result<u64> {
    let local = local_status_favourite::Entity::find()
        .filter(local_status_favourite::Column::StatusId.eq(status_id.0))
        .count(db)
        .await?;
    let remote = remote_status_favourite::Entity::find()
        .filter(remote_status_favourite::Column::LocalStatusId.eq(status_id.0))
        .count(db)
        .await?;
    Ok(local + remote)
}

/// Return whether a local account has favourited a status.
pub async fn is_local_status_favourited(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    status_id: StatusId,
) -> Result<bool> {
    Ok(
        local_status_favourite::Entity::find_by_id((account_id.0, status_id.0))
            .one(db)
            .await?
            .is_some(),
    )
}

/// Return whether a local account has favourited a cached remote Note.
pub async fn is_remote_status_favourited(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    status_id: StatusId,
) -> Result<bool> {
    Ok(local_remote_status_favourite::Entity::find()
        .filter(local_remote_status_favourite::Column::LocalAccountId.eq(account_id.0))
        .filter(local_remote_status_favourite::Column::RemoteStatusId.eq(status_id.0))
        .one(db)
        .await?
        .is_some())
}

/// List local statuses favourited by an account, newest favourite first.
pub async fn local_favourites_for_account(
    db: &DbConnection,
    account_id: AccountId,
    limit: u64,
    cursor: CollectionCursor,
) -> Result<CollectionPage<LocalStatus>> {
    let rows = local_status_favourite::Entity::find()
        .filter(local_status_favourite::Column::AccountId.eq(account_id.0))
        .apply_collection_cursor(cursor)
        .order_by_desc(local_status_favourite::Column::Id)
        .limit(page_query_limit(limit))
        .all(db)
        .await?;
    let (rows, has_more) = trim_to_page(rows, limit);
    let first_cursor = rows.first().map(|model| model.id);
    let last_cursor = rows.last().map(|model| model.id);
    let status_ids = rows
        .into_iter()
        .map(|model| StatusId(model.status_id))
        .collect::<Vec<_>>();

    Ok(CollectionPage {
        items: active_statuses_by_id(db, status_ids).await?,
        first_cursor,
        last_cursor,
        has_more,
    })
}

/// List local and cached remote statuses favourited by an account, newest first.
pub async fn favourites_for_account(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    limit: u64,
    cursor: CollectionCursor,
) -> Result<CollectionPage<FavouriteStatus>> {
    let local = local_status_favourite::Entity::find()
        .filter(local_status_favourite::Column::AccountId.eq(account_id.0))
        .apply_collection_cursor(cursor)
        .order_by_desc(local_status_favourite::Column::Id)
        .limit(page_query_limit(limit))
        .all(db)
        .await?;
    let remote = local_remote_status_favourite::Entity::find()
        .filter(local_remote_status_favourite::Column::LocalAccountId.eq(account_id.0))
        .apply_collection_cursor(cursor)
        .order_by_desc(local_remote_status_favourite::Column::Id)
        .limit(page_query_limit(limit))
        .all(db)
        .await?;
    let mut entries = Vec::new();
    for favourite in local {
        if let Some(status) = find_local_status_by_id(db, StatusId(favourite.status_id)).await? {
            entries.push((favourite.id, FavouriteStatus::Local(status)));
        }
    }
    for favourite in remote {
        if let Some(status) =
            find_remote_status_by_id(db, StatusId(favourite.remote_status_id)).await?
        {
            entries.push((favourite.id, FavouriteStatus::Remote(status)));
        }
    }
    entries.sort_by_key(|(id, _)| Reverse(*id));
    let (entries, has_more) = trim_to_page(entries, limit);
    Ok(CollectionPage {
        first_cursor: entries.first().map(|(id, _)| *id),
        last_cursor: entries.last().map(|(id, _)| *id),
        items: entries.into_iter().map(|(_, status)| status).collect(),
        has_more,
    })
}

/// Mark a local status as bookmarked by an account.
pub async fn bookmark_local_status(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    status_id: StatusId,
) -> Result<()> {
    if local_status_bookmark::Entity::find_by_id((account_id.0, status_id.0))
        .one(db)
        .await?
        .is_none()
    {
        local_status_bookmark::ActiveModel {
            id: Set(Uuid::now_v7()),
            account_id: Set(account_id.0),
            status_id: Set(status_id.0),
            created_at: Set(OffsetDateTime::now_utc()),
        }
        .insert(db)
        .await?;
    }

    Ok(())
}

/// Remove a local account's bookmark from a status when it exists.
pub async fn unbookmark_local_status(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    status_id: StatusId,
) -> Result<()> {
    if let Some(model) = local_status_bookmark::Entity::find_by_id((account_id.0, status_id.0))
        .one(db)
        .await?
    {
        model.into_active_model().delete(db).await?;
    }

    Ok(())
}

/// Return whether a local account has bookmarked a status.
pub async fn is_local_status_bookmarked(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    status_id: StatusId,
) -> Result<bool> {
    Ok(
        local_status_bookmark::Entity::find_by_id((account_id.0, status_id.0))
            .one(db)
            .await?
            .is_some(),
    )
}

/// List local statuses bookmarked by an account, newest bookmark first.
pub async fn local_bookmarks_for_account(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    limit: u64,
    cursor: CollectionCursor,
) -> Result<CollectionPage<LocalStatus>> {
    let rows = local_status_bookmark::Entity::find()
        .filter(local_status_bookmark::Column::AccountId.eq(account_id.0))
        .apply_collection_cursor(cursor)
        .order_by_desc(local_status_bookmark::Column::Id)
        .limit(page_query_limit(limit))
        .all(db)
        .await?;
    let (rows, has_more) = trim_to_page(rows, limit);
    let first_cursor = rows.first().map(|model| model.id);
    let last_cursor = rows.last().map(|model| model.id);
    let status_ids = rows
        .into_iter()
        .map(|model| StatusId(model.status_id))
        .collect::<Vec<_>>();

    Ok(CollectionPage {
        items: active_statuses_by_id(db, status_ids).await?,
        first_cursor,
        last_cursor,
        has_more,
    })
}

/// Mark a local status as boosted by an account.
pub async fn reblog_local_status(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    status_id: StatusId,
) -> Result<LocalStatusReblog> {
    if let Some(model) = local_status_reblog::Entity::find_by_id((account_id.0, status_id.0))
        .one(db)
        .await?
    {
        return Ok(local_status_reblog_from_model(model));
    }

    let model = local_status_reblog::ActiveModel {
        id: Set(Uuid::now_v7()),
        account_id: Set(account_id.0),
        status_id: Set(status_id.0),
        created_at: Set(OffsetDateTime::now_utc()),
    }
    .insert(db)
    .await?;
    adjust_status_trend(db, TrendTarget::LocalStatus(status_id), 0, 1).await?;
    let reblog = local_status_reblog_from_model(model);
    Ok(reblog)
}

/// Remove a local account's boost from a status when it exists.
pub async fn unreblog_local_status(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    status_id: StatusId,
) -> Result<Option<LocalStatusReblog>> {
    if let Some(model) = local_status_reblog::Entity::find_by_id((account_id.0, status_id.0))
        .one(db)
        .await?
    {
        let reblog = local_status_reblog_from_model(model.clone());
        model.into_active_model().delete(db).await?;
        adjust_status_trend(db, TrendTarget::LocalStatus(status_id), 0, -1).await?;
        return Ok(Some(reblog));
    }
    Ok(None)
}

/// Store a local account's Announce of a cached remote Note.
pub async fn reblog_remote_status(
    db: &impl ConnectionTrait,
    local_account_id: AccountId,
    remote_status_id: StatusId,
    activity_id: &str,
) -> Result<LocalRemoteStatusReblog> {
    if let Some(model) = local_remote_status_reblog::Entity::find()
        .filter(local_remote_status_reblog::Column::LocalAccountId.eq(local_account_id.0))
        .filter(local_remote_status_reblog::Column::RemoteStatusId.eq(remote_status_id.0))
        .one(db)
        .await?
    {
        return Ok(local_remote_status_reblog_from_model(model));
    }
    let model = local_remote_status_reblog::ActiveModel {
        id: Set(Uuid::now_v7()),
        local_account_id: Set(local_account_id.0),
        remote_status_id: Set(remote_status_id.0),
        activity_id: Set(activity_id.to_owned()),
        created_at: Set(OffsetDateTime::now_utc()),
    }
    .insert(db)
    .await?;
    adjust_status_trend(db, TrendTarget::RemoteStatus(remote_status_id), 0, 1).await?;
    let reblog = local_remote_status_reblog_from_model(model);
    Ok(reblog)
}

/// Store a remote-status boost and its Announce delivery job in `txn`.
pub async fn reblog_remote_status_with_job(
    txn: &impl ConnectionTrait,
    local_account_id: AccountId,
    remote_status_id: StatusId,
    activity_id: &str,
    job: NewJob,
) -> Result<LocalRemoteStatusReblog> {
    let existing = local_remote_status_reblog::Entity::find()
        .filter(local_remote_status_reblog::Column::LocalAccountId.eq(local_account_id.0))
        .filter(local_remote_status_reblog::Column::RemoteStatusId.eq(remote_status_id.0))
        .one(txn)
        .await?;
    let model = match existing {
        Some(model) => model,
        None => {
            let model = local_remote_status_reblog::ActiveModel {
                id: Set(Uuid::now_v7()),
                local_account_id: Set(local_account_id.0),
                remote_status_id: Set(remote_status_id.0),
                activity_id: Set(activity_id.to_owned()),
                created_at: Set(OffsetDateTime::now_utc()),
            }
            .insert(txn)
            .await?;
            adjust_status_trend(txn, TrendTarget::RemoteStatus(remote_status_id), 0, 1).await?;
            model
        }
    };
    enqueue_job_in_transaction(txn, job).await?;
    Ok(local_remote_status_reblog_from_model(model))
}

/// Remove a local account's Announce of a cached remote Note.
pub async fn unreblog_remote_status(
    db: &DbConnection,
    local_account_id: AccountId,
    remote_status_id: StatusId,
) -> Result<Option<LocalRemoteStatusReblog>> {
    let txn = db.begin().await?;
    let model = local_remote_status_reblog::Entity::find()
        .filter(local_remote_status_reblog::Column::LocalAccountId.eq(local_account_id.0))
        .filter(local_remote_status_reblog::Column::RemoteStatusId.eq(remote_status_id.0))
        .one(db)
        .await?;
    let Some(model) = model else {
        txn.commit().await?;
        return Ok(None);
    };
    let reblog = local_remote_status_reblog_from_model(model.clone());
    model.into_active_model().delete(db).await?;
    adjust_status_trend(db, TrendTarget::RemoteStatus(remote_status_id), 0, -1).await?;
    txn.commit().await?;
    Ok(Some(reblog))
}

/// Find a local Announce of a cached remote Note without changing it.
pub async fn find_remote_status_reblog(
    db: &impl ConnectionTrait,
    local_account_id: AccountId,
    remote_status_id: StatusId,
) -> Result<Option<LocalRemoteStatusReblog>> {
    Ok(local_remote_status_reblog::Entity::find()
        .filter(local_remote_status_reblog::Column::LocalAccountId.eq(local_account_id.0))
        .filter(local_remote_status_reblog::Column::RemoteStatusId.eq(remote_status_id.0))
        .one(db)
        .await?
        .map(local_remote_status_reblog_from_model))
}

/// Remove a remote-status boost and insert its Undo delivery job in `txn`.
pub async fn unreblog_remote_status_with_job(
    txn: &impl ConnectionTrait,
    local_account_id: AccountId,
    remote_status_id: StatusId,
    job: NewJob,
) -> Result<Option<LocalRemoteStatusReblog>> {
    let model = local_remote_status_reblog::Entity::find()
        .filter(local_remote_status_reblog::Column::LocalAccountId.eq(local_account_id.0))
        .filter(local_remote_status_reblog::Column::RemoteStatusId.eq(remote_status_id.0))
        .one(txn)
        .await?;
    let Some(model) = model else {
        return Ok(None);
    };
    let reblog = local_remote_status_reblog_from_model(model.clone());
    model.into_active_model().delete(txn).await?;
    adjust_status_trend(txn, TrendTarget::RemoteStatus(remote_status_id), 0, -1).await?;
    enqueue_job_in_transaction(txn, job).await?;
    Ok(Some(reblog))
}

/// Return whether a local account announced a cached remote Note.
pub async fn is_remote_status_reblogged(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    status_id: StatusId,
) -> Result<bool> {
    Ok(local_remote_status_reblog::Entity::find()
        .filter(local_remote_status_reblog::Column::LocalAccountId.eq(account_id.0))
        .filter(local_remote_status_reblog::Column::RemoteStatusId.eq(status_id.0))
        .one(db)
        .await?
        .is_some())
}

/// Store a validated inbound Announce, returning false when it already exists.
pub async fn reblog_status_by_remote_actor(
    db: &impl ConnectionTrait,
    remote_actor_id: AccountId,
    target: RemoteStatusReblogTarget,
    activity_id: &str,
) -> Result<bool> {
    let (local_status_id, remote_status_id) = match target {
        RemoteStatusReblogTarget::Local(id) => (Some(id.0), None),
        RemoteStatusReblogTarget::Remote(id) => (None, Some(id.0)),
    };
    if remote_status_reblog::Entity::find()
        .filter(remote_status_reblog::Column::RemoteActorId.eq(remote_actor_id.0))
        .filter(remote_status_reblog::Column::LocalStatusId.eq(local_status_id))
        .filter(remote_status_reblog::Column::RemoteStatusId.eq(remote_status_id))
        .one(db)
        .await?
        .is_some()
    {
        return Ok(false);
    }
    remote_status_reblog::ActiveModel {
        id: Set(Uuid::now_v7()),
        remote_actor_id: Set(remote_actor_id.0),
        local_status_id: Set(local_status_id),
        remote_status_id: Set(remote_status_id),
        activity_id: Set(activity_id.to_owned()),
        created_at: Set(OffsetDateTime::now_utc()),
    }
    .insert(db)
    .await?;
    let trend_target = match target {
        RemoteStatusReblogTarget::Local(id) => TrendTarget::LocalStatus(id),
        RemoteStatusReblogTarget::Remote(id) => TrendTarget::RemoteStatus(id),
    };
    adjust_status_trend(db, trend_target, 0, 1).await?;
    Ok(true)
}

/// Remove a remote Announce by its canonical activity identity.
pub async fn unreblog_status_by_remote_actor(
    db: &DbConnection,
    remote_actor_id: AccountId,
    activity_id: &str,
) -> Result<Option<RemoteStatusReblog>> {
    let txn = db.begin().await?;
    let model = remote_status_reblog::Entity::find()
        .filter(remote_status_reblog::Column::RemoteActorId.eq(remote_actor_id.0))
        .filter(remote_status_reblog::Column::ActivityId.eq(activity_id))
        .one(db)
        .await?;
    let Some(model) = model else {
        txn.commit().await?;
        return Ok(None);
    };
    let Some(reblog) = remote_status_reblog_from_model(model.clone()) else {
        txn.commit().await?;
        return Ok(None);
    };
    model.into_active_model().delete(db).await?;
    let target = match reblog.target {
        RemoteStatusReblogTarget::Local(id) => TrendTarget::LocalStatus(id),
        RemoteStatusReblogTarget::Remote(id) => TrendTarget::RemoteStatus(id),
    };
    adjust_status_trend(db, target, 0, -1).await?;
    txn.commit().await?;
    Ok(Some(reblog))
}

/// Record an inbound Undo(Announce) and remove its original Announce atomically.
pub async fn process_remote_undo_reblog(
    txn: &sea_orm::DatabaseTransaction,
    remote_actor_id: AccountId,
    original_activity_id: &str,
) -> Result<Option<RemoteStatusReblog>> {
    let model = remote_status_reblog::Entity::find()
        .filter(remote_status_reblog::Column::RemoteActorId.eq(remote_actor_id.0))
        .filter(remote_status_reblog::Column::ActivityId.eq(original_activity_id))
        .one(txn)
        .await?;
    let reblog = model.clone().and_then(remote_status_reblog_from_model);
    if let Some(model) = model {
        model.into_active_model().delete(txn).await?;
    }
    if let Some(reblog) = &reblog {
        let target = match reblog.target {
            RemoteStatusReblogTarget::Local(id) => TrendTarget::LocalStatus(id),
            RemoteStatusReblogTarget::Remote(id) => TrendTarget::RemoteStatus(id),
        };
        adjust_status_trend(txn, target, 0, -1).await?;
    }
    Ok(reblog)
}

/// Find a remote Announce by actor and ActivityPub identity.
pub async fn find_remote_status_reblog_by_activity_id(
    db: &impl ConnectionTrait,
    remote_actor_id: AccountId,
    activity_id: &str,
) -> Result<Option<RemoteStatusReblog>> {
    Ok(remote_status_reblog::Entity::find()
        .filter(remote_status_reblog::Column::RemoteActorId.eq(remote_actor_id.0))
        .filter(remote_status_reblog::Column::ActivityId.eq(activity_id))
        .one(db)
        .await?
        .and_then(remote_status_reblog_from_model))
}

/// Count active local boosts on a status.
pub async fn count_local_reblogs(db: &impl ConnectionTrait, status_id: StatusId) -> Result<u64> {
    let local = local_status_reblog::Entity::find()
        .filter(local_status_reblog::Column::StatusId.eq(status_id.0))
        .count(db)
        .await?;
    let remote = remote_status_reblog::Entity::find()
        .filter(remote_status_reblog::Column::LocalStatusId.eq(Some(status_id.0)))
        .count(db)
        .await?;
    Ok(local + remote)
}

/// Return whether a local account has boosted a status.
pub async fn is_local_status_reblogged(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    status_id: StatusId,
) -> Result<bool> {
    Ok(
        local_status_reblog::Entity::find_by_id((account_id.0, status_id.0))
            .one(db)
            .await?
            .is_some(),
    )
}

/// List local accounts that boosted a status, newest boost first.
pub async fn local_reblogged_by_for_status(
    db: &DbConnection,
    status_id: StatusId,
    limit: u64,
    cursor: CollectionCursor,
) -> Result<CollectionPage<LocalAccount>> {
    let rows = local_status_reblog::Entity::find()
        .filter(local_status_reblog::Column::StatusId.eq(status_id.0))
        .apply_collection_cursor(cursor)
        .order_by_desc(local_status_reblog::Column::Id)
        .limit(page_query_limit(limit))
        .all(db)
        .await?;
    let (rows, has_more) = trim_to_page(rows, limit);
    let first_cursor = rows.first().map(|model| model.id);
    let last_cursor = rows.last().map(|model| model.id);
    let account_ids = rows
        .into_iter()
        .map(|model| AccountId(model.account_id))
        .collect::<Vec<_>>();

    Ok(CollectionPage {
        items: local_accounts_by_id(db, account_ids).await?,
        first_cursor,
        last_cursor,
        has_more,
    })
}

/// List locally known accounts that favourited a local or cached-remote status, newest first.
pub async fn favourited_by_for_status(
    db: &impl ConnectionTrait,
    target: StatusInteractionTarget,
    limit: u64,
    cursor: CollectionCursor,
) -> Result<CollectionPage<StatusInteractionAccount>> {
    let mut entries = Vec::new();
    match target {
        StatusInteractionTarget::Local(status_id) => {
            let local = local_status_favourite::Entity::find()
                .filter(local_status_favourite::Column::StatusId.eq(status_id.0))
                .apply_collection_cursor(cursor)
                .order_by_desc(local_status_favourite::Column::Id)
                .limit(page_query_limit(limit))
                .all(db)
                .await?;
            let remote = remote_status_favourite::Entity::find()
                .filter(remote_status_favourite::Column::LocalStatusId.eq(status_id.0))
                .apply_collection_cursor(cursor)
                .order_by_desc(remote_status_favourite::Column::Id)
                .limit(page_query_limit(limit))
                .all(db)
                .await?;
            entries.extend(local.into_iter().map(|favourite| {
                (
                    favourite.id,
                    InteractionActorId::Local(AccountId(favourite.account_id)),
                )
            }));
            entries.extend(remote.into_iter().map(|favourite| {
                (
                    favourite.id,
                    InteractionActorId::Remote(AccountId(favourite.remote_actor_id)),
                )
            }));
        }
        StatusInteractionTarget::Remote(status_id) => {
            let local = local_remote_status_favourite::Entity::find()
                .filter(local_remote_status_favourite::Column::RemoteStatusId.eq(status_id.0))
                .apply_collection_cursor(cursor)
                .order_by_desc(local_remote_status_favourite::Column::Id)
                .limit(page_query_limit(limit))
                .all(db)
                .await?;
            entries.extend(local.into_iter().map(|favourite| {
                (
                    favourite.id,
                    InteractionActorId::Local(AccountId(favourite.local_account_id)),
                )
            }));
        }
    }
    let page = interaction_accounts_page(db, entries, limit).await?;
    Ok(page)
}

/// List locally known accounts that boosted a local or cached-remote status, newest first.
pub async fn reblogged_by_for_status(
    db: &impl ConnectionTrait,
    target: StatusInteractionTarget,
    limit: u64,
    cursor: CollectionCursor,
) -> Result<CollectionPage<StatusInteractionAccount>> {
    let mut entries = Vec::new();
    match target {
        StatusInteractionTarget::Local(status_id) => {
            let local = local_status_reblog::Entity::find()
                .filter(local_status_reblog::Column::StatusId.eq(status_id.0))
                .apply_collection_cursor(cursor)
                .order_by_desc(local_status_reblog::Column::Id)
                .limit(page_query_limit(limit))
                .all(db)
                .await?;
            let remote = remote_status_reblog::Entity::find()
                .filter(remote_status_reblog::Column::LocalStatusId.eq(Some(status_id.0)))
                .apply_collection_cursor(cursor)
                .order_by_desc(remote_status_reblog::Column::Id)
                .limit(page_query_limit(limit))
                .all(db)
                .await?;
            entries.extend(local.into_iter().map(|reblog| {
                (
                    reblog.id,
                    InteractionActorId::Local(AccountId(reblog.account_id)),
                )
            }));
            entries.extend(remote.into_iter().map(|reblog| {
                (
                    reblog.id,
                    InteractionActorId::Remote(AccountId(reblog.remote_actor_id)),
                )
            }));
        }
        StatusInteractionTarget::Remote(status_id) => {
            let local = local_remote_status_reblog::Entity::find()
                .filter(local_remote_status_reblog::Column::RemoteStatusId.eq(status_id.0))
                .apply_collection_cursor(cursor)
                .order_by_desc(local_remote_status_reblog::Column::Id)
                .limit(page_query_limit(limit))
                .all(db)
                .await?;
            let remote = remote_status_reblog::Entity::find()
                .filter(remote_status_reblog::Column::RemoteStatusId.eq(Some(status_id.0)))
                .apply_collection_cursor(cursor)
                .order_by_desc(remote_status_reblog::Column::Id)
                .limit(page_query_limit(limit))
                .all(db)
                .await?;
            entries.extend(local.into_iter().map(|reblog| {
                (
                    reblog.id,
                    InteractionActorId::Local(AccountId(reblog.local_account_id)),
                )
            }));
            entries.extend(remote.into_iter().map(|reblog| {
                (
                    reblog.id,
                    InteractionActorId::Remote(AccountId(reblog.remote_actor_id)),
                )
            }));
        }
    }
    let page = interaction_accounts_page(db, entries, limit).await?;
    Ok(page)
}

#[derive(Clone, Copy, Debug)]
enum InteractionActorId {
    Local(AccountId),
    Remote(AccountId),
}

async fn interaction_accounts_page(
    db: &impl ConnectionTrait,
    mut entries: Vec<(Uuid, InteractionActorId)>,
    limit: u64,
) -> Result<CollectionPage<StatusInteractionAccount>> {
    entries.sort_by_key(|(id, _)| Reverse(*id));
    let (entries, has_more) = trim_to_page(entries, limit);
    let first_cursor = entries.first().map(|(id, _)| *id);
    let last_cursor = entries.last().map(|(id, _)| *id);
    let local_ids = entries
        .iter()
        .filter_map(|(_, actor)| match actor {
            InteractionActorId::Local(id) => Some(*id),
            InteractionActorId::Remote(_) => None,
        })
        .collect::<Vec<_>>();
    let remote_ids = entries
        .iter()
        .filter_map(|(_, actor)| match actor {
            InteractionActorId::Local(_) => None,
            InteractionActorId::Remote(id) => Some(*id),
        })
        .collect::<Vec<_>>();
    let local = local_accounts_by_id(db, local_ids).await?;
    let remote = remote_actors_by_id(db, remote_ids).await?;
    let mut local = local
        .into_iter()
        .map(|account| (account.id, account))
        .collect::<HashMap<_, _>>();
    let mut remote = remote
        .into_iter()
        .map(|actor| (actor.id, actor))
        .collect::<HashMap<_, _>>();
    let items = entries
        .into_iter()
        .filter_map(|(_, actor)| match actor {
            InteractionActorId::Local(id) => local.remove(&id).map(StatusInteractionAccount::Local),
            InteractionActorId::Remote(id) => {
                remote.remove(&id).map(StatusInteractionAccount::Remote)
            }
        })
        .collect();
    Ok(CollectionPage {
        items,
        first_cursor,
        last_cursor,
        has_more,
    })
}

/// List local boost rows for an original status.
pub async fn local_reblogs_for_status(
    db: &impl ConnectionTrait,
    status_id: StatusId,
) -> Result<Vec<LocalStatusReblog>> {
    let reblogs = local_status_reblog::Entity::find()
        .filter(local_status_reblog::Column::StatusId.eq(status_id.0))
        .all(db)
        .await?;

    Ok(reblogs
        .into_iter()
        .map(local_status_reblog_from_model)
        .collect())
}

/// Find one local boost by its opaque id.
pub async fn find_local_reblog_by_id(
    db: &DbConnection,
    reblog_id: Uuid,
) -> Result<Option<LocalStatusReblog>> {
    let reblog = local_status_reblog::Entity::find()
        .filter(local_status_reblog::Column::Id.eq(reblog_id))
        .one(db)
        .await?;

    Ok(reblog.map(local_status_reblog_from_model))
}

/// Load active local statuses for ordered status identifiers.
async fn active_statuses_by_id(
    db: &impl ConnectionTrait,
    status_ids: Vec<StatusId>,
) -> Result<Vec<LocalStatus>> {
    let mut statuses = Vec::with_capacity(status_ids.len());
    for status_id in status_ids {
        if let Some(status) = find_local_status_by_id(db, status_id).await? {
            statuses.push(status);
        }
    }

    Ok(statuses)
}

/// Return local accounts in the same order as the provided ids.
async fn local_accounts_by_id(
    db: &impl ConnectionTrait,
    account_ids: Vec<AccountId>,
) -> Result<Vec<LocalAccount>> {
    if account_ids.is_empty() {
        return Ok(Vec::new());
    }

    let models = local_account::Entity::find()
        .filter(local_account::Column::Id.is_in(account_ids.iter().map(|id| id.0)))
        .all(db)
        .await?;
    let mut accounts_by_id = models
        .into_iter()
        .map(|model| (model.id, model))
        .collect::<HashMap<_, _>>();
    let mut accounts = Vec::with_capacity(account_ids.len());
    for account_id in account_ids {
        if let Some(model) = accounts_by_id.remove(&account_id.0) {
            accounts.push(local_account_from_model(model)?);
        }
    }

    Ok(accounts)
}

/// Return remote actors in the same order as the provided ids.
pub async fn remote_actors_by_id(
    db: &impl ConnectionTrait,
    actor_ids: Vec<AccountId>,
) -> Result<Vec<RemoteActor>> {
    if actor_ids.is_empty() {
        return Ok(Vec::new());
    }

    let models = remote_actor::Entity::find()
        .filter(remote_actor::Column::Id.is_in(actor_ids.iter().map(|id| id.0)))
        .all(db)
        .await?;
    let mut actors_by_id = models
        .into_iter()
        .map(|model| (model.id, model))
        .collect::<HashMap<_, _>>();
    let mut actors = Vec::with_capacity(actor_ids.len());
    for actor_id in actor_ids {
        if let Some(model) = actors_by_id.remove(&actor_id.0) {
            actors.push(remote_actor_from_model(model));
        }
    }

    Ok(actors)
}

/// Soft-delete a local status when the authenticated account owns it.
pub async fn delete_owned_local_status(
    db: &impl ConnectionTrait,
    status_id: StatusId,
    account_id: AccountId,
) -> Result<Option<LocalStatus>> {
    let Some(status) = local_status::Entity::find_by_id(status_id.0)
        .filter(local_status::Column::DeletedAt.is_null())
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    if status.account_id != account_id.0 {
        return Err(RoostyError::InvalidInput(
            "status is owned by another account".to_owned(),
        ));
    }

    remove_status_preview_card(db, PreviewStatusTarget::Local(status_id)).await?;
    let tag_ids = local_status_tag::Entity::find()
        .filter(local_status_tag::Column::StatusId.eq(status_id.0))
        .all(db)
        .await?
        .into_iter()
        .map(|row| row.tag_id)
        .collect::<Vec<_>>();
    if status.visibility == StatusVisibility::Public {
        adjust_tag_usage(
            db,
            &tag_ids,
            utc_date(status.created_at),
            "local",
            status.account_id,
            -1,
        )
        .await?;
    }
    let mut active = status.into_active_model();
    active.deleted_at = Set(Some(OffsetDateTime::now_utc()));
    active.updated_at = Set(OffsetDateTime::now_utc());
    let status = local_status_from_model(active.update(db).await?)?;
    refresh_status_search_document(db, StatusReference::Local(status_id)).await?;
    refresh_local_account_last_status_at(db, account_id).await?;
    local_status_pin::Entity::delete_many()
        .filter(local_status_pin::Column::StatusId.eq(status_id.0))
        .exec(db)
        .await?;
    mark_trend_dirty(db, "local_status", status_id.0).await?;
    Ok(Some(status))
}

/// List local and cached remote statuses for the public timeline.
pub async fn public_timeline(
    db: &DbConnection,
    limit: u64,
    cursor: TimelineCursor,
) -> Result<TimelinePage<PublicTimelineItem>> {
    public_timeline_with_options(db, limit, cursor, PublicTimelineOptions::default()).await
}

/// List public statuses with Mastodon-compatible origin and media filters.
pub async fn public_timeline_with_options(
    db: &impl ConnectionTrait,
    limit: u64,
    cursor: TimelineCursor,
    options: PublicTimelineOptions,
) -> Result<TimelinePage<PublicTimelineItem>> {
    let hidden_local_ids = if let Some(viewer) = options.viewer {
        hidden_local_account_ids_for_account(db, viewer)
            .await?
            .into_iter()
            .map(|id| id.0)
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let hidden_remote_ids = if let Some(viewer) = options.viewer {
        hidden_remote_actor_ids_for_account(db, viewer)
            .await?
            .into_iter()
            .map(|id| id.0)
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    let mut items = Vec::new();
    if options.origin != PublicTimelineOrigin::Remote {
        let mut query = apply_timeline_cursor(
            local_status::Entity::find()
                .filter(local_status::Column::Visibility.eq(StatusVisibility::Public))
                .filter(local_status::Column::DeletedAt.is_null())
                .filter(local_status::Column::InReplyToId.is_null())
                .filter(local_status::Column::InReplyToRemoteStatusId.is_null()),
            cursor,
        );
        query = query.filter(
            local_status::Column::AccountId.in_subquery(
                Query::select()
                    .column(local_account::Column::Id)
                    .from(local_account::Entity)
                    .and_where(local_account::Column::LimitedAt.is_null())
                    .and_where(local_account::Column::SuspendedAt.is_null())
                    .to_owned(),
            ),
        );
        if !hidden_local_ids.is_empty() {
            query = query.filter(local_status::Column::AccountId.is_not_in(hidden_local_ids));
        }
        if options.only_media {
            query = query.filter(
                local_status::Column::Id.in_subquery(
                    Query::select()
                        .column(local_media_attachment::Column::StatusId)
                        .from(local_media_attachment::Entity)
                        .to_owned(),
                ),
            );
        }
        let local = query
            .order_by_desc(local_status::Column::Id)
            .limit(page_query_limit(limit))
            .all(db)
            .await?;
        items.extend(
            local
                .into_iter()
                .map(|model| local_status_from_model(model).map(PublicTimelineItem::Local))
                .collect::<Result<Vec<_>>>()?,
        );
    }

    if options.origin != PublicTimelineOrigin::Local && !options.allowed_remote_domains.is_empty() {
        let mut actor_condition = Condition::all()
            .add(remote_actor::Column::DeletedAt.is_null())
            .add(remote_actor::Column::LimitedAt.is_null())
            .add(remote_actor::Column::SuspendedAt.is_null());
        if !options
            .allowed_remote_domains
            .iter()
            .any(|domain| domain == "*")
        {
            actor_condition = actor_condition
                .add(remote_actor::Column::Domain.is_in(options.allowed_remote_domains.clone()));
        }
        for domain in &options.blocked_remote_domains {
            actor_condition = actor_condition
                .add(remote_actor::Column::Domain.ne(domain.clone()))
                .add(remote_actor::Column::Domain.not_like(format!("%.{domain}")));
        }
        let allowed_actors = Query::select()
            .column(remote_actor::Column::Id)
            .from(remote_actor::Entity)
            .and_where(actor_condition.into())
            .to_owned();
        let mut query = remote_status::Entity::find()
            .filter(remote_status::Column::Visibility.eq(StatusVisibility::Public))
            .filter(remote_status::Column::DeletedAt.is_null())
            .filter(remote_status::Column::InReplyTo.is_null())
            .filter(remote_status::Column::RemoteActorId.in_subquery(allowed_actors));
        if !hidden_remote_ids.is_empty() {
            query = query.filter(remote_status::Column::RemoteActorId.is_not_in(hidden_remote_ids));
        }
        if let Some(max_id) = cursor.max_id {
            query = query.filter(remote_status::Column::Id.lt(max_id.0));
        }
        if let Some(since_id) = cursor.since_id {
            query = query.filter(remote_status::Column::Id.gt(since_id.0));
        }
        if let Some(min_id) = cursor.min_id {
            query = query.filter(remote_status::Column::Id.gt(min_id.0));
        }
        if options.only_media {
            query = query.filter(
                remote_status::Column::Id.in_subquery(
                    Query::select()
                        .column(remote_media_attachment::Column::RemoteStatusId)
                        .from(remote_media_attachment::Entity)
                        .to_owned(),
                ),
            );
        }
        let remote = query
            .order_by_desc(remote_status::Column::Id)
            .limit(page_query_limit(limit))
            .all(db)
            .await?;
        items.extend(
            remote
                .into_iter()
                .map(|model| remote_status_from_model(model).map(PublicTimelineItem::Remote))
                .collect::<Result<Vec<_>>>()?,
        );
    }

    items.sort_by_key(|item| {
        Reverse(match item {
            PublicTimelineItem::Local(status) => status.id.0,
            PublicTimelineItem::Remote(status) => status.id.0,
        })
    });
    let (items, has_more) = trim_to_page(items, limit);
    let item_id = |item: &PublicTimelineItem| match item {
        PublicTimelineItem::Local(status) => status.id.0,
        PublicTimelineItem::Remote(status) => status.id.0,
    };
    let first_cursor = items.first().map(item_id);
    let last_cursor = items.last().map(item_id);

    Ok(TimelinePage {
        items,
        first_cursor,
        last_cursor,
        has_more,
    })
}

/// List statuses visible on an account's profile timeline.
pub async fn local_statuses_by_account(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    viewer: Option<AccountId>,
    limit: u64,
    cursor: TimelineCursor,
    options: AccountStatusTimelineOptions,
) -> Result<TimelinePage<LocalStatus>> {
    let owner = viewer.is_some_and(|viewer| viewer == account_id);
    let mut query = local_status::Entity::find()
        .filter(local_status::Column::AccountId.eq(account_id.0))
        .filter(local_status::Column::DeletedAt.is_null());
    if !owner {
        let mut visible =
            Condition::any().add(local_status::Column::Visibility.is_in(["public", "unlisted"]));
        if let Some(viewer) = viewer {
            visible = visible.add(
                Condition::all()
                    .add(local_status::Column::Visibility.eq(StatusVisibility::Private))
                    .add(
                        Condition::any()
                            .add(
                                local_status::Column::Id.in_subquery(
                                    Query::select()
                                        .column(local_status_local_recipient::Column::StatusId)
                                        .from(local_status_local_recipient::Entity)
                                        .and_where(
                                            local_status_local_recipient::Column::AccountId
                                                .eq(viewer.0),
                                        )
                                        .to_owned(),
                                ),
                            )
                            .add(
                                local_status::Column::AccountId.in_subquery(
                                    Query::select()
                                        .column(local_follow::Column::FollowedAccountId)
                                        .from(local_follow::Entity)
                                        .and_where(
                                            local_follow::Column::FollowerAccountId.eq(viewer.0),
                                        )
                                        .to_owned(),
                                ),
                            ),
                    ),
            );
            visible = visible.add(
                Condition::all()
                    .add(local_status::Column::Visibility.eq(StatusVisibility::Direct))
                    .add(
                        local_status::Column::Id.in_subquery(
                            Query::select()
                                .column(local_status_local_recipient::Column::StatusId)
                                .from(local_status_local_recipient::Entity)
                                .and_where(
                                    local_status_local_recipient::Column::AccountId.eq(viewer.0),
                                )
                                .to_owned(),
                        ),
                    ),
            );
        }
        query = query.filter(visible);
    }
    if options.exclude_replies {
        query = query
            .filter(local_status::Column::InReplyToId.is_null())
            .filter(local_status::Column::InReplyToRemoteStatusId.is_null());
    }
    if options.only_media {
        query = query.filter(local_status::Column::Id.in_subquery(media_status_subquery()));
    }
    if let Some(tag) = options.tagged.as_deref() {
        let Some(tag) = find_local_tag_by_name(db, tag).await? else {
            return Ok(TimelinePage {
                items: Vec::new(),
                first_cursor: None,
                last_cursor: None,
                has_more: false,
            });
        };
        query = query.filter(local_status::Column::Id.in_subquery(status_tag_subquery(tag.id)));
    }

    let statuses = apply_timeline_cursor(query, cursor)
        .order_by_desc(local_status::Column::Id)
        .limit(page_query_limit(limit))
        .all(db)
        .await?;
    let (statuses, has_more) = trim_to_page(statuses, limit);
    let first_cursor = statuses.first().map(|status| status.id);
    let last_cursor = statuses.last().map(|status| status.id);

    Ok(TimelinePage {
        items: statuses
            .into_iter()
            .map(local_status_from_model)
            .collect::<Result<_>>()?,
        first_cursor,
        last_cursor,
        has_more,
    })
}

/// List the locally cached public or unlisted statuses on a remote actor profile.
pub async fn remote_statuses_by_account(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    viewer: Option<AccountId>,
    limit: u64,
    cursor: TimelineCursor,
    options: AccountStatusTimelineOptions,
) -> Result<TimelinePage<RemoteStatus>> {
    let mut query = remote_status::Entity::find()
        .filter(remote_status::Column::RemoteActorId.eq(account_id.0))
        .filter(remote_status::Column::DeletedAt.is_null());
    let mut visible =
        Condition::any().add(remote_status::Column::Visibility.is_in(["public", "unlisted"]));
    if let Some(viewer) = viewer {
        visible = visible.add(
            Condition::all()
                .add(remote_status::Column::Visibility.eq(StatusVisibility::Private))
                .add(
                    Condition::any()
                        .add(
                            remote_status::Column::Id.in_subquery(
                                Query::select()
                                    .column(remote_status_local_recipient::Column::RemoteStatusId)
                                    .from(remote_status_local_recipient::Entity)
                                    .and_where(
                                        remote_status_local_recipient::Column::AccountId
                                            .eq(viewer.0),
                                    )
                                    .to_owned(),
                            ),
                        )
                        .add(
                            remote_status::Column::RemoteActorId.in_subquery(
                                Query::select()
                                    .column(remote_following::Column::RemoteActorId)
                                    .from(remote_following::Entity)
                                    .and_where(
                                        remote_following::Column::LocalAccountId.eq(viewer.0),
                                    )
                                    .and_where(
                                        remote_following::Column::State
                                            .eq(RemoteFollowState::Accepted),
                                    )
                                    .and_where(remote_following::Column::DeactivatedAt.is_null())
                                    .to_owned(),
                            ),
                        ),
                ),
        );
    }
    query = query.filter(visible);
    if let Some(max_id) = cursor.max_id {
        query = query.filter(remote_status::Column::Id.lt(max_id.0));
    }
    if let Some(since_id) = cursor.since_id {
        query = query.filter(remote_status::Column::Id.gt(since_id.0));
    }
    if let Some(min_id) = cursor.min_id {
        query = query.filter(remote_status::Column::Id.gt(min_id.0));
    }
    if options.exclude_replies {
        query = query.filter(remote_status::Column::InReplyTo.is_null());
    }
    if options.only_media {
        query = query.filter(
            remote_status::Column::Id.in_subquery(
                Query::select()
                    .column(remote_media_attachment::Column::RemoteStatusId)
                    .from(remote_media_attachment::Entity)
                    .to_owned(),
            ),
        );
    }
    if let Some(tag) = options.tagged {
        let Some(tag) = find_local_tag_by_name(db, &tag).await? else {
            return Ok(TimelinePage {
                items: Vec::new(),
                first_cursor: None,
                last_cursor: None,
                has_more: false,
            });
        };
        query =
            query.filter(remote_status::Column::Id.in_subquery(remote_status_tag_subquery(tag.id)));
    }
    let mut statuses = query
        .order_by_desc(remote_status::Column::Id)
        .limit(page_query_limit(limit))
        .all(db)
        .await?
        .into_iter()
        .map(remote_status_from_model)
        .collect::<Result<Vec<_>>>()?;
    let has_more = statuses.len() > limit as usize;
    if has_more {
        statuses.truncate(limit as usize);
    }
    let first_cursor = statuses.first().map(|status| status.id.0);
    let last_cursor = statuses.last().map(|status| status.id.0);
    Ok(TimelinePage {
        items: statuses,
        first_cursor,
        last_cursor,
        has_more,
    })
}

fn local_list_from_model(model: local_list::Model) -> LocalList {
    LocalList {
        id: model.id,
        account_id: AccountId(model.account_id),
        title: model.title,
        replies_policy: model.replies_policy,
        exclusive: model.exclusive,
        created_at: model.created_at,
        updated_at: model.updated_at,
    }
}

async fn remove_local_account_from_owned_lists(
    db: &impl ConnectionTrait,
    owner_id: AccountId,
    member_id: AccountId,
) -> Result<()> {
    local_list_local_member::Entity::delete_many()
        .filter(local_list_local_member::Column::AccountId.eq(member_id.0))
        .filter(
            local_list_local_member::Column::ListId.in_subquery(
                Query::select()
                    .column(local_list::Column::Id)
                    .from(local_list::Entity)
                    .and_where(local_list::Column::AccountId.eq(owner_id.0))
                    .to_owned(),
            ),
        )
        .exec(db)
        .await?;
    Ok(())
}

async fn remove_remote_account_from_owned_lists(
    db: &impl ConnectionTrait,
    owner_id: AccountId,
    member_id: AccountId,
) -> Result<()> {
    local_list_remote_member::Entity::delete_many()
        .filter(local_list_remote_member::Column::RemoteActorId.eq(member_id.0))
        .filter(
            local_list_remote_member::Column::ListId.in_subquery(
                Query::select()
                    .column(local_list::Column::Id)
                    .from(local_list::Entity)
                    .and_where(local_list::Column::AccountId.eq(owner_id.0))
                    .to_owned(),
            ),
        )
        .exec(db)
        .await?;
    Ok(())
}

/// List every private list owned by an account in creation order.
pub async fn local_lists_for_account(
    db: &impl ConnectionTrait,
    account_id: AccountId,
) -> Result<Vec<LocalList>> {
    Ok(local_list::Entity::find()
        .filter(local_list::Column::AccountId.eq(account_id.0))
        .order_by_asc(local_list::Column::CreatedAt)
        .order_by_asc(local_list::Column::Id)
        .all(db)
        .await?
        .into_iter()
        .map(local_list_from_model)
        .collect())
}

/// Find a private list only when it belongs to the supplied account.
pub async fn find_owned_local_list(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    list_id: Uuid,
) -> Result<Option<LocalList>> {
    Ok(local_list::Entity::find_by_id(list_id)
        .filter(local_list::Column::AccountId.eq(account_id.0))
        .one(db)
        .await?
        .map(local_list_from_model))
}

/// Create a private list.
pub async fn create_local_list(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    title: &str,
    replies_policy: ListRepliesPolicy,
    exclusive: bool,
) -> Result<LocalList> {
    let model = local_list::ActiveModel {
        id: Set(Uuid::now_v7()),
        account_id: Set(account_id.0),
        title: Set(title.to_owned()),
        replies_policy: Set(replies_policy),
        exclusive: Set(exclusive),
        ..Default::default()
    }
    .insert(db)
    .await?;
    Ok(local_list_from_model(model))
}

/// Update an owned private list, returning `None` when ownership does not match.
pub async fn update_local_list(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    list_id: Uuid,
    title: &str,
    replies_policy: ListRepliesPolicy,
    exclusive: bool,
) -> Result<Option<LocalList>> {
    let Some(model) = local_list::Entity::find_by_id(list_id)
        .filter(local_list::Column::AccountId.eq(account_id.0))
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    let mut active = model.into_active_model();
    active.title = Set(title.to_owned());
    active.replies_policy = Set(replies_policy);
    active.exclusive = Set(exclusive);
    active.updated_at = Set(OffsetDateTime::now_utc());
    Ok(Some(local_list_from_model(active.update(db).await?)))
}

/// Delete an owned private list and all memberships.
pub async fn delete_local_list(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    list_id: Uuid,
) -> Result<bool> {
    Ok(local_list::Entity::delete_many()
        .filter(local_list::Column::Id.eq(list_id))
        .filter(local_list::Column::AccountId.eq(account_id.0))
        .exec(db)
        .await?
        .rows_affected
        == 1)
}

/// Return owned lists containing a local or cached-remote account id.
pub async fn local_lists_containing_account(
    db: &impl ConnectionTrait,
    owner_id: AccountId,
    member_id: AccountId,
) -> Result<Vec<LocalList>> {
    let local_ids = local_list_local_member::Entity::find()
        .filter(local_list_local_member::Column::AccountId.eq(member_id.0))
        .all(db)
        .await?
        .into_iter()
        .map(|row| row.list_id);
    let remote_ids = local_list_remote_member::Entity::find()
        .filter(local_list_remote_member::Column::RemoteActorId.eq(member_id.0))
        .all(db)
        .await?
        .into_iter()
        .map(|row| row.list_id);
    let ids = local_ids.chain(remote_ids).collect::<Vec<_>>();
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    Ok(local_list::Entity::find()
        .filter(local_list::Column::AccountId.eq(owner_id.0))
        .filter(local_list::Column::Id.is_in(ids))
        .order_by_asc(local_list::Column::CreatedAt)
        .order_by_asc(local_list::Column::Id)
        .all(db)
        .await?
        .into_iter()
        .map(local_list_from_model)
        .collect())
}

#[derive(Clone, Copy)]
enum ListMemberReference {
    Local(Uuid, Uuid),
    Remote(Uuid, Uuid),
}

impl ListMemberReference {
    fn cursor(self) -> Uuid {
        match self {
            Self::Local(cursor, _) | Self::Remote(cursor, _) => cursor,
        }
    }
}

/// Return one cursor page of list members without per-account queries.
pub async fn local_list_accounts(
    db: &impl ConnectionTrait,
    owner_id: AccountId,
    list_id: Uuid,
    limit: Option<u64>,
    cursor: CollectionCursor,
) -> Result<Option<CollectionPage<ListAccount>>> {
    if find_owned_local_list(db, owner_id, list_id)
        .await?
        .is_none()
    {
        return Ok(None);
    }
    let query_limit = limit.map(page_query_limit);
    let mut local_query = local_list_local_member::Entity::find()
        .filter(local_list_local_member::Column::ListId.eq(list_id));
    let mut remote_query = local_list_remote_member::Entity::find()
        .filter(local_list_remote_member::Column::ListId.eq(list_id));
    if let Some(id) = cursor.max_id {
        local_query = local_query.filter(local_list_local_member::Column::Id.lt(id));
        remote_query = remote_query.filter(local_list_remote_member::Column::Id.lt(id));
    }
    if let Some(id) = cursor.since_id {
        local_query = local_query.filter(local_list_local_member::Column::Id.gt(id));
        remote_query = remote_query.filter(local_list_remote_member::Column::Id.gt(id));
    }
    if let Some(id) = cursor.min_id {
        local_query = local_query.filter(local_list_local_member::Column::Id.gt(id));
        remote_query = remote_query.filter(local_list_remote_member::Column::Id.gt(id));
    }
    local_query = local_query.order_by_desc(local_list_local_member::Column::Id);
    remote_query = remote_query.order_by_desc(local_list_remote_member::Column::Id);
    if let Some(query_limit) = query_limit {
        local_query = local_query.limit(query_limit);
        remote_query = remote_query.limit(query_limit);
    }
    let local_rows = local_query.all(db).await?;
    let remote_rows = remote_query.all(db).await?;
    let mut references = local_rows
        .into_iter()
        .map(|row| ListMemberReference::Local(row.id, row.account_id))
        .chain(
            remote_rows
                .into_iter()
                .map(|row| ListMemberReference::Remote(row.id, row.remote_actor_id)),
        )
        .collect::<Vec<_>>();
    references.sort_by_key(|member| Reverse(member.cursor()));
    let has_more = limit.is_some_and(|limit| references.len() > limit as usize);
    if let Some(limit) = limit {
        references.truncate(limit as usize);
    }
    let local_ids = references
        .iter()
        .filter_map(|member| match member {
            ListMemberReference::Local(_, id) => Some(id.to_owned()),
            ListMemberReference::Remote(_, _) => None,
        })
        .collect::<Vec<_>>();
    let remote_ids = references
        .iter()
        .filter_map(|member| match member {
            ListMemberReference::Remote(_, id) => Some(id.to_owned()),
            ListMemberReference::Local(_, _) => None,
        })
        .collect::<Vec<_>>();
    let locals = local_account::Entity::find()
        .filter(local_account::Column::Id.is_in(local_ids))
        .all(db)
        .await?
        .into_iter()
        .map(|model| {
            let id = model.id;
            local_account_from_model(model).map(|account| (id, account))
        })
        .collect::<Result<HashMap<_, _>>>()?;
    let remotes = remote_actor::Entity::find()
        .filter(remote_actor::Column::Id.is_in(remote_ids))
        .all(db)
        .await?
        .into_iter()
        .map(|model| (model.id, remote_actor_from_model(model)))
        .collect::<HashMap<_, _>>();
    let first_cursor = references.first().map(|member| member.cursor());
    let last_cursor = references.last().map(|member| member.cursor());
    let items = references
        .into_iter()
        .filter_map(|member| match member {
            ListMemberReference::Local(_, id) => locals.get(&id).cloned().map(ListAccount::Local),
            ListMemberReference::Remote(_, id) => {
                remotes.get(&id).cloned().map(ListAccount::Remote)
            }
        })
        .collect();
    Ok(Some(CollectionPage {
        items,
        first_cursor,
        last_cursor,
        has_more,
    }))
}

/// Atomically add followed local or remote accounts to an owned list.
pub async fn add_local_list_accounts(
    txn: &DatabaseTransaction,
    owner_id: AccountId,
    list_id: Uuid,
    account_ids: &[AccountId],
) -> Result<AddListAccountsResult> {
    // Serialize membership validation and insertion across server processes so
    // concurrent duplicate requests receive Mastodon's validation outcome.
    txn.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        vec![format!("list:{list_id}").into()],
    ))
    .await?;
    if find_owned_local_list(txn, owner_id, list_id)
        .await?
        .is_none()
    {
        return Ok(AddListAccountsResult::ListNotFound);
    }
    let ids = account_ids.iter().map(|id| id.0).collect::<HashSet<_>>();
    let local_followed = local_follow::Entity::find()
        .filter(local_follow::Column::FollowerAccountId.eq(owner_id.0))
        .filter(local_follow::Column::FollowedAccountId.is_in(ids.iter().copied()))
        .all(txn)
        .await?
        .into_iter()
        .map(|row| row.followed_account_id)
        .collect::<HashSet<_>>();
    let remote_followed = remote_following::Entity::find()
        .filter(remote_following::Column::LocalAccountId.eq(owner_id.0))
        .filter(remote_following::Column::RemoteActorId.is_in(ids.iter().copied()))
        .filter(remote_following::Column::State.eq(RemoteFollowState::Accepted))
        .filter(remote_following::Column::DeactivatedAt.is_null())
        .all(txn)
        .await?
        .into_iter()
        .map(|row| row.remote_actor_id)
        .collect::<HashSet<_>>();
    if ids
        .iter()
        .any(|id| !local_followed.contains(id) && !remote_followed.contains(id))
    {
        return Ok(AddListAccountsResult::AccountNotFollowed);
    }
    let existing_local = local_list_local_member::Entity::find()
        .filter(local_list_local_member::Column::ListId.eq(list_id))
        .filter(local_list_local_member::Column::AccountId.is_in(ids.iter().copied()))
        .count(txn)
        .await?;
    let existing_remote = local_list_remote_member::Entity::find()
        .filter(local_list_remote_member::Column::ListId.eq(list_id))
        .filter(local_list_remote_member::Column::RemoteActorId.is_in(ids.iter().copied()))
        .count(txn)
        .await?;
    if existing_local + existing_remote > 0 {
        return Ok(AddListAccountsResult::AlreadyPresent);
    }
    for id in ids {
        if local_followed.contains(&id) {
            local_list_local_member::ActiveModel {
                id: Set(Uuid::now_v7()),
                list_id: Set(list_id),
                account_id: Set(id),
                ..Default::default()
            }
            .insert(txn)
            .await?;
        } else {
            local_list_remote_member::ActiveModel {
                id: Set(Uuid::now_v7()),
                list_id: Set(list_id),
                remote_actor_id: Set(id),
                ..Default::default()
            }
            .insert(txn)
            .await?;
        }
    }
    Ok(AddListAccountsResult::Added)
}

/// Idempotently remove accounts from an owned list.
pub async fn remove_local_list_accounts(
    txn: &DatabaseTransaction,
    owner_id: AccountId,
    list_id: Uuid,
    account_ids: &[AccountId],
) -> Result<bool> {
    if find_owned_local_list(txn, owner_id, list_id)
        .await?
        .is_none()
    {
        return Ok(false);
    }
    let ids = account_ids.iter().map(|id| id.0).collect::<Vec<_>>();
    local_list_local_member::Entity::delete_many()
        .filter(local_list_local_member::Column::ListId.eq(list_id))
        .filter(local_list_local_member::Column::AccountId.is_in(ids.clone()))
        .exec(txn)
        .await?;
    local_list_remote_member::Entity::delete_many()
        .filter(local_list_remote_member::Column::ListId.eq(list_id))
        .filter(local_list_remote_member::Column::RemoteActorId.is_in(ids))
        .exec(txn)
        .await?;
    Ok(true)
}

/// Return the mixed local and cached-remote timeline for an owned list.
pub async fn local_list_timeline(
    db: &impl ConnectionTrait,
    owner_id: AccountId,
    list_id: Uuid,
    limit: u64,
    cursor: TimelineCursor,
) -> Result<Option<TimelinePage<HomeTimelineItem>>> {
    let Some(list) = find_owned_local_list(db, owner_id, list_id).await? else {
        return Ok(None);
    };
    let local_member_ids = local_list_local_member::Entity::find()
        .filter(local_list_local_member::Column::ListId.eq(list_id))
        .all(db)
        .await?
        .into_iter()
        .map(|member| member.account_id)
        .collect::<Vec<_>>();
    let remote_member_ids = local_list_remote_member::Entity::find()
        .filter(local_list_remote_member::Column::ListId.eq(list_id))
        .all(db)
        .await?
        .into_iter()
        .map(|member| member.remote_actor_id)
        .collect::<Vec<_>>();
    let hidden_local = hidden_local_account_ids_for_account(db, owner_id)
        .await?
        .into_iter()
        .map(|id| id.0)
        .collect::<HashSet<_>>();
    let hidden_remote = hidden_remote_actor_ids_for_account(db, owner_id)
        .await?
        .into_iter()
        .map(|id| id.0)
        .collect::<HashSet<_>>();
    let visible_local_ids = local_member_ids
        .iter()
        .copied()
        .filter(|id| !hidden_local.contains(id))
        .collect::<Vec<_>>();
    let visible_remote_ids = remote_member_ids
        .iter()
        .copied()
        .filter(|id| !hidden_remote.contains(id))
        .collect::<Vec<_>>();

    let (reply_local_ids, reply_remote_ids) = match list.replies_policy {
        ListRepliesPolicy::None => (Vec::new(), Vec::new()),
        ListRepliesPolicy::List => (local_member_ids.clone(), remote_member_ids.clone()),
        ListRepliesPolicy::Followed => {
            let locals = local_follow::Entity::find()
                .filter(local_follow::Column::FollowerAccountId.eq(owner_id.0))
                .all(db)
                .await?
                .into_iter()
                .map(|follow| follow.followed_account_id)
                .collect();
            let remotes = remote_following::Entity::find()
                .filter(remote_following::Column::LocalAccountId.eq(owner_id.0))
                .filter(remote_following::Column::State.eq(RemoteFollowState::Accepted))
                .filter(remote_following::Column::DeactivatedAt.is_null())
                .all(db)
                .await?
                .into_iter()
                .map(|follow| follow.remote_actor_id)
                .collect();
            (locals, remotes)
        }
    };

    let mut local_query = apply_timeline_cursor(
        local_status::Entity::find()
            .filter(local_status::Column::AccountId.is_in(visible_local_ids.clone()))
            .filter(local_status::Column::Visibility.is_in(["public", "unlisted", "private"]))
            .filter(local_status::Column::DeletedAt.is_null()),
        cursor,
    );
    local_query = match list.replies_policy {
        ListRepliesPolicy::None => local_query
            .filter(local_status::Column::InReplyToId.is_null())
            .filter(local_status::Column::InReplyToRemoteStatusId.is_null()),
        ListRepliesPolicy::List | ListRepliesPolicy::Followed => local_query.filter(
            Condition::any()
                .add(
                    Condition::all()
                        .add(local_status::Column::InReplyToId.is_null())
                        .add(local_status::Column::InReplyToRemoteStatusId.is_null()),
                )
                .add(
                    local_status::Column::InReplyToId.in_subquery(
                        Query::select()
                            .column(local_status::Column::Id)
                            .from(local_status::Entity)
                            .and_where(
                                local_status::Column::AccountId.is_in(reply_local_ids.clone()),
                            )
                            .to_owned(),
                    ),
                )
                .add(
                    local_status::Column::InReplyToRemoteStatusId.in_subquery(
                        Query::select()
                            .column(remote_status::Column::Id)
                            .from(remote_status::Entity)
                            .and_where(
                                remote_status::Column::RemoteActorId
                                    .is_in(reply_remote_ids.clone()),
                            )
                            .to_owned(),
                    ),
                ),
        ),
    };
    let statuses = local_query
        .order_by_desc(local_status::Column::Id)
        .limit(page_query_limit(limit))
        .all(db)
        .await?;

    let mut remote_query = remote_status::Entity::find()
        .filter(remote_status::Column::RemoteActorId.is_in(visible_remote_ids.clone()))
        .filter(remote_status::Column::Visibility.is_in(["public", "unlisted", "private"]))
        .filter(remote_status::Column::DeletedAt.is_null());
    if let Some(id) = cursor.max_id {
        remote_query = remote_query.filter(remote_status::Column::Id.lt(id.0));
    }
    if let Some(id) = cursor.since_id {
        remote_query = remote_query.filter(remote_status::Column::Id.gt(id.0));
    }
    if let Some(id) = cursor.min_id {
        remote_query = remote_query.filter(remote_status::Column::Id.gt(id.0));
    }
    remote_query = match list.replies_policy {
        ListRepliesPolicy::None => remote_query.filter(remote_status::Column::InReplyTo.is_null()),
        ListRepliesPolicy::List | ListRepliesPolicy::Followed => remote_query.filter(
            Condition::any()
                .add(remote_status::Column::InReplyTo.is_null())
                .add(
                    remote_status::Column::InReplyToLocalStatusId.in_subquery(
                        Query::select()
                            .column(local_status::Column::Id)
                            .from(local_status::Entity)
                            .and_where(local_status::Column::AccountId.is_in(reply_local_ids))
                            .to_owned(),
                    ),
                )
                .add(
                    remote_status::Column::InReplyToRemoteStatusId.in_subquery(
                        Query::select()
                            .column(remote_status::Column::Id)
                            .from(remote_status::Entity)
                            .and_where(remote_status::Column::RemoteActorId.is_in(reply_remote_ids))
                            .to_owned(),
                    ),
                ),
        ),
    };
    let remote_statuses = remote_query
        .order_by_desc(remote_status::Column::Id)
        .limit(page_query_limit(limit))
        .all(db)
        .await?;

    let reblog_account_ids = visible_local_ids;
    let reblogs = apply_reblog_timeline_cursor(
        local_status_reblog::Entity::find()
            .filter(local_status_reblog::Column::AccountId.is_in(reblog_account_ids.clone())),
        cursor,
    )
    .order_by_desc(local_status_reblog::Column::Id)
    .limit(page_query_limit(limit))
    .all(db)
    .await?;
    let mut local_remote_reblog_query = local_remote_status_reblog::Entity::find()
        .filter(local_remote_status_reblog::Column::LocalAccountId.is_in(reblog_account_ids));
    if let Some(id) = cursor.max_id {
        local_remote_reblog_query =
            local_remote_reblog_query.filter(local_remote_status_reblog::Column::Id.lt(id.0));
    }
    if let Some(id) = cursor.since_id {
        local_remote_reblog_query =
            local_remote_reblog_query.filter(local_remote_status_reblog::Column::Id.gt(id.0));
    }
    if let Some(id) = cursor.min_id {
        local_remote_reblog_query =
            local_remote_reblog_query.filter(local_remote_status_reblog::Column::Id.gt(id.0));
    }
    let local_remote_reblogs = local_remote_reblog_query
        .order_by_desc(local_remote_status_reblog::Column::Id)
        .limit(page_query_limit(limit))
        .all(db)
        .await?;
    let mut remote_reblog_query = remote_status_reblog::Entity::find()
        .filter(remote_status_reblog::Column::RemoteActorId.is_in(visible_remote_ids));
    if let Some(id) = cursor.max_id {
        remote_reblog_query = remote_reblog_query.filter(remote_status_reblog::Column::Id.lt(id.0));
    }
    if let Some(id) = cursor.since_id {
        remote_reblog_query = remote_reblog_query.filter(remote_status_reblog::Column::Id.gt(id.0));
    }
    if let Some(id) = cursor.min_id {
        remote_reblog_query = remote_reblog_query.filter(remote_status_reblog::Column::Id.gt(id.0));
    }
    let remote_reblogs = remote_reblog_query
        .order_by_desc(remote_status_reblog::Column::Id)
        .limit(page_query_limit(limit))
        .all(db)
        .await?;
    let mut items = statuses
        .into_iter()
        .map(local_status_from_model)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .map(HomeTimelineItem::Status)
        .chain(
            remote_statuses
                .into_iter()
                .map(remote_status_from_model)
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .map(HomeTimelineItem::RemoteStatus),
        )
        .chain(
            reblogs
                .into_iter()
                .map(local_status_reblog_from_model)
                .map(HomeTimelineItem::Reblog),
        )
        .chain(
            local_remote_reblogs
                .into_iter()
                .map(local_remote_status_reblog_from_model)
                .map(HomeTimelineItem::LocalRemoteReblog),
        )
        .chain(
            remote_reblogs
                .into_iter()
                .filter_map(remote_status_reblog_from_model)
                .map(HomeTimelineItem::RemoteReblog),
        )
        .collect::<Vec<_>>();
    items.sort_by_key(|item| Reverse(timeline_item_id(item)));
    let (items, has_more) = trim_to_page(items, limit);
    let first_cursor = items.first().map(timeline_item_id);
    let last_cursor = items.last().map(timeline_item_id);
    Ok(Some(TimelinePage {
        items,
        first_cursor,
        last_cursor,
        has_more,
    }))
}

/// List statuses authored by the account and followed local accounts.
pub async fn home_timeline_for_account(
    db: &impl ConnectionTrait,
    account_id: AccountId,
    limit: u64,
    cursor: TimelineCursor,
) -> Result<TimelinePage<HomeTimelineItem>> {
    let hidden_account_ids = hidden_local_account_ids_for_account(db, account_id)
        .await?
        .into_iter()
        .map(|account_id| account_id.0)
        .collect::<Vec<_>>();
    let hidden_remote_actor_ids = hidden_remote_actor_ids_for_account(db, account_id)
        .await?
        .into_iter()
        .map(|actor_id| actor_id.0)
        .collect::<Vec<_>>();
    let follows = local_follow::Entity::find()
        .filter(local_follow::Column::FollowerAccountId.eq(account_id.0))
        .all(db)
        .await?;
    let exclusive_list_ids = local_list::Entity::find()
        .filter(local_list::Column::AccountId.eq(account_id.0))
        .filter(local_list::Column::Exclusive.eq(true))
        .all(db)
        .await?
        .into_iter()
        .map(|list| list.id)
        .collect::<Vec<_>>();
    let exclusive_local_ids = if exclusive_list_ids.is_empty() {
        HashSet::new()
    } else {
        local_list_local_member::Entity::find()
            .filter(local_list_local_member::Column::ListId.is_in(exclusive_list_ids.clone()))
            .all(db)
            .await?
            .into_iter()
            .map(|member| member.account_id)
            .collect::<HashSet<_>>()
    };
    let exclusive_remote_ids = if exclusive_list_ids.is_empty() {
        Vec::new()
    } else {
        local_list_remote_member::Entity::find()
            .filter(local_list_remote_member::Column::ListId.is_in(exclusive_list_ids))
            .all(db)
            .await?
            .into_iter()
            .map(|member| member.remote_actor_id)
            .collect::<Vec<_>>()
    };
    let followed_ids = follows
        .iter()
        .filter(|follow| !exclusive_local_ids.contains(&follow.followed_account_id))
        .map(|follow| follow.followed_account_id)
        .collect::<Vec<_>>();
    let reblog_followed_ids = follows
        .iter()
        .filter(|follow| !exclusive_local_ids.contains(&follow.followed_account_id))
        .filter(|follow| follow.show_reblogs)
        .map(|follow| follow.followed_account_id)
        .collect::<Vec<_>>();
    let followed_tag_ids = local_tag_follow::Entity::find()
        .filter(local_tag_follow::Column::AccountId.eq(account_id.0))
        .all(db)
        .await?
        .into_iter()
        .map(|follow| follow.tag_id)
        .collect::<Vec<_>>();

    let mut status_condition = Condition::any()
        .add(local_status::Column::AccountId.eq(account_id.0))
        .add(
            Condition::all()
                .add(local_status::Column::AccountId.is_in(followed_ids.clone()))
                .add(local_status::Column::Visibility.is_in(["public", "unlisted", "private"])),
        );
    status_condition = status_condition.add(
        Condition::all()
            .add(local_status::Column::Visibility.eq(StatusVisibility::Private))
            .add(
                local_status::Column::Id.in_subquery(
                    Query::select()
                        .column(local_status_local_recipient::Column::StatusId)
                        .from(local_status_local_recipient::Entity)
                        .and_where(local_status_local_recipient::Column::AccountId.eq(account_id.0))
                        .to_owned(),
                ),
            ),
    );
    if !followed_tag_ids.is_empty() {
        status_condition = status_condition.add(
            Condition::all()
                .add(local_status::Column::Visibility.eq(StatusVisibility::Public))
                .add(
                    local_status::Column::Id
                        .in_subquery(status_tags_subquery(followed_tag_ids.clone())),
                ),
        );
    }
    let mut status_query = apply_timeline_cursor(
        local_status::Entity::find()
            .filter(status_condition)
            .filter(local_status::Column::DeletedAt.is_null()),
        cursor,
    );
    if !hidden_account_ids.is_empty() {
        status_query = status_query
            .filter(local_status::Column::AccountId.is_not_in(hidden_account_ids.clone()));
    }
    if !exclusive_local_ids.is_empty() {
        status_query =
            status_query.filter(local_status::Column::AccountId.is_not_in(exclusive_local_ids));
    }
    let statuses = status_query
        .order_by_desc(local_status::Column::Id)
        .limit(page_query_limit(limit))
        .all(db)
        .await?;
    let reblog_account_ids = std::iter::once(account_id.0)
        .chain(reblog_followed_ids.iter().copied())
        .collect::<Vec<_>>();
    let mut reblog_query = apply_reblog_timeline_cursor(
        local_status_reblog::Entity::find()
            .filter(local_status_reblog::Column::AccountId.is_in(reblog_account_ids.clone())),
        cursor,
    );
    if !hidden_account_ids.is_empty() {
        reblog_query = reblog_query
            .filter(local_status_reblog::Column::AccountId.is_not_in(hidden_account_ids));
    }
    let reblogs = reblog_query
        .order_by_desc(local_status_reblog::Column::Id)
        .limit(page_query_limit(limit))
        .all(db)
        .await?;
    let mut remote_reblogs_by_local_query = local_remote_status_reblog::Entity::find()
        .filter(local_remote_status_reblog::Column::LocalAccountId.is_in(reblog_account_ids));
    if let Some(max_id) = cursor.max_id {
        remote_reblogs_by_local_query = remote_reblogs_by_local_query
            .filter(local_remote_status_reblog::Column::Id.lt(max_id.0));
    }
    if let Some(since_id) = cursor.since_id {
        remote_reblogs_by_local_query = remote_reblogs_by_local_query
            .filter(local_remote_status_reblog::Column::Id.gt(since_id.0));
    }
    if let Some(min_id) = cursor.min_id {
        remote_reblogs_by_local_query = remote_reblogs_by_local_query
            .filter(local_remote_status_reblog::Column::Id.gt(min_id.0));
    }
    let remote_reblogs_by_local = remote_reblogs_by_local_query
        .order_by_desc(local_remote_status_reblog::Column::Id)
        .limit(page_query_limit(limit))
        .all(db)
        .await?;
    let mut followed_remote_actors = Query::select();
    followed_remote_actors
        .column(remote_following::Column::RemoteActorId)
        .from(remote_following::Entity)
        .and_where(remote_following::Column::LocalAccountId.eq(account_id.0))
        .and_where(remote_following::Column::State.eq(RemoteFollowState::Accepted))
        .and_where(remote_following::Column::DeactivatedAt.is_null());
    if !exclusive_remote_ids.is_empty() {
        followed_remote_actors.and_where(
            remote_following::Column::RemoteActorId.is_not_in(exclusive_remote_ids.clone()),
        );
    }
    let followed_remote_actors = followed_remote_actors.to_owned();
    let mut remote_status_condition = Condition::any()
        .add(
            Condition::all()
                .add(remote_status::Column::RemoteActorId.in_subquery(followed_remote_actors))
                .add(remote_status::Column::Visibility.is_in(["public", "unlisted", "private"])),
        )
        .add(
            Condition::all()
                .add(remote_status::Column::Visibility.eq(StatusVisibility::Private))
                .add(
                    remote_status::Column::Id.in_subquery(
                        Query::select()
                            .column(remote_status_local_recipient::Column::RemoteStatusId)
                            .from(remote_status_local_recipient::Entity)
                            .and_where(
                                remote_status_local_recipient::Column::AccountId.eq(account_id.0),
                            )
                            .to_owned(),
                    ),
                ),
        );
    if !followed_tag_ids.is_empty() {
        remote_status_condition = remote_status_condition.add(
            Condition::all()
                .add(remote_status::Column::Visibility.eq(StatusVisibility::Public))
                .add(
                    remote_status::Column::Id
                        .in_subquery(remote_status_tags_subquery(followed_tag_ids)),
                ),
        );
    }
    let mut remote_query = remote_status::Entity::find()
        .filter(remote_status_condition)
        .filter(remote_status::Column::DeletedAt.is_null());
    if !hidden_remote_actor_ids.is_empty() {
        remote_query = remote_query.filter(
            remote_status::Column::RemoteActorId.is_not_in(hidden_remote_actor_ids.clone()),
        );
    }
    if !exclusive_remote_ids.is_empty() {
        remote_query = remote_query
            .filter(remote_status::Column::RemoteActorId.is_not_in(exclusive_remote_ids.clone()));
    }
    if let Some(max_id) = cursor.max_id {
        remote_query = remote_query.filter(remote_status::Column::Id.lt(max_id.0));
    }
    if let Some(since_id) = cursor.since_id {
        remote_query = remote_query.filter(remote_status::Column::Id.gt(since_id.0));
    }
    if let Some(min_id) = cursor.min_id {
        remote_query = remote_query.filter(remote_status::Column::Id.gt(min_id.0));
    }
    let remote_statuses = remote_query
        .order_by_desc(remote_status::Column::Id)
        .limit(page_query_limit(limit))
        .all(db)
        .await?;
    let mut remote_reblog_query = remote_status_reblog::Entity::find().filter(
        remote_status_reblog::Column::RemoteActorId.in_subquery(
            Query::select()
                .column(remote_following::Column::RemoteActorId)
                .from(remote_following::Entity)
                .and_where(remote_following::Column::LocalAccountId.eq(account_id.0))
                .and_where(remote_following::Column::State.eq(RemoteFollowState::Accepted))
                .and_where(remote_following::Column::ShowReblogs.eq(true))
                .and_where(remote_following::Column::DeactivatedAt.is_null())
                .to_owned(),
        ),
    );
    if !exclusive_remote_ids.is_empty() {
        remote_reblog_query = remote_reblog_query
            .filter(remote_status_reblog::Column::RemoteActorId.is_not_in(exclusive_remote_ids));
    }
    if !hidden_remote_actor_ids.is_empty() {
        remote_reblog_query = remote_reblog_query
            .filter(remote_status_reblog::Column::RemoteActorId.is_not_in(hidden_remote_actor_ids));
    }
    if let Some(max_id) = cursor.max_id {
        remote_reblog_query =
            remote_reblog_query.filter(remote_status_reblog::Column::Id.lt(max_id.0));
    }
    if let Some(since_id) = cursor.since_id {
        remote_reblog_query =
            remote_reblog_query.filter(remote_status_reblog::Column::Id.gt(since_id.0));
    }
    if let Some(min_id) = cursor.min_id {
        remote_reblog_query =
            remote_reblog_query.filter(remote_status_reblog::Column::Id.gt(min_id.0));
    }
    let remote_reblogs = remote_reblog_query
        .order_by_desc(remote_status_reblog::Column::Id)
        .limit(page_query_limit(limit))
        .all(db)
        .await?;
    let statuses = statuses
        .into_iter()
        .map(local_status_from_model)
        .collect::<Result<Vec<_>>>()?;
    let remote_statuses = remote_statuses
        .into_iter()
        .map(remote_status_from_model)
        .collect::<Result<Vec<_>>>()?;
    let mut items = statuses
        .into_iter()
        .map(HomeTimelineItem::Status)
        .chain(
            reblogs
                .into_iter()
                .map(local_status_reblog_from_model)
                .map(HomeTimelineItem::Reblog),
        )
        .chain(
            remote_statuses
                .into_iter()
                .map(HomeTimelineItem::RemoteStatus),
        )
        .chain(
            remote_reblogs_by_local
                .into_iter()
                .map(local_remote_status_reblog_from_model)
                .map(HomeTimelineItem::LocalRemoteReblog),
        )
        .chain(
            remote_reblogs
                .into_iter()
                .filter_map(remote_status_reblog_from_model)
                .map(HomeTimelineItem::RemoteReblog),
        )
        .collect::<Vec<_>>();
    items.sort_by_key(|item| Reverse(timeline_item_id(item)));
    let (items, has_more) = trim_to_page(items, limit);
    let first_cursor = items.first().map(timeline_item_id);
    let last_cursor = items.last().map(timeline_item_id);

    Ok(TimelinePage {
        items,
        first_cursor,
        last_cursor,
        has_more,
    })
}

/// Apply Mastodon cursor parameters to a local status query.
fn apply_timeline_cursor(
    mut query: Select<local_status::Entity>,
    cursor: TimelineCursor,
) -> Select<local_status::Entity> {
    if let Some(max_id) = cursor.max_id {
        query = query.filter(local_status::Column::Id.lt(max_id.0));
    }
    if let Some(since_id) = cursor.since_id {
        query = query.filter(local_status::Column::Id.gt(since_id.0));
    }
    if let Some(min_id) = cursor.min_id {
        query = query.filter(local_status::Column::Id.gt(min_id.0));
    }
    query
}

/// Apply Mastodon timeline cursor parameters to a local boost query.
fn apply_reblog_timeline_cursor(
    mut query: Select<local_status_reblog::Entity>,
    cursor: TimelineCursor,
) -> Select<local_status_reblog::Entity> {
    if let Some(max_id) = cursor.max_id {
        query = query.filter(local_status_reblog::Column::Id.lt(max_id.0));
    }
    if let Some(since_id) = cursor.since_id {
        query = query.filter(local_status_reblog::Column::Id.gt(since_id.0));
    }
    if let Some(min_id) = cursor.min_id {
        query = query.filter(local_status_reblog::Column::Id.gt(min_id.0));
    }
    query
}

fn page_query_limit(limit: u64) -> u64 {
    limit.saturating_add(1)
}

fn trim_to_page<T>(mut items: Vec<T>, limit: u64) -> (Vec<T>, bool) {
    let limit = limit as usize;
    let has_more = items.len() > limit;
    if has_more {
        items.truncate(limit);
    }
    (items, has_more)
}

fn timeline_item_id(item: &HomeTimelineItem) -> Uuid {
    match item {
        HomeTimelineItem::Status(status) => status.id.0,
        HomeTimelineItem::Reblog(reblog) => reblog.id,
        HomeTimelineItem::RemoteStatus(status) => status.id.0,
        HomeTimelineItem::LocalRemoteReblog(reblog) => reblog.id,
        HomeTimelineItem::RemoteReblog(reblog) => reblog.id,
    }
}

/// Adds Mastodon cursor filters to SeaORM collection queries.
trait ApplyCollectionCursor {
    /// Apply `max_id`, `since_id`, and `min_id` filters to an ordered collection query.
    fn apply_collection_cursor(self, cursor: CollectionCursor) -> Self;
}

impl ApplyCollectionCursor for Select<local_status_favourite::Entity> {
    fn apply_collection_cursor(mut self, cursor: CollectionCursor) -> Self {
        if let Some(max_id) = cursor.max_id {
            self = self.filter(local_status_favourite::Column::Id.lt(max_id));
        }
        if let Some(since_id) = cursor.since_id {
            self = self.filter(local_status_favourite::Column::Id.gt(since_id));
        }
        if let Some(min_id) = cursor.min_id {
            self = self.filter(local_status_favourite::Column::Id.gt(min_id));
        }
        self
    }
}

impl ApplyCollectionCursor for Select<local_status_bookmark::Entity> {
    fn apply_collection_cursor(mut self, cursor: CollectionCursor) -> Self {
        if let Some(max_id) = cursor.max_id {
            self = self.filter(local_status_bookmark::Column::Id.lt(max_id));
        }
        if let Some(since_id) = cursor.since_id {
            self = self.filter(local_status_bookmark::Column::Id.gt(since_id));
        }
        if let Some(min_id) = cursor.min_id {
            self = self.filter(local_status_bookmark::Column::Id.gt(min_id));
        }
        self
    }
}

impl ApplyCollectionCursor for Select<local_status_reblog::Entity> {
    fn apply_collection_cursor(mut self, cursor: CollectionCursor) -> Self {
        if let Some(max_id) = cursor.max_id {
            self = self.filter(local_status_reblog::Column::Id.lt(max_id));
        }
        if let Some(since_id) = cursor.since_id {
            self = self.filter(local_status_reblog::Column::Id.gt(since_id));
        }
        if let Some(min_id) = cursor.min_id {
            self = self.filter(local_status_reblog::Column::Id.gt(min_id));
        }
        self
    }
}

impl ApplyCollectionCursor for Select<local_follow::Entity> {
    fn apply_collection_cursor(mut self, cursor: CollectionCursor) -> Self {
        if let Some(max_id) = cursor.max_id {
            self = self.filter(local_follow::Column::Id.lt(max_id));
        }
        if let Some(since_id) = cursor.since_id {
            self = self.filter(local_follow::Column::Id.gt(since_id));
        }
        if let Some(min_id) = cursor.min_id {
            self = self.filter(local_follow::Column::Id.gt(min_id));
        }
        self
    }
}

impl ApplyCollectionCursor for Select<local_account_block::Entity> {
    fn apply_collection_cursor(mut self, cursor: CollectionCursor) -> Self {
        if let Some(max_id) = cursor.max_id {
            self = self.filter(local_account_block::Column::Id.lt(max_id));
        }
        if let Some(since_id) = cursor.since_id {
            self = self.filter(local_account_block::Column::Id.gt(since_id));
        }
        if let Some(min_id) = cursor.min_id {
            self = self.filter(local_account_block::Column::Id.gt(min_id));
        }
        self
    }
}

impl ApplyCollectionCursor for Select<local_account_mute::Entity> {
    fn apply_collection_cursor(mut self, cursor: CollectionCursor) -> Self {
        if let Some(max_id) = cursor.max_id {
            self = self.filter(local_account_mute::Column::Id.lt(max_id));
        }
        if let Some(since_id) = cursor.since_id {
            self = self.filter(local_account_mute::Column::Id.gt(since_id));
        }
        if let Some(min_id) = cursor.min_id {
            self = self.filter(local_account_mute::Column::Id.gt(min_id));
        }
        self
    }
}

impl ApplyCollectionCursor for Select<local_notification::Entity> {
    fn apply_collection_cursor(mut self, cursor: CollectionCursor) -> Self {
        if let Some(max_id) = cursor.max_id {
            self = self.filter(local_notification::Column::Id.lt(max_id));
        }
        if let Some(since_id) = cursor.since_id {
            self = self.filter(local_notification::Column::Id.gt(since_id));
        }
        if let Some(min_id) = cursor.min_id {
            self = self.filter(local_notification::Column::Id.gt(min_id));
        }
        self
    }
}

impl ApplyCollectionCursor for Select<local_conversation_account::Entity> {
    fn apply_collection_cursor(mut self, cursor: CollectionCursor) -> Self {
        if let Some(max_id) = cursor.max_id {
            self = self.filter(local_conversation_account::Column::CursorId.lt(max_id));
        }
        if let Some(since_id) = cursor.since_id {
            self = self.filter(local_conversation_account::Column::CursorId.gt(since_id));
        }
        if let Some(min_id) = cursor.min_id {
            self = self.filter(local_conversation_account::Column::CursorId.gt(min_id));
        }
        self
    }
}

impl ApplyCollectionCursor for Select<remote_follow::Entity> {
    fn apply_collection_cursor(mut self, cursor: CollectionCursor) -> Self {
        if let Some(max_id) = cursor.max_id {
            self = self.filter(remote_follow::Column::Id.lt(max_id));
        }
        if let Some(since_id) = cursor.since_id {
            self = self.filter(remote_follow::Column::Id.gt(since_id));
        }
        if let Some(min_id) = cursor.min_id {
            self = self.filter(remote_follow::Column::Id.gt(min_id));
        }
        self
    }
}

impl ApplyCollectionCursor for Select<remote_following::Entity> {
    fn apply_collection_cursor(mut self, cursor: CollectionCursor) -> Self {
        if let Some(max_id) = cursor.max_id {
            self = self.filter(remote_following::Column::Id.lt(max_id));
        }
        if let Some(since_id) = cursor.since_id {
            self = self.filter(remote_following::Column::Id.gt(since_id));
        }
        if let Some(min_id) = cursor.min_id {
            self = self.filter(remote_following::Column::Id.gt(min_id));
        }
        self
    }
}

impl ApplyCollectionCursor for Select<local_remote_status_favourite::Entity> {
    fn apply_collection_cursor(mut self, cursor: CollectionCursor) -> Self {
        if let Some(max_id) = cursor.max_id {
            self = self.filter(local_remote_status_favourite::Column::Id.lt(max_id));
        }
        if let Some(since_id) = cursor.since_id {
            self = self.filter(local_remote_status_favourite::Column::Id.gt(since_id));
        }
        if let Some(min_id) = cursor.min_id {
            self = self.filter(local_remote_status_favourite::Column::Id.gt(min_id));
        }
        self
    }
}

impl ApplyCollectionCursor for Select<remote_status_favourite::Entity> {
    fn apply_collection_cursor(mut self, cursor: CollectionCursor) -> Self {
        if let Some(max_id) = cursor.max_id {
            self = self.filter(remote_status_favourite::Column::Id.lt(max_id));
        }
        if let Some(since_id) = cursor.since_id {
            self = self.filter(remote_status_favourite::Column::Id.gt(since_id));
        }
        if let Some(min_id) = cursor.min_id {
            self = self.filter(remote_status_favourite::Column::Id.gt(min_id));
        }
        self
    }
}

impl ApplyCollectionCursor for Select<local_remote_status_reblog::Entity> {
    fn apply_collection_cursor(mut self, cursor: CollectionCursor) -> Self {
        if let Some(max_id) = cursor.max_id {
            self = self.filter(local_remote_status_reblog::Column::Id.lt(max_id));
        }
        if let Some(since_id) = cursor.since_id {
            self = self.filter(local_remote_status_reblog::Column::Id.gt(since_id));
        }
        if let Some(min_id) = cursor.min_id {
            self = self.filter(local_remote_status_reblog::Column::Id.gt(min_id));
        }
        self
    }
}

impl ApplyCollectionCursor for Select<remote_status_reblog::Entity> {
    fn apply_collection_cursor(mut self, cursor: CollectionCursor) -> Self {
        if let Some(max_id) = cursor.max_id {
            self = self.filter(remote_status_reblog::Column::Id.lt(max_id));
        }
        if let Some(since_id) = cursor.since_id {
            self = self.filter(remote_status_reblog::Column::Id.gt(since_id));
        }
        if let Some(min_id) = cursor.min_id {
            self = self.filter(remote_status_reblog::Column::Id.gt(min_id));
        }
        self
    }
}

/// Mark an active model field as changed only when an update value is present.
fn set_if_some<T>(active_value: &mut ActiveValue<T>, value: Option<T>)
where
    T: Into<sea_orm::Value>,
{
    if let Some(value) = value {
        *active_value = Set(value);
    }
}

/// Register an OAuth application and return stored metadata plus the raw client secret.
pub async fn create_oauth_application(
    db: &impl ConnectionTrait,
    name: &str,
    redirect_uri: &str,
    scopes: &str,
    website: Option<&str>,
    token_pepper: &str,
) -> Result<(OAuthApplication, String)> {
    let app_id = Uuid::now_v7();
    let client_id = random_token();
    let client_secret = random_token();
    let client_secret_hash = secret_hash(token_pepper, &client_secret)?;

    oauth_application::ActiveModel {
        id: Set(app_id),
        client_id: Set(client_id.clone()),
        client_secret_hash: Set(client_secret_hash.clone()),
        name: Set(name.to_owned()),
        redirect_uri: Set(redirect_uri.to_owned()),
        scopes: Set(scopes.to_owned()),
        website: Set(website.map(str::to_owned)),
        ..Default::default()
    }
    .insert(db)
    .await?;

    Ok((
        OAuthApplication {
            id: app_id,
            client_id,
            client_secret_hash,
            name: name.to_owned(),
            redirect_uri: redirect_uri.to_owned(),
            scopes: scopes.to_owned(),
            website: website.map(str::to_owned),
        },
        client_secret,
    ))
}

/// Find an OAuth application by public client id.
pub async fn find_oauth_application_by_client_id(
    db: &impl ConnectionTrait,
    client_id: &str,
) -> Result<Option<OAuthApplication>> {
    let app = oauth_application::Entity::find()
        .filter(oauth_application::Column::ClientId.eq(client_id))
        .one(db)
        .await?;

    Ok(app.map(oauth_application_from_model))
}

/// PKCE method persisted with an OAuth authorization code.
#[derive(
    Clone, Copy, Debug, DeriveValueType, Display, EnumString, Eq, IntoStaticStr, PartialEq,
)]
#[sea_orm(value_type = "String")]
pub enum PkceCodeChallengeMethod {
    #[strum(serialize = "")]
    None,
    #[strum(serialize = "S256")]
    S256,
}

/// Data needed to issue a short-lived OAuth authorization code.
pub struct NewAuthorizationCode<'a> {
    /// Account granting the authorization.
    pub account_id: AccountId,
    /// OAuth application receiving the grant.
    pub application_id: Uuid,
    /// Redirect URI used by the authorization request.
    pub redirect_uri: &'a str,
    /// Space-separated granted scopes.
    pub scopes: &'a str,
    /// PKCE code challenge.
    pub code_challenge: &'a str,
    /// PKCE challenge method.
    pub code_challenge_method: PkceCodeChallengeMethod,
}

/// Create a one-time OAuth authorization code.
pub async fn create_authorization_code(
    db: &impl ConnectionTrait,
    token_pepper: &str,
    new_code: NewAuthorizationCode<'_>,
) -> Result<String> {
    let code = random_token();
    let code_hash = secret_hash(token_pepper, &code)?;
    let expires_at = OffsetDateTime::now_utc() + Duration::minutes(5);

    oauth_authorization_code::ActiveModel {
        id: Set(Uuid::now_v7()),
        code_hash: Set(code_hash),
        account_id: Set(new_code.account_id.0),
        application_id: Set(new_code.application_id),
        redirect_uri: Set(new_code.redirect_uri.to_owned()),
        scopes: Set(new_code.scopes.to_owned()),
        code_challenge: Set(new_code.code_challenge.to_owned()),
        code_challenge_method: Set(new_code.code_challenge_method.to_owned()),
        expires_at: Set(expires_at),
        ..Default::default()
    }
    .insert(db)
    .await?;

    Ok(code)
}

/// Consume a one-time authorization code and return grant metadata when valid.
pub async fn consume_authorization_code(
    db: &impl ConnectionTrait,
    token_pepper: &str,
    code: &str,
    application_id: Uuid,
    redirect_uri: &str,
) -> Result<Option<(AccountId, String, String, PkceCodeChallengeMethod)>> {
    let code_hash = secret_hash(token_pepper, code)?;
    let Some(code) = oauth_authorization_code::Entity::find()
        .filter(oauth_authorization_code::Column::CodeHash.eq(code_hash))
        .filter(oauth_authorization_code::Column::ApplicationId.eq(application_id))
        .filter(oauth_authorization_code::Column::RedirectUri.eq(redirect_uri))
        .filter(oauth_authorization_code::Column::ConsumedAt.is_null())
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    if code.expires_at <= OffsetDateTime::now_utc() {
        return Ok(None);
    }

    let grant = (
        AccountId(code.account_id),
        code.scopes.clone(),
        code.code_challenge.clone(),
        code.code_challenge_method,
    );
    let mut active_code = code.into_active_model();
    active_code.consumed_at = Set(Some(OffsetDateTime::now_utc()));
    active_code.update(db).await?;

    Ok(Some(grant))
}

/// Create and persist a hashed opaque OAuth access token.
pub async fn create_access_token(
    db: &impl ConnectionTrait,
    token_pepper: &str,
    account_id: AccountId,
    application_id: Uuid,
    scopes: &str,
) -> Result<OAuthAccessToken> {
    let token = random_token();
    let token_hash = secret_hash(token_pepper, &token)?;
    let issued_at = OffsetDateTime::now_utc();

    oauth_access_token::ActiveModel {
        id: Set(Uuid::now_v7()),
        token_hash: Set(token_hash),
        account_id: Set(account_id.0),
        application_id: Set(application_id),
        scopes: Set(scopes.to_owned()),
        issued_at: Set(issued_at),
        ..Default::default()
    }
    .insert(db)
    .await?;

    Ok(OAuthAccessToken {
        token,
        token_type: OAuthTokenType::Bearer,
        scope: scopes.to_owned(),
        created_at: issued_at.unix_timestamp(),
    })
}

/// Resolve a raw OAuth access token to its local account and granted scopes.
pub async fn find_account_by_access_token(
    db: &impl ConnectionTrait,
    token_pepper: &str,
    token: &str,
) -> Result<Option<(LocalAccount, String)>> {
    let token_hash = secret_hash(token_pepper, token)?;
    let Some(token) = oauth_access_token::Entity::find()
        .filter(oauth_access_token::Column::TokenHash.eq(token_hash))
        .filter(oauth_access_token::Column::RevokedAt.is_null())
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    if token
        .expires_at
        .is_some_and(|expires_at| expires_at <= OffsetDateTime::now_utc())
    {
        return Ok(None);
    }

    let account = local_account::Entity::find_by_id(token.account_id)
        .filter(local_account::Column::SuspendedAt.is_null())
        .one(db)
        .await?;

    account
        .map(|account| Ok((local_account_from_model(account)?, token.scopes)))
        .transpose()
}

/// Resolve a raw access token while preserving the persisted token identifier.
pub async fn find_access_token_grant(
    db: &impl ConnectionTrait,
    token_pepper: &str,
    raw_token: &str,
) -> Result<Option<AccessTokenGrant>> {
    let token_hash = secret_hash(token_pepper, raw_token)?;
    let Some(token) = oauth_access_token::Entity::find()
        .filter(oauth_access_token::Column::TokenHash.eq(token_hash))
        .filter(oauth_access_token::Column::RevokedAt.is_null())
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    if token
        .expires_at
        .is_some_and(|expires_at| expires_at <= OffsetDateTime::now_utc())
    {
        return Ok(None);
    }
    let Some(account) = local_account::Entity::find_by_id(token.account_id)
        .filter(local_account::Column::SuspendedAt.is_null())
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    Ok(Some(AccessTokenGrant {
        id: token.id,
        account: local_account_from_model(account)?,
        scopes: token.scopes,
    }))
}

/// Revoke an OAuth access token if it exists.
pub async fn revoke_access_token(
    db: &impl ConnectionTrait,
    token_pepper: &str,
    token: &str,
) -> Result<()> {
    let token_hash = secret_hash(token_pepper, token)?;
    if let Some(token) = oauth_access_token::Entity::find()
        .filter(oauth_access_token::Column::TokenHash.eq(token_hash))
        .one(db)
        .await?
    {
        push_subscription::Entity::delete_many()
            .filter(push_subscription::Column::AccessTokenId.eq(token.id))
            .exec(db)
            .await?;
        let mut active_token = token.into_active_model();
        active_token.revoked_at = Set(Some(OffsetDateTime::now_utc()));
        active_token.update(db).await?;
    }

    Ok(())
}

/// Return the Web Push subscription belonging to one access token.
pub async fn push_subscription_for_access_token(
    db: &DbConnection,
    access_token_id: Uuid,
) -> Result<Option<PushSubscription>> {
    push_subscription::Entity::find()
        .filter(push_subscription::Column::AccessTokenId.eq(access_token_id))
        .one(db)
        .await?
        .map(push_subscription_from_model)
        .transpose()
}

/// Replace the Web Push subscription belonging to an access token.
pub async fn upsert_push_subscription(
    db: &DbConnection,
    input: NewPushSubscription,
) -> Result<PushSubscription> {
    let now = OffsetDateTime::now_utc();
    let access_token_id = input.access_token_id;
    push_subscription::Entity::insert(push_subscription::ActiveModel {
        id: Set(Uuid::now_v7()),
        access_token_id: Set(access_token_id),
        account_id: Set(input.account_id.0),
        endpoint: Set(input.endpoint),
        p256dh: Set(input.p256dh),
        auth: Set(input.auth),
        standard: Set(input.encoding == PushSubscriptionEncoding::Standard),
        policy: Set(input.policy),
        alerts: Set(push_alerts_json(input.alerts)?),
        access_token_ciphertext: Set(input.access_token_ciphertext),
        access_token_nonce: Set(input.access_token_nonce),
        created_at: Set(now),
        updated_at: Set(now),
    })
    .on_conflict(
        OnConflict::column(push_subscription::Column::AccessTokenId)
            .update_columns([
                push_subscription::Column::AccountId,
                push_subscription::Column::Endpoint,
                push_subscription::Column::P256dh,
                push_subscription::Column::Auth,
                push_subscription::Column::Standard,
                push_subscription::Column::Policy,
                push_subscription::Column::Alerts,
                push_subscription::Column::AccessTokenCiphertext,
                push_subscription::Column::AccessTokenNonce,
                push_subscription::Column::UpdatedAt,
            ])
            .to_owned(),
    )
    .exec(db)
    .await?;
    let model = push_subscription::Entity::find()
        .filter(push_subscription::Column::AccessTokenId.eq(access_token_id))
        .one(db)
        .await?
        .ok_or_else(|| {
            RoostyError::InvalidInput("push subscription disappeared after upsert".to_owned())
        })?;
    push_subscription_from_model(model)
}

/// Update alert and policy settings without replacing endpoint key material.
pub async fn update_push_subscription(
    db: &DbConnection,
    access_token_id: Uuid,
    alerts: PushAlerts,
    policy: PushPolicy,
) -> Result<Option<PushSubscription>> {
    let Some(model) = push_subscription::Entity::find()
        .filter(push_subscription::Column::AccessTokenId.eq(access_token_id))
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    let mut active = model.into_active_model();
    active.alerts = Set(push_alerts_json(alerts)?);
    active.policy = Set(policy);
    active.updated_at = Set(OffsetDateTime::now_utc());
    Ok(Some(push_subscription_from_model(
        active.update(db).await?,
    )?))
}

/// Delete one token's subscription, returning whether it existed.
pub async fn delete_push_subscription(db: &DbConnection, access_token_id: Uuid) -> Result<bool> {
    Ok(push_subscription::Entity::delete_many()
        .filter(push_subscription::Column::AccessTokenId.eq(access_token_id))
        .exec(db)
        .await?
        .rows_affected
        > 0)
}

/// Load a queued subscription only while it still belongs to the notification recipient.
pub async fn push_delivery(
    db: &DbConnection,
    notification_id: Uuid,
    subscription_id: Uuid,
) -> Result<Option<(LocalNotification, PushSubscription)>> {
    let Some(notification) = local_notification::Entity::find_by_id(notification_id)
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    let Some(subscription) = push_subscription::Entity::find_by_id(subscription_id)
        .filter(push_subscription::Column::AccountId.eq(notification.account_id))
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    Ok(Some((
        local_notification_from_model(notification),
        push_subscription_from_model(subscription)?,
    )))
}

/// Evaluate a Mastodon push policy against the actor that caused a notification.
pub async fn push_policy_allows(
    db: &DbConnection,
    notification: &LocalNotification,
    policy: PushPolicy,
) -> Result<bool> {
    match policy {
        PushPolicy::All => Ok(true),
        PushPolicy::None => Ok(false),
        PushPolicy::Followed => match (notification.actor_account_id, notification.remote_actor_id)
        {
            (Some(actor), None) => Ok(local_follow::Entity::find_by_id((
                notification.account_id.0,
                actor.0,
            ))
            .one(db)
            .await?
            .is_some()),
            (None, Some(actor)) => Ok(remote_following::Entity::find()
                .filter(remote_following::Column::LocalAccountId.eq(notification.account_id.0))
                .filter(remote_following::Column::RemoteActorId.eq(actor.0))
                .filter(remote_following::Column::State.eq(RemoteFollowState::Accepted))
                .filter(remote_following::Column::DeactivatedAt.is_null())
                .one(db)
                .await?
                .is_some()),
            _ => Ok(false),
        },
        PushPolicy::Follower => match (notification.actor_account_id, notification.remote_actor_id)
        {
            (Some(actor), None) => Ok(local_follow::Entity::find_by_id((
                actor.0,
                notification.account_id.0,
            ))
            .one(db)
            .await?
            .is_some()),
            (None, Some(actor)) => {
                remote_actor_follows_local_account(db, actor, notification.account_id).await
            }
            _ => Ok(false),
        },
    }
}

/// Remove a subscription after a push service permanently rejects it.
pub async fn delete_push_subscription_by_id(db: &DbConnection, id: Uuid) -> Result<()> {
    push_subscription::Entity::delete_by_id(id).exec(db).await?;
    Ok(())
}

/// Generate a URL-safe random opaque token.
pub fn random_token() -> String {
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Compute the stable HMAC hash stored for opaque secrets and tokens.
pub fn secret_hash(pepper: &str, secret: &str) -> Result<String> {
    let mut mac = HmacSha256::new_from_slice(pepper.as_bytes())
        .map_err(|error| RoostyError::InvalidInput(error.to_string()))?;
    mac.update(secret.as_bytes());
    Ok(URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
}

/// Compute the OAuth PKCE S256 challenge for a verifier.
pub fn pkce_s256_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn local_account_from_model(account: local_account::Model) -> Result<LocalAccount> {
    Ok(LocalAccount {
        id: AccountId(account.id),
        username: account.username,
        email: account.email,
        password_hash: account.password_hash,
        is_admin: account.is_admin,
        display_name: account.display_name,
        note: account.note,
        locked: account.locked,
        bot: account.bot,
        discoverable: account.discoverable,
        default_visibility: account.default_visibility,
        default_sensitive: account.default_sensitive,
        default_language: account.default_language,
        default_quote_policy: account.default_quote_policy,
        profile_fields: account.profile_fields,
        avatar_file_path: account.avatar_file_path,
        header_file_path: account.header_file_path,
        limited_at: account.limited_at,
        suspended_at: account.suspended_at,
        data_purged_at: account.data_purged_at,
        created_at: account.created_at,
    })
}

/// Convert a persisted remote actor cache model into the shared projection.
fn remote_actor_from_model(actor: remote_actor::Model) -> RemoteActor {
    RemoteActor {
        id: AccountId(actor.id),
        activitypub_id: actor.activitypub_id,
        username: actor.username,
        domain: actor.domain,
        display_name: actor.display_name,
        summary: actor.summary,
        emojis: actor.emojis,
        inbox_url: actor.inbox_url,
        shared_inbox_url: actor.shared_inbox_url,
        followers_url: actor.followers_url,
        featured_url: actor.featured_url,
        featured_tags_url: actor.featured_tags_url,
        public_key_id: actor.public_key_id,
        public_key_pem: actor.public_key_pem,
        expires_at: actor.expires_at,
        profile_created_at: actor.profile_created_at,
        first_seen_at: actor.created_at,
        deleted_at: actor.deleted_at,
        moved_to_remote_actor_id: actor.moved_to_remote_actor_id.map(AccountId),
        limited_at: actor.limited_at,
        suspended_at: actor.suspended_at,
        data_purged_at: actor.data_purged_at,
        discoverable: actor.discoverable,
    }
}

fn federation_domain_block_from_model(
    model: federation_domain_block::Model,
) -> FederationDomainBlock {
    FederationDomainBlock {
        id: model.id,
        domain: model.domain,
        severity: model.severity,
        reject_media: model.reject_media,
        reject_reports: model.reject_reports,
        private_comment: model.private_comment,
        public_comment: model.public_comment,
        obfuscate: model.obfuscate,
        created_at: model.created_at,
        updated_at: model.updated_at,
    }
}

fn remote_custom_emoji_from_model(model: remote_custom_emoji::Model) -> Result<RemoteCustomEmoji> {
    Ok(RemoteCustomEmoji {
        id: model.id,
        shortcode: model.shortcode,
        remote_url: model.remote_url,
        content_type: model.content_type,
        state: model.state,
        file_path: model.file_path,
        expires_at: model.expires_at,
    })
}

/// Convert a persisted remote Note cache model into the shared projection.
fn remote_status_from_model(status: remote_status::Model) -> Result<RemoteStatus> {
    Ok(RemoteStatus {
        id: StatusId(status.id),
        activitypub_id: status.activitypub_id,
        remote_actor_id: AccountId(status.remote_actor_id),
        content: status.content,
        visibility: status.visibility,
        published_at: status.published_at,
        updated_at: status.updated_at,
        deleted_at: status.deleted_at,
        in_reply_to: status.in_reply_to,
        in_reply_to_local_status_id: status.in_reply_to_local_status_id.map(StatusId),
        in_reply_to_remote_status_id: status.in_reply_to_remote_status_id.map(StatusId),
        conversation_id: status.conversation_id,
        object: status.object,
        quote_automatic_policy: serde_json::from_value(status.quote_automatic_policy)
            .map_err(|error| RoostyError::InvalidInput(error.to_string()))?,
        quote_manual_policy: serde_json::from_value(status.quote_manual_policy)
            .map_err(|error| RoostyError::InvalidInput(error.to_string()))?,
    })
}

/// Convert a persisted local Announce of a remote Note into its shared projection.
fn local_remote_status_reblog_from_model(
    reblog: local_remote_status_reblog::Model,
) -> LocalRemoteStatusReblog {
    LocalRemoteStatusReblog {
        id: reblog.id,
        local_account_id: AccountId(reblog.local_account_id),
        remote_status_id: StatusId(reblog.remote_status_id),
        activity_id: reblog.activity_id,
        created_at: reblog.created_at,
    }
}

/// Convert a persisted remote Announce into its shared projection.
fn remote_status_reblog_from_model(
    reblog: remote_status_reblog::Model,
) -> Option<RemoteStatusReblog> {
    let target = match (reblog.local_status_id, reblog.remote_status_id) {
        (Some(status_id), None) => RemoteStatusReblogTarget::Local(StatusId(status_id)),
        (None, Some(status_id)) => RemoteStatusReblogTarget::Remote(StatusId(status_id)),
        _ => return None,
    };
    Some(RemoteStatusReblog {
        id: reblog.id,
        remote_actor_id: AccountId(reblog.remote_actor_id),
        target,
        activity_id: reblog.activity_id,
        created_at: reblog.created_at,
    })
}

fn remote_following_from_model(follow: remote_following::Model) -> RemoteFollowing {
    RemoteFollowing {
        local_account_id: AccountId(follow.local_account_id),
        remote_actor_id: AccountId(follow.remote_actor_id),
        activity_id: follow.activity_id,
        state: follow.state,
        show_reblogs: follow.show_reblogs,
        notify: follow.notify,
    }
}

fn remote_follow_from_model(follow: remote_follow::Model) -> RemoteFollow {
    RemoteFollow {
        id: follow.id,
        remote_actor_id: AccountId(follow.remote_actor_id),
        local_account_id: AccountId(follow.local_account_id),
        activity_id: follow.activity_id,
        activity: follow.activity,
        state: follow.state,
    }
}

fn remote_follow_from_row(row: RemoteFollowRow) -> RemoteFollow {
    RemoteFollow {
        id: row.id,
        remote_actor_id: AccountId(row.remote_actor_id),
        local_account_id: AccountId(row.local_account_id),
        activity_id: row.activity_id,
        activity: row.activity,
        state: row.state,
    }
}

/// Convert a SeaORM local follow model into the public DB value type.
fn local_follow_from_model(follow: local_follow::Model) -> LocalFollow {
    LocalFollow {
        follower_account_id: AccountId(follow.follower_account_id),
        followed_account_id: AccountId(follow.followed_account_id),
        show_reblogs: follow.show_reblogs,
        notify: follow.notify,
    }
}

/// Convert a SeaORM mute row into its database API representation.
fn local_account_mute_from_model(mute: local_account_mute::Model) -> LocalAccountMute {
    LocalAccountMute {
        account_id: AccountId(mute.account_id),
        target_account_id: AccountId(mute.target_account_id),
        notifications: mute.notifications,
        expires_at: mute.expires_at,
    }
}

fn local_remote_account_mute_from_model(
    mute: local_remote_account_mute::Model,
) -> LocalRemoteAccountMute {
    LocalRemoteAccountMute {
        local_account_id: AccountId(mute.local_account_id),
        remote_actor_id: AccountId(mute.remote_actor_id),
        notifications: mute.notifications,
        expires_at: mute.expires_at,
    }
}

fn local_status_from_model(status: local_status::Model) -> Result<LocalStatus> {
    Ok(LocalStatus {
        id: StatusId(status.id),
        account_id: AccountId(status.account_id),
        content: status.content,
        visibility: status.visibility,
        sensitive: status.sensitive,
        spoiler_text: status.spoiler_text,
        language: status.language,
        in_reply_to_id: status.in_reply_to_id.map(StatusId),
        in_reply_to_remote_status_id: status.in_reply_to_remote_status_id.map(StatusId),
        conversation_id: status.conversation_id,
        created_at: status.created_at,
        updated_at: status.updated_at,
        deleted_at: status.deleted_at,
        quote_approval_policy: status.quote_approval_policy,
    })
}

fn status_quote_from_model(model: status_quote::Model) -> Result<StatusQuote> {
    let quoting_status = match (
        model.local_quoting_status_id,
        model.remote_quoting_status_id,
    ) {
        (Some(id), None) => StatusReference::Local(StatusId(id)),
        (None, Some(id)) => StatusReference::Remote(StatusId(id)),
        _ => {
            return Err(RoostyError::InvalidInput(
                "stored quote origin is invalid".to_owned(),
            ));
        }
    };
    let quoted_status = match (model.quoted_local_status_id, model.quoted_remote_status_id) {
        (Some(id), None) => Some(StatusReference::Local(StatusId(id))),
        (None, Some(id)) => Some(StatusReference::Remote(StatusId(id))),
        (None, None) => None,
        _ => {
            return Err(RoostyError::InvalidInput(
                "stored quote target is invalid".to_owned(),
            ));
        }
    };
    Ok(StatusQuote {
        id: model.id,
        quoting_status,
        quoted_status,
        quoted_activitypub_id: model.quoted_activitypub_id,
        state: model.state,
        quote_request_id: model.quote_request_id,
        authorization_id: model.authorization_id,
        created_at: model.created_at,
        updated_at: model.updated_at,
    })
}

fn local_tag_from_model(tag: local_tag::Model) -> LocalTag {
    LocalTag {
        id: tag.id,
        name: tag.name,
        created_at: tag.created_at,
        updated_at: tag.updated_at,
    }
}

fn local_conversation_from_model(conversation: local_conversation::Model) -> LocalConversation {
    LocalConversation {
        id: conversation.id,
        last_status_id: conversation.last_status_id.map(StatusId),
        last_remote_status_id: conversation.last_remote_status_id.map(StatusId),
        created_at: conversation.created_at,
        updated_at: conversation.updated_at,
    }
}

fn local_conversation_account_from_model(
    account: local_conversation_account::Model,
) -> LocalConversationAccount {
    LocalConversationAccount {
        id: account.id,
        cursor_id: account.cursor_id,
        conversation_id: account.conversation_id,
        account_id: AccountId(account.account_id),
        unread: account.unread,
        hidden_at: account.hidden_at,
        last_status_id: account.last_status_id.map(StatusId),
        last_remote_status_id: account.last_remote_status_id.map(StatusId),
        created_at: account.created_at,
        updated_at: account.updated_at,
    }
}

fn local_status_reblog_from_model(reblog: local_status_reblog::Model) -> LocalStatusReblog {
    LocalStatusReblog {
        id: reblog.id,
        account_id: AccountId(reblog.account_id),
        status_id: StatusId(reblog.status_id),
        created_at: reblog.created_at,
    }
}

fn local_notification_from_model(notification: local_notification::Model) -> LocalNotification {
    LocalNotification {
        id: notification.id,
        account_id: AccountId(notification.account_id),
        notification_type: notification.notification_type,
        actor_account_id: notification.actor_account_id.map(AccountId),
        remote_actor_id: notification.remote_actor_id.map(AccountId),
        status_id: notification.status_id.map(StatusId),
        remote_status_id: notification.remote_status_id.map(StatusId),
        report_id: notification.report_id,
        group_id: notification.group_id,
        filtered: notification.filtered,
        notification_request_id: notification.notification_request_id,
        created_at: notification.created_at,
        dismissed_at: notification.dismissed_at,
    }
}

fn notification_policy_from_model(policy: local_notification_policy::Model) -> NotificationPolicy {
    NotificationPolicy {
        for_not_following: policy.for_not_following,
        for_not_followers: policy.for_not_followers,
        for_new_accounts: policy.for_new_accounts,
        for_private_mentions: policy.for_private_mentions,
        for_limited_accounts: policy.for_limited_accounts,
    }
}

fn notification_request_from_row(row: NotificationRequestRow) -> Result<NotificationRequest> {
    let actor = match (row.actor_account_id, row.remote_actor_id) {
        (Some(id), None) => NotificationActor::Local(AccountId(id)),
        (None, Some(id)) => NotificationActor::Remote(AccountId(id)),
        _ => {
            return Err(RoostyError::InvalidInput(
                "stored notification request actor is invalid".to_owned(),
            ));
        }
    };
    Ok(NotificationRequest {
        id: row.id,
        account_id: AccountId(row.account_id),
        actor,
        last_status_id: row.last_status_id.map(StatusId),
        last_remote_status_id: row.last_remote_status_id.map(StatusId),
        notifications_count: u64::try_from(row.notifications_count).map_err(|_| {
            RoostyError::InvalidInput("stored notification request count is invalid".to_owned())
        })?,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

fn push_subscription_from_model(
    subscription: push_subscription::Model,
) -> Result<PushSubscription> {
    let alerts = serde_json::from_value(subscription.alerts).map_err(|error| {
        RoostyError::InvalidInput(format!("stored push alerts are invalid: {error}"))
    })?;
    Ok(PushSubscription {
        id: subscription.id,
        access_token_id: subscription.access_token_id,
        account_id: AccountId(subscription.account_id),
        endpoint: subscription.endpoint,
        p256dh: subscription.p256dh,
        auth: subscription.auth,
        encoding: if subscription.standard {
            PushSubscriptionEncoding::Standard
        } else {
            PushSubscriptionEncoding::Legacy
        },
        policy: subscription.policy,
        alerts,
        access_token_ciphertext: subscription.access_token_ciphertext,
        access_token_nonce: subscription.access_token_nonce,
    })
}

fn push_alerts_json(alerts: PushAlerts) -> Result<JsonValue> {
    serde_json::to_value(alerts).map_err(|error| {
        RoostyError::InvalidInput(format!("push alerts cannot be serialized: {error}"))
    })
}

/// Convert a SeaORM timeline marker row into its database API representation.
fn local_timeline_marker_from_model(
    marker: local_timeline_marker::Model,
) -> Result<LocalTimelineMarker> {
    Ok(LocalTimelineMarker {
        timeline: marker.timeline,
        last_read_id: marker.last_read_id,
        version: marker.version,
        updated_at: marker.updated_at,
    })
}

fn local_media_attachment_from_model(media: local_media_attachment::Model) -> LocalMediaAttachment {
    LocalMediaAttachment {
        id: media.id,
        account_id: AccountId(media.account_id),
        status_id: media.status_id.map(StatusId),
        scheduled_status_id: media.scheduled_status_id,
        status_order: media.status_order,
        content_type: media.content_type,
        original_filename: media.original_filename,
        file_path: media.file_path,
        preview_file_path: media.preview_file_path,
        file_size: media.file_size,
        description: media.description,
        focus_x: media.focus_x,
        focus_y: media.focus_y,
        width: media.width,
        height: media.height,
        preview_width: media.preview_width,
        preview_height: media.preview_height,
        blurhash: media.blurhash,
    }
}

fn scheduled_status_from_model(status: scheduled_status::Model) -> ScheduledStatus {
    ScheduledStatus {
        id: status.id,
        account_id: AccountId(status.account_id),
        publication_status_id: StatusId(status.publication_status_id),
        content: status.content,
        visibility: status.visibility,
        sensitive: status.sensitive,
        spoiler_text: status.spoiler_text,
        language: status.language,
        in_reply_to_id: status.in_reply_to_id.map(StatusId),
        in_reply_to_remote_status_id: status.in_reply_to_remote_status_id.map(StatusId),
        quoted_status_id: status.quoted_status_id.map(StatusId),
        quote_approval_policy: status.quote_approval_policy,
        scheduled_at: status.scheduled_at,
    }
}

fn oauth_application_from_model(app: oauth_application::Model) -> OAuthApplication {
    OAuthApplication {
        id: app.id,
        client_id: app.client_id,
        client_secret_hash: app.client_secret_hash,
        name: app.name,
        redirect_uri: app.redirect_uri,
        scopes: app.scopes,
        website: app.website,
    }
}

/// Durable background job claimed by a worker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimedJob {
    /// Job identifier.
    pub id: JobId,
    /// Lease identity that must accompany the final job outcome.
    pub claim_id: JobClaimId,
    /// Application job kind.
    pub kind: JobKind,
    /// JSON job payload.
    pub payload: JsonValue,
    /// Number of prior failed attempts.
    pub attempts: u32,
    /// Time the job was first enqueued.
    pub created_at: OffsetDateTime,
}

/// Cross-process durable queue health shown to administrators.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminJobSummary {
    pub due: u64,
    pub in_progress: u64,
    pub scheduled_retries: u64,
    pub permanently_failed: u64,
    pub oldest_due_at: Option<OffsetDateTime>,
}

/// Sanitized durable-job diagnostics; the stored payload is deliberately omitted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminJobDiagnostic {
    pub id: JobId,
    pub kind: JobKind,
    pub attempts: u32,
    pub run_after: OffsetDateTime,
    pub locked_at: Option<OffsetDateTime>,
    pub last_error: Option<String>,
    pub created_at: OffsetDateTime,
    pub completed_at: Option<OffsetDateTime>,
    pub permanently_failed_at: Option<OffsetDateTime>,
}

/// Origin of an administrator mutation.
#[derive(
    Clone, Copy, Debug, DeriveValueType, Display, EnumString, Eq, IntoStaticStr, PartialEq,
)]
#[sea_orm(value_type = "String")]
#[strum(serialize_all = "snake_case")]
pub enum AdminAuditSource {
    Web,
    Api,
    Cli,
}

/// Closed administrator mutations currently supported by Roosty.
#[derive(
    Clone, Copy, Debug, DeriveValueType, Display, EnumString, Eq, IntoStaticStr, PartialEq,
)]
#[sea_orm(value_type = "String")]
pub enum AdminAuditAction {
    #[strum(serialize = "account.create")]
    AccountCreate,
    #[strum(serialize = "account.reset_password")]
    AccountResetPassword,
    #[strum(serialize = "account.limit")]
    AccountLimit,
    #[strum(serialize = "account.unlimit")]
    AccountUnlimit,
    #[strum(serialize = "account.suspend")]
    AccountSuspend,
    #[strum(serialize = "account.unsuspend")]
    AccountUnsuspend,
    #[strum(serialize = "account.purge")]
    AccountPurge,
    #[strum(serialize = "domain_block.create")]
    DomainBlockCreate,
    #[strum(serialize = "domain_block.update")]
    DomainBlockUpdate,
    #[strum(serialize = "domain_block.delete")]
    DomainBlockDelete,
    #[strum(serialize = "instance_rule.create")]
    InstanceRuleCreate,
    #[strum(serialize = "instance_rule.update")]
    InstanceRuleUpdate,
    #[strum(serialize = "instance_rule.delete")]
    InstanceRuleDelete,
    #[strum(serialize = "instance_rule.reorder")]
    InstanceRuleReorder,
    #[strum(serialize = "report.update")]
    ReportUpdate,
    #[strum(serialize = "report.assign")]
    ReportAssign,
    #[strum(serialize = "report.unassign")]
    ReportUnassign,
    #[strum(serialize = "report.resolve")]
    ReportResolve,
    #[strum(serialize = "report.reopen")]
    ReportReopen,
}

/// Kind of record affected by an administrator mutation.
#[derive(
    Clone, Copy, Debug, DeriveValueType, Display, EnumString, Eq, IntoStaticStr, PartialEq,
)]
#[sea_orm(value_type = "String")]
#[strum(serialize_all = "snake_case")]
pub enum AdminAuditTargetKind {
    LocalAccount,
    RemoteActor,
    FederationDomain,
    InstanceRule,
    Report,
}

/// Immutable administrator action suitable for an audit-log UI.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminAuditEntry {
    pub id: Uuid,
    pub actor_account_id: Option<AccountId>,
    pub source: AdminAuditSource,
    pub action: AdminAuditAction,
    pub target_kind: AdminAuditTargetKind,
    pub target_id: String,
    pub metadata: JsonValue,
    pub created_at: OffsetDateTime,
}

/// Append an administrator audit event through a caller-owned transaction.
pub async fn insert_admin_audit_entry(
    txn: &DatabaseTransaction,
    actor_account_id: Option<AccountId>,
    source: AdminAuditSource,
    action: AdminAuditAction,
    target_kind: AdminAuditTargetKind,
    target_id: &str,
    metadata: JsonValue,
) -> Result<AdminAuditEntry> {
    let model = entity::admin_audit_log::ActiveModel {
        id: Set(Uuid::now_v7()),
        actor_account_id: Set(actor_account_id.map(|id| id.0)),
        source: Set(source),
        action: Set(action),
        target_kind: Set(target_kind),
        target_id: Set(target_id.to_owned()),
        metadata: Set(metadata),
        ..Default::default()
    }
    .insert(txn)
    .await?;
    Ok(admin_audit_entry_from_model(model))
}

/// Return recent audit events in stable UUIDv7 cursor order.
pub async fn list_admin_audit_entries(
    db: &impl ConnectionTrait,
    limit: u64,
    max_id: Option<Uuid>,
) -> Result<Vec<AdminAuditEntry>> {
    let mut query = entity::admin_audit_log::Entity::find()
        .order_by_desc(entity::admin_audit_log::Column::Id)
        .limit(limit.min(100));
    if let Some(max_id) = max_id {
        query = query.filter(entity::admin_audit_log::Column::Id.lt(max_id));
    }
    let entries = query
        .all(db)
        .await?
        .into_iter()
        .map(admin_audit_entry_from_model)
        .collect();
    Ok(entries)
}

fn admin_audit_entry_from_model(model: entity::admin_audit_log::Model) -> AdminAuditEntry {
    AdminAuditEntry {
        id: model.id,
        actor_account_id: model.actor_account_id.map(AccountId),
        source: model.source,
        action: model.action,
        target_kind: model.target_kind,
        target_id: model.target_id,
        metadata: model.metadata,
        created_at: model.created_at,
    }
}

/// Summarize durable work from shared database state.
pub async fn admin_job_summary(db: &impl ConnectionTrait) -> Result<AdminJobSummary> {
    let row = db
        .query_one(Statement::from_string(
            DatabaseBackend::Postgres,
            r#"
            SELECT
                count(*) FILTER (
                    WHERE completed_at IS NULL AND locked_at IS NULL AND run_after <= now()
                ) AS due,
                count(*) FILTER (
                    WHERE completed_at IS NULL AND locked_at IS NOT NULL
                ) AS in_progress,
                count(*) FILTER (
                    WHERE completed_at IS NULL AND locked_at IS NULL
                      AND attempts > 0 AND run_after > now()
                ) AS scheduled_retries,
                count(*) FILTER (
                    WHERE permanently_failed_at IS NOT NULL
                ) AS permanently_failed,
                min(run_after) FILTER (
                    WHERE completed_at IS NULL AND locked_at IS NULL AND run_after <= now()
                ) AS oldest_due_at
            FROM job
            "#,
        ))
        .await?
        .ok_or_else(|| DbErr::RecordNotFound("job summary returned no row".to_owned()))?;
    let summary = AdminJobSummary {
        due: u64::try_from(row.try_get::<i64>("", "due")?)
            .map_err(|_| DbErr::Type("negative due job count".to_owned()))?,
        in_progress: u64::try_from(row.try_get::<i64>("", "in_progress")?)
            .map_err(|_| DbErr::Type("negative claimed job count".to_owned()))?,
        scheduled_retries: u64::try_from(row.try_get::<i64>("", "scheduled_retries")?)
            .map_err(|_| DbErr::Type("negative retry job count".to_owned()))?,
        permanently_failed: u64::try_from(row.try_get::<i64>("", "permanently_failed")?)
            .map_err(|_| DbErr::Type("negative permanent failure count".to_owned()))?,
        oldest_due_at: row.try_get("", "oldest_due_at")?,
    };
    Ok(summary)
}

/// List current and recently permanently failed jobs without exposing payloads.
pub async fn admin_job_diagnostics(
    db: &impl ConnectionTrait,
    limit: u64,
    max_id: Option<Uuid>,
) -> Result<Vec<AdminJobDiagnostic>> {
    let mut query = entity::job::Entity::find()
        .filter(
            Condition::any()
                .add(entity::job::Column::CompletedAt.is_null())
                .add(entity::job::Column::PermanentlyFailedAt.is_not_null()),
        )
        .order_by_desc(entity::job::Column::Id)
        .limit(limit.min(100));
    if let Some(max_id) = max_id {
        query = query.filter(entity::job::Column::Id.lt(max_id));
    }
    let diagnostics = query
        .all(db)
        .await?
        .into_iter()
        .map(|model| {
            let attempts = u32::try_from(model.attempts).map_err(|_| {
                RoostyError::InvalidInput("stored job attempts must not be negative".to_owned())
            })?;
            Ok(AdminJobDiagnostic {
                id: JobId(model.id),
                kind: model.kind,
                attempts,
                run_after: model.run_after,
                locked_at: model.locked_at,
                last_error: model
                    .last_error
                    .map(|error| sanitize_admin_job_error(&error)),
                created_at: model.created_at,
                completed_at: model.completed_at,
                permanently_failed_at: model.permanently_failed_at,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(diagnostics)
}

fn sanitize_admin_job_error(error: &str) -> String {
    error
        .split_inclusive(char::is_whitespace)
        .map(|part| {
            part.find("https://")
                .or_else(|| part.find("http://"))
                .map_or_else(
                    || part.to_owned(),
                    |start| format!("{}[redacted-url] ", &part[..start]),
                )
        })
        .collect::<String>()
        .trim_end()
        .chars()
        .take(500)
        .collect()
}

/// Enqueue a durable job, reusing an active deduplicated job when present.
pub async fn enqueue_job(
    db: &DbConnection,
    kind: JobKind,
    payload: JsonValue,
    deduplication_key: Option<&str>,
    run_after: OffsetDateTime,
) -> Result<JobId> {
    enqueue_job_on_connection(
        db,
        NewJob {
            kind,
            payload,
            deduplication_key: deduplication_key.map(str::to_owned),
            run_after,
        },
    )
    .await
}

/// Insert a durable job through a caller-owned transaction.
///
/// The job is rolled back with the enclosing domain mutation, preventing a
/// delivery from observing state that was never committed.
pub async fn enqueue_job_in_transaction(txn: &impl ConnectionTrait, job: NewJob) -> Result<JobId> {
    enqueue_job_on_connection(txn, job).await
}

/// Insert or reuse a job through either the pool or a database transaction.
async fn enqueue_job_on_connection<C>(db: &C, job: NewJob) -> Result<JobId>
where
    C: ConnectionTrait,
{
    let job_id = JobId(Uuid::now_v7());
    let row = db
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
            WITH inserted AS (
                INSERT INTO job (id, kind, payload, deduplication_key, run_after)
                VALUES ($1, $2, $3, $4, $5)
                ON CONFLICT (kind, deduplication_key)
                WHERE deduplication_key IS NOT NULL AND completed_at IS NULL
                DO NOTHING
                RETURNING id
            )
            SELECT id FROM inserted
            UNION ALL
            SELECT id FROM job
            WHERE kind = $2
              AND deduplication_key = $4
              AND completed_at IS NULL
            LIMIT 1
            "#,
            vec![
                job_id.0.into(),
                job.kind.as_str().to_owned().into(),
                job.payload.into(),
                job.deduplication_key.into(),
                job.run_after.into(),
            ],
        ))
        .await?
        .ok_or_else(|| {
            RoostyError::from(DbErr::RecordNotFound(
                "job enqueue returned no row".to_owned(),
            ))
        })?;
    let id: Uuid = row.try_get("", "id")?;

    Ok(JobId(id))
}

/// Claim one due job using PostgreSQL row locking.
pub async fn claim_due_job(
    db: &DbConnection,
    worker_id: &str,
    claim_ttl: Duration,
) -> Result<Option<ClaimedJob>> {
    let expired_before = OffsetDateTime::now_utc() - claim_ttl;
    let claim_id = JobClaimId(Uuid::now_v7());
    let row = db
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
            UPDATE job
            SET locked_at = now(), locked_by = $1, claim_id = $2
            WHERE id IN (
                SELECT id
                FROM job
                WHERE completed_at IS NULL
                  AND run_after <= now()
                  AND (locked_at IS NULL OR locked_at < $3)
                  AND kind IN (
                    'federation_follow_response', 'federation_status_delivery',
                    'federation_quote_delivery', 'federation_follow_delivery',
                    'federation_favourite_delivery', 'federation_reblog_delivery',
                    'federation_actor_update_delivery', 'federation_moderation_delivery',
                    'federation_remote_media_fetch', 'federation_featured_refresh',
                    'federation_featured_tags_refresh', 'federation_thread_resolve',
                    'federation_replies_fetch', 'federation_reply_fetch', 'web_push_delivery',
                    'notification_request_merge', 'notification_request_cleanup',
                    'scheduled_status_publish', 'trend_maintenance',
                    'preview_card_fetch', 'preview_card_backfill'
                  )
                ORDER BY run_after, created_at
                LIMIT 1
                FOR UPDATE SKIP LOCKED
            )
            RETURNING id, kind, payload, attempts, created_at, claim_id
            "#,
            vec![
                worker_id.to_owned().into(),
                claim_id.0.into(),
                expired_before.into(),
            ],
        ))
        .await?;

    row.map(|row| {
        let id: Uuid = row.try_get("", "id")?;
        let kind: JobKind = row.try_get("", "kind")?;
        let payload: JsonValue = row.try_get("", "payload")?;
        let attempts: i32 = row.try_get("", "attempts")?;
        let attempts = u32::try_from(attempts).map_err(|_| {
            RoostyError::InvalidInput("stored job attempts must not be negative".to_owned())
        })?;
        let created_at: OffsetDateTime = row.try_get("", "created_at")?;
        let claim_id: Uuid = row.try_get("", "claim_id")?;

        Ok(ClaimedJob {
            id: JobId(id),
            claim_id: JobClaimId(claim_id),
            kind,
            payload,
            attempts,
            created_at,
        })
    })
    .transpose()
}

/// Mark a claimed job as completed when its lease is still owned by this worker.
pub async fn mark_job_completed(db: &DbConnection, job: &ClaimedJob) -> Result<bool> {
    let result = db
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
        UPDATE job
        SET completed_at = now(), locked_at = NULL, locked_by = NULL, claim_id = NULL
        WHERE id = $1 AND claim_id = $2
        "#,
            vec![job.id.0.into(), job.claim_id.0.into()],
        ))
        .await?;

    Ok(result.rows_affected() == 1)
}

/// Mark a job failed, release its claim, and schedule its next retry.
pub async fn mark_job_failed(
    db: &DbConnection,
    job: &ClaimedJob,
    error: &str,
) -> Result<Option<OffsetDateTime>> {
    let run_after = next_retry_at(job.attempts);
    let result = db
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
        UPDATE job
        SET attempts = attempts + 1,
            last_error = $2,
            run_after = $3,
            locked_at = NULL,
            locked_by = NULL,
            claim_id = NULL
        WHERE id = $1 AND claim_id = $4
        "#,
            vec![
                job.id.0.into(),
                error.to_owned().into(),
                run_after.into(),
                job.claim_id.0.into(),
            ],
        ))
        .await?;

    Ok((result.rows_affected() == 1).then_some(run_after))
}

/// Mark a job as permanently failed while retaining its diagnostic error.
pub async fn mark_job_permanently_failed(
    db: &DbConnection,
    job: &ClaimedJob,
    error: &str,
) -> Result<bool> {
    let result = db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "UPDATE job SET last_error = $2, completed_at = now(), permanently_failed_at = now(), locked_at = NULL, locked_by = NULL, claim_id = NULL WHERE id = $1 AND claim_id = $3",
        vec![job.id.0.into(), error.to_owned().into(), job.claim_id.0.into()],
    )).await?;
    Ok(result.rows_affected() == 1)
}

/// Return whether a job has exceeded its configured retry age.
pub fn job_has_exceeded_max_age(created_at: OffsetDateTime, max_age: Duration) -> bool {
    OffsetDateTime::now_utc() - created_at >= max_age
}

/// Calculate the next retry timestamp for a failed job.
pub fn next_retry_at(attempts: u32) -> OffsetDateTime {
    let exponent = attempts.min(12);
    let seconds = 2_i64.pow(exponent).min(3_600);
    OffsetDateTime::now_utc() + Duration::seconds(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Given overlapping policy predicates, when actions differ, then drop wins over filter and
    /// filter wins over accept.
    #[test]
    fn notification_policy_uses_strongest_matching_action() {
        assert_eq!(
            strongest_notification_policy_action([
                NotificationPolicyAction::Accept,
                NotificationPolicyAction::Filter,
                NotificationPolicyAction::Drop,
            ]),
            NotificationPolicyAction::Drop
        );
        assert_eq!(
            strongest_notification_policy_action([
                NotificationPolicyAction::Accept,
                NotificationPolicyAction::Filter,
            ]),
            NotificationPolicyAction::Filter
        );
        assert_eq!(
            strongest_notification_policy_action([]),
            NotificationPolicyAction::Accept
        );
    }

    #[test]
    fn hashes_secrets_with_pepper() {
        let first = secret_hash("pepper", "secret").unwrap();
        let second = secret_hash("pepper", "secret").unwrap();
        let different = secret_hash("other-pepper", "secret").unwrap();

        assert_eq!(first, second);
        assert_ne!(first, "secret");
        assert_ne!(first, different);
    }

    #[test]
    fn computes_pkce_s256_challenge() {
        assert_eq!(
            pkce_s256_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn retry_backoff_is_capped() {
        let now = OffsetDateTime::now_utc();
        let early = next_retry_at(1);
        let late = next_retry_at(100);

        assert!(early > now);
        assert!(late - now <= Duration::hours(1) + Duration::seconds(1));
    }

    /// Given a delivery error containing a private endpoint, when it is projected to an
    /// administrator, then diagnostic context remains without exposing the complete URL.
    #[test]
    fn administrator_job_errors_redact_urls() {
        assert_eq!(
            sanitize_admin_job_error("request to https://remote.example/inbox?token=secret failed"),
            "request to [redacted-url] failed"
        );
    }

    /// Worker job identifiers retain stable persisted spellings through the typed API.
    #[test]
    fn job_kinds_use_stable_persisted_values() {
        assert_eq!(
            JobKind::FederationFollowResponse.as_str(),
            "federation_follow_response"
        );
        assert_eq!(
            JobKind::FederationStatusDelivery.as_str(),
            "federation_status_delivery"
        );
        assert_eq!(
            JobKind::FederationFollowDelivery.as_str(),
            "federation_follow_delivery"
        );
        assert_eq!(
            JobKind::FederationFeaturedTagsRefresh.as_str(),
            "federation_featured_tags_refresh"
        );
        assert_eq!(
            JobKind::FederationThreadResolve.as_str(),
            "federation_thread_resolve"
        );
        assert_eq!(
            JobKind::FederationRepliesFetch.as_str(),
            "federation_replies_fetch"
        );
        assert_eq!(
            JobKind::FederationReplyFetch.as_str(),
            "federation_reply_fetch"
        );
        assert_eq!(JobKind::WebPushDelivery.as_str(), "web_push_delivery");
    }

    /// Status ranking enforces Mastodon's interaction threshold and hourly decay.
    #[test]
    fn status_trend_scoring_applies_threshold_eligibility_and_decay() {
        assert_eq!(status_trend_score(4, 0.0, true), 0.0);
        assert_eq!(status_trend_score(5, 0.0, false), 0.0);
        assert_eq!(status_trend_score(5, 0.0, true), 16.0);
        assert!((status_trend_score(5, 3_600.0, true) - 8.0).abs() < f64::EPSILON);
    }

    /// Hashtag ranking compares distinct actors and retains a recent peak during cooldown.
    #[test]
    fn tag_trend_scoring_applies_threshold_and_peak_cooldown() {
        assert_eq!(tag_raw_score(4, 1), 0.0);
        assert_eq!(tag_raw_score(5, 6), 0.0);
        assert_eq!(tag_raw_score(5, 1), 16.0);
        let now = OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
        let old_at = now - Duration::hours(12);
        assert_eq!(
            choose_tag_peak(4.0, Some(16.0), Some(old_at), now),
            (16.0, old_at)
        );
        assert_eq!(
            choose_tag_peak(25.0, Some(16.0), Some(old_at), now),
            (25.0, now)
        );
        assert_eq!(
            choose_tag_peak(4.0, Some(16.0), Some(now - Duration::days(3)), now),
            (4.0, now)
        );
    }

    /// Scheduled trend cycles align to exact UTC cadence boundaries without drift.
    #[test]
    fn trend_refresh_boundaries_are_strictly_future_and_aligned() {
        let now = OffsetDateTime::from_unix_timestamp(1_800_000_123).unwrap();
        let interval_milliseconds = 300_000;
        let next = next_trend_refresh_at(now, interval_milliseconds).unwrap();
        assert!(next > now);
        assert_eq!(
            next.unix_timestamp_nanos()
                .div_euclid(1_000_000)
                .rem_euclid(i128::from(interval_milliseconds)),
            0
        );
        assert_eq!(
            next_trend_refresh_at(next, interval_milliseconds).unwrap() - next,
            Duration::minutes(5)
        );
    }

    #[test]
    fn push_alerts_use_typed_notification_dispatch() {
        let enabled = PushAlerts {
            mention: true,
            favourite: true,
            follow: true,
            follow_request: true,
            reblog: true,
            status: true,
            update: true,
            quote: true,
            quoted_update: true,
            poll: true,
            admin_report: true,
        };
        for notification_type in [
            LocalNotificationType::Mention,
            LocalNotificationType::Favourite,
            LocalNotificationType::Follow,
            LocalNotificationType::FollowRequest,
            LocalNotificationType::Reblog,
            LocalNotificationType::Status,
            LocalNotificationType::Update,
            LocalNotificationType::Quote,
            LocalNotificationType::QuotedUpdate,
            LocalNotificationType::Poll,
            LocalNotificationType::AdminReport,
        ] {
            assert!(enabled.enabled(notification_type));
            assert!(!PushAlerts::default().enabled(notification_type));
        }
    }

    /// Domain moderation normalizes DNS names and applies parent rules to subdomains.
    #[test]
    fn federation_domain_rules_use_normalized_dns_suffixes() {
        assert_eq!(
            normalize_federation_domain(" Example.COM. ").unwrap(),
            "example.com"
        );
        assert_eq!(
            federation_domain_suffixes("social.team.example.com").unwrap(),
            ["social.team.example.com", "team.example.com", "example.com"]
        );
        for invalid in ["*", "localhost", "https://example.com", "user@example.com"] {
            assert!(normalize_federation_domain(invalid).is_err());
        }
    }

    /// Featured hashtag input is normalized while malformed and numeric-only names are rejected.
    #[test]
    fn validates_featured_hashtag_names() {
        assert_eq!(
            normalize_featured_tag_name(" #Rust_2026 ").as_deref(),
            Some("rust_2026")
        );
        assert_eq!(
            normalize_featured_tag_name("日本語").as_deref(),
            Some("日本語")
        );
        assert!(normalize_featured_tag_name("1234").is_none());
        assert!(normalize_featured_tag_name("two words").is_none());
        assert!(normalize_featured_tag_name("#").is_none());
    }

    /// Streaming edit events retain their database spelling independently of the wire name.
    #[test]
    fn streaming_event_kinds_use_stable_persisted_values() {
        assert_eq!(StreamingEventKind::Update.to_string(), "update");
        assert_eq!(
            StreamingEventKind::StatusUpdate.to_string(),
            "status_update"
        );
    }
}
