#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use rand_core::{OsRng, RngCore};
use roost_core::{AccountId, JobId, Result, RoostError, StatusId};
use sea_orm::{
    ActiveModelTrait, ActiveValue, ColumnTrait, Condition, ConnectionTrait, Database,
    DatabaseBackend, DatabaseConnection, DbErr, EntityTrait, IntoActiveModel, PaginatorTrait,
    QueryFilter, QueryOrder, QuerySelect, Select, Set, Statement, TransactionTrait,
};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};
use std::str::FromStr;
use strum::{EnumString, IntoStaticStr};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

mod entity;

use entity::{
    local_account, local_follow, local_media_attachment, local_notification, local_status,
    local_status_bookmark, local_status_favourite, oauth_access_token, oauth_application,
    oauth_authorization_code,
};

/// Shared database connection type used across Roost crates.
pub type DbConnection = DatabaseConnection;

/// Open a database connection using SeaORM's PostgreSQL driver.
pub async fn connect(database_url: &str) -> Result<DbConnection> {
    Ok(Database::connect(database_url).await?)
}

/// Verify that the database connection can execute a trivial query.
pub async fn ping(db: &DbConnection) -> Result<()> {
    db.query_one(Statement::from_string(
        DatabaseBackend::Postgres,
        "SELECT 1".to_owned(),
    ))
    .await?;

    Ok(())
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
    let count = local_account::Entity::find().count(db).await?;
    if count != 0 {
        return Err(RoostError::InvalidInput(
            "bootstrap is only allowed before local accounts exist".to_owned(),
        ));
    }

    insert_local_account(db, username, email, password_hash, true).await
}

/// Create a non-admin local account.
pub async fn create_local_account(
    db: &DbConnection,
    username: &str,
    email: &str,
    password_hash: &str,
) -> Result<Uuid> {
    insert_local_account(db, username, email, password_hash, false).await
}

/// Create an administrator local account after bootstrap.
pub async fn create_admin_account(
    db: &DbConnection,
    username: &str,
    email: &str,
    password_hash: &str,
) -> Result<Uuid> {
    insert_local_account(db, username, email, password_hash, true).await
}

/// Insert a local account after checking user-facing unique account fields.
async fn insert_local_account(
    db: &DbConnection,
    username: &str,
    email: &str,
    password_hash: &str,
    is_admin: bool,
) -> Result<Uuid> {
    ensure_local_account_available(db, username, email).await?;

    let account_id = Uuid::now_v7();
    local_account::ActiveModel {
        id: Set(account_id),
        username: Set(username.to_owned()),
        email: Set(email.to_owned()),
        password_hash: Set(password_hash.to_owned()),
        is_admin: Set(is_admin),
        ..Default::default()
    }
    .insert(db)
    .await?;

    Ok(account_id)
}

/// Reject account creation when the requested username or email is already in use.
async fn ensure_local_account_available(
    db: &DbConnection,
    username: &str,
    email: &str,
) -> Result<()> {
    if local_account::Entity::find()
        .filter(local_account::Column::Username.eq(username))
        .one(db)
        .await?
        .is_some()
    {
        return Err(RoostError::InvalidInput(
            "username is already in use".to_owned(),
        ));
    }

    if local_account::Entity::find()
        .filter(local_account::Column::Email.eq(email))
        .one(db)
        .await?
        .is_some()
    {
        return Err(RoostError::InvalidInput(
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
    pub default_visibility: String,
    /// Whether authored statuses are sensitive by default.
    pub default_sensitive: bool,
    /// Default language for authored statuses.
    pub default_language: Option<String>,
    /// Default quote policy for authored statuses.
    pub default_quote_policy: String,
    /// Profile metadata fields.
    pub profile_fields: JsonValue,
    /// Optional local avatar path relative to the media root.
    pub avatar_file_path: Option<String>,
    /// Optional local header image path relative to the media root.
    pub header_file_path: Option<String>,
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
    pub default_visibility: Option<String>,
    /// Whether authored statuses are sensitive by default.
    pub default_sensitive: Option<bool>,
    /// Default language for authored statuses.
    pub default_language: Option<Option<String>>,
    /// Default quote policy for authored statuses.
    pub default_quote_policy: Option<String>,
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
    pub visibility: String,
    /// Whether the status is marked sensitive.
    pub sensitive: bool,
    /// Optional content warning text.
    pub spoiler_text: String,
    /// Optional BCP-47 language tag.
    pub language: Option<String>,
    /// Optional local status this status replies to.
    pub in_reply_to_id: Option<StatusId>,
    /// Creation timestamp.
    pub created_at: OffsetDateTime,
    /// Last update timestamp.
    pub updated_at: OffsetDateTime,
    /// Soft-delete timestamp.
    pub deleted_at: Option<OffsetDateTime>,
}

/// Data accepted when creating a local status.
#[derive(Clone, Debug)]
pub struct NewLocalStatus {
    /// Authoring local account identifier.
    pub account_id: AccountId,
    /// Plain text status content.
    pub content: String,
    /// Mastodon status visibility value.
    pub visibility: String,
    /// Whether the status is marked sensitive.
    pub sensitive: bool,
    /// Optional content warning text.
    pub spoiler_text: String,
    /// Optional BCP-47 language tag.
    pub language: Option<String>,
    /// Optional local status this status replies to.
    pub in_reply_to_id: Option<StatusId>,
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
}

/// Supported local Mastodon notification kinds.
#[derive(Clone, Copy, Debug, EnumString, Eq, IntoStaticStr, PartialEq)]
#[strum(serialize_all = "snake_case")]
pub enum LocalNotificationType {
    /// A local status mentioned the recipient.
    Mention,
    /// A local account favourited one of the recipient's statuses.
    Favourite,
    /// A local account followed the recipient.
    Follow,
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
    pub actor_account_id: AccountId,
    /// Related local status for mention and favourite notifications.
    pub status_id: Option<StatusId>,
    /// Creation timestamp.
    pub created_at: OffsetDateTime,
    /// Soft-dismiss timestamp.
    pub dismissed_at: Option<OffsetDateTime>,
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
    pub token_type: &'static str,
    /// Space-separated scopes granted to the token.
    pub scope: String,
    /// Unix timestamp for token issuance.
    pub created_at: i64,
}

/// Find a local account by username or email for password login.
pub async fn find_local_account_by_login(
    db: &DbConnection,
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

    Ok(account.map(local_account_from_model))
}

/// Find a local account by internal id.
pub async fn find_local_account_by_id(
    db: &DbConnection,
    account_id: AccountId,
) -> Result<Option<LocalAccount>> {
    let account = local_account::Entity::find_by_id(account_id.0)
        .one(db)
        .await?;

    Ok(account.map(local_account_from_model))
}

/// Find a local account by its exact local username.
pub async fn find_local_account_by_username(
    db: &DbConnection,
    username: &str,
) -> Result<Option<LocalAccount>> {
    let account = local_account::Entity::find()
        .filter(local_account::Column::Username.eq(username))
        .one(db)
        .await?;

    Ok(account.map(local_account_from_model))
}

/// Search local accounts by username or display name for Mastodon autocomplete.
pub async fn search_local_accounts(
    db: &DbConnection,
    query: &str,
    limit: u64,
    offset: u64,
) -> Result<Vec<LocalAccount>> {
    if query.trim().is_empty() || limit == 0 {
        return Ok(Vec::new());
    }

    let accounts = local_account::Entity::find()
        .filter(
            local_account::Column::Username
                .contains(query)
                .or(local_account::Column::DisplayName.contains(query)),
        )
        .order_by_asc(local_account::Column::Username)
        .limit(limit)
        .offset(offset)
        .all(db)
        .await?;

    Ok(accounts.into_iter().map(local_account_from_model).collect())
}

/// Count local accounts following this account.
pub async fn count_local_followers(db: &DbConnection, account_id: AccountId) -> Result<u64> {
    Ok(local_follow::Entity::find()
        .filter(local_follow::Column::FollowedAccountId.eq(account_id.0))
        .count(db)
        .await?)
}

/// Count local accounts this account follows.
pub async fn count_local_following(db: &DbConnection, account_id: AccountId) -> Result<u64> {
    Ok(local_follow::Entity::find()
        .filter(local_follow::Column::FollowerAccountId.eq(account_id.0))
        .count(db)
        .await?)
}

/// Return whether one local account follows another.
pub async fn local_follow_relationship(
    db: &DbConnection,
    follower_account_id: AccountId,
    followed_account_id: AccountId,
) -> Result<Option<LocalFollow>> {
    let follow = local_follow::Entity::find_by_id((follower_account_id.0, followed_account_id.0))
        .one(db)
        .await?;

    Ok(follow.map(local_follow_from_model))
}

/// Follow a local account, updating follow options when it already exists.
pub async fn follow_local_account(
    db: &DbConnection,
    follower_account_id: AccountId,
    followed_account_id: AccountId,
    show_reblogs: bool,
    notify: bool,
) -> Result<LocalFollow> {
    if follower_account_id == followed_account_id {
        return Err(RoostError::InvalidInput(
            "accounts cannot follow themselves".to_owned(),
        ));
    }
    if find_local_account_by_id(db, followed_account_id)
        .await?
        .is_none()
    {
        return Err(RoostError::InvalidInput(
            "followed account does not exist".to_owned(),
        ));
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
    db: &DbConnection,
    follower_account_id: AccountId,
    followed_account_id: AccountId,
) -> Result<()> {
    if let Some(model) =
        local_follow::Entity::find_by_id((follower_account_id.0, followed_account_id.0))
            .one(db)
            .await?
    {
        model.into_active_model().delete(db).await?;
    }

    Ok(())
}

/// Create or return an existing local notification for one logical event.
pub async fn notify_local_account(
    db: &DbConnection,
    account_id: AccountId,
    notification_type: LocalNotificationType,
    actor_account_id: AccountId,
    status_id: Option<StatusId>,
) -> Result<LocalNotification> {
    if account_id == actor_account_id {
        return Err(RoostError::InvalidInput(
            "accounts cannot notify themselves".to_owned(),
        ));
    }
    let type_value = notification_type.as_str();
    let status_uuid = status_id.map(|id| id.0);
    if let Some(existing) = local_notification::Entity::find()
        .filter(local_notification::Column::AccountId.eq(account_id.0))
        .filter(local_notification::Column::NotificationType.eq(type_value))
        .filter(local_notification::Column::ActorAccountId.eq(actor_account_id.0))
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
        notification_type: Set(type_value.to_owned()),
        actor_account_id: Set(actor_account_id.0),
        status_id: Set(status_uuid),
        created_at: Set(OffsetDateTime::now_utc()),
        dismissed_at: Set(None),
    }
    .insert(db)
    .await?;

    Ok(local_notification_from_model(model))
}

/// List visible local notifications for one recipient with Mastodon cursor filters.
pub async fn local_notifications_for_account(
    db: &DbConnection,
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
        .limit(limit);

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
        query = query.filter(local_notification::Column::ActorAccountId.eq(actor_id.0));
    }

    let rows = query.all(db).await?;
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
    })
}

/// Find one visible local notification belonging to a recipient.
pub async fn find_local_notification_for_account(
    db: &DbConnection,
    account_id: AccountId,
    notification_id: Uuid,
) -> Result<Option<LocalNotification>> {
    let notification = local_notification::Entity::find_by_id(notification_id)
        .filter(local_notification::Column::AccountId.eq(account_id.0))
        .filter(local_notification::Column::DismissedAt.is_null())
        .one(db)
        .await?;

    Ok(notification.map(local_notification_from_model))
}

/// Dismiss one visible local notification for a recipient.
pub async fn dismiss_local_notification(
    db: &DbConnection,
    account_id: AccountId,
    notification_id: Uuid,
) -> Result<bool> {
    let Some(model) = local_notification::Entity::find_by_id(notification_id)
        .filter(local_notification::Column::AccountId.eq(account_id.0))
        .filter(local_notification::Column::DismissedAt.is_null())
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
pub async fn clear_local_notifications(db: &DbConnection, account_id: AccountId) -> Result<()> {
    let notifications = local_notification::Entity::find()
        .filter(local_notification::Column::AccountId.eq(account_id.0))
        .filter(local_notification::Column::DismissedAt.is_null())
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
        .limit(limit)
        .all(db)
        .await?;
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
        .limit(limit)
        .all(db)
        .await?;
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
    })
}

/// Update mutable local account settings and return the refreshed account.
pub async fn update_local_account_settings(
    db: &DbConnection,
    account_id: AccountId,
    update: LocalAccountSettingsUpdate,
) -> Result<LocalAccount> {
    let account = local_account::Entity::find_by_id(account_id.0)
        .one(db)
        .await?
        .ok_or_else(|| RoostError::InvalidInput("local account does not exist".to_owned()))?;
    let mut active = account.into_active_model();

    set_if_some(&mut active.display_name, update.display_name);
    set_if_some(&mut active.note, update.note);
    set_if_some(&mut active.locked, update.locked);
    set_if_some(&mut active.bot, update.bot);
    set_if_some(&mut active.discoverable, update.discoverable);
    set_if_some(&mut active.default_visibility, update.default_visibility);
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

    Ok(local_account_from_model(active.update(db).await?))
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

    Ok(Some(local_account_from_model(active.update(db).await?)))
}

/// Create a local status authored by an account on this instance.
pub async fn create_local_status(
    db: &DbConnection,
    new_status: NewLocalStatus,
) -> Result<LocalStatus> {
    let status_id = Uuid::now_v7();
    let created_at = OffsetDateTime::now_utc();

    let status = local_status::ActiveModel {
        id: Set(status_id),
        account_id: Set(new_status.account_id.0),
        content: Set(new_status.content),
        visibility: Set(new_status.visibility),
        sensitive: Set(new_status.sensitive),
        spoiler_text: Set(new_status.spoiler_text),
        language: Set(new_status.language),
        in_reply_to_id: Set(new_status.in_reply_to_id.map(|id| id.0)),
        created_at: Set(created_at),
        updated_at: Set(created_at),
        deleted_at: Set(None),
    }
    .insert(db)
    .await?;

    Ok(local_status_from_model(status))
}

/// Create a local status and attach pre-uploaded local media in one transaction.
pub async fn create_local_status_with_media(
    db: &DbConnection,
    new_status: NewLocalStatus,
    media_ids: &[Uuid],
) -> Result<LocalStatus> {
    if media_ids.is_empty() {
        return create_local_status(db, new_status).await;
    }

    let txn = db.begin().await?;
    let status_id = Uuid::now_v7();
    let created_at = OffsetDateTime::now_utc();
    let account_id = new_status.account_id;

    for media_id in media_ids {
        let Some(media) = local_media_attachment::Entity::find_by_id(*media_id)
            .one(&txn)
            .await?
        else {
            return Err(RoostError::InvalidInput(
                "media attachment not found".to_owned(),
            ));
        };
        if media.account_id != account_id.0 || media.status_id.is_some() {
            return Err(RoostError::InvalidInput(
                "media attachment is not available".to_owned(),
            ));
        }
    }

    let status = local_status::ActiveModel {
        id: Set(status_id),
        account_id: Set(account_id.0),
        content: Set(new_status.content),
        visibility: Set(new_status.visibility),
        sensitive: Set(new_status.sensitive),
        spoiler_text: Set(new_status.spoiler_text),
        language: Set(new_status.language),
        in_reply_to_id: Set(new_status.in_reply_to_id.map(|id| id.0)),
        created_at: Set(created_at),
        updated_at: Set(created_at),
        deleted_at: Set(None),
    }
    .insert(&txn)
    .await?;

    for (index, media_id) in media_ids.iter().enumerate() {
        let Some(media) = local_media_attachment::Entity::find_by_id(*media_id)
            .one(&txn)
            .await?
        else {
            return Err(RoostError::InvalidInput(
                "media attachment not found".to_owned(),
            ));
        };
        let mut active = media.into_active_model();
        active.status_id = Set(Some(status_id));
        active.status_order = Set(index as i32);
        active.updated_at = Set(OffsetDateTime::now_utc());
        active.update(&txn).await?;
    }

    txn.commit().await?;
    Ok(local_status_from_model(status))
}

/// Update an owned local status and its attached media metadata.
pub async fn update_owned_local_status(
    db: &DbConnection,
    status_id: StatusId,
    account_id: AccountId,
    update: LocalStatusUpdate,
    media_ids: Option<&[Uuid]>,
    media_attributes: &[LocalStatusMediaAttributeUpdate],
) -> Result<Option<LocalStatus>> {
    let txn = db.begin().await?;
    let Some(status) = local_status::Entity::find_by_id(status_id.0)
        .filter(local_status::Column::AccountId.eq(account_id.0))
        .filter(local_status::Column::DeletedAt.is_null())
        .one(&txn)
        .await?
    else {
        return Ok(None);
    };

    if let Some(media_ids) = media_ids {
        for media_id in media_ids {
            let Some(media) = local_media_attachment::Entity::find_by_id(*media_id)
                .one(&txn)
                .await?
            else {
                return Err(RoostError::InvalidInput(
                    "media attachment not found".to_owned(),
                ));
            };
            let available = media
                .status_id
                .is_none_or(|existing| existing == status_id.0);
            if media.account_id != account_id.0 || !available {
                return Err(RoostError::InvalidInput(
                    "media attachment is not available".to_owned(),
                ));
            }
        }

        let keep = media_ids.to_vec();
        let current = local_media_attachment::Entity::find()
            .filter(local_media_attachment::Column::StatusId.eq(status_id.0))
            .all(&txn)
            .await?;
        for media in current {
            if !keep.contains(&media.id) {
                let mut active = media.into_active_model();
                active.status_id = Set(None);
                active.status_order = Set(0);
                active.updated_at = Set(OffsetDateTime::now_utc());
                active.update(&txn).await?;
            }
        }

        for (index, media_id) in media_ids.iter().enumerate() {
            let Some(media) = local_media_attachment::Entity::find_by_id(*media_id)
                .one(&txn)
                .await?
            else {
                return Err(RoostError::InvalidInput(
                    "media attachment not found".to_owned(),
                ));
            };
            let mut active = media.into_active_model();
            active.status_id = Set(Some(status_id.0));
            active.status_order = Set(index as i32);
            active.updated_at = Set(OffsetDateTime::now_utc());
            active.update(&txn).await?;
        }
    }

    for attribute in media_attributes {
        let Some(media) = local_media_attachment::Entity::find_by_id(attribute.media_id)
            .filter(local_media_attachment::Column::AccountId.eq(account_id.0))
            .filter(local_media_attachment::Column::StatusId.eq(status_id.0))
            .one(&txn)
            .await?
        else {
            return Err(RoostError::InvalidInput(
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
        active.update(&txn).await?;
    }

    let mut active = status.into_active_model();
    set_if_some(&mut active.content, update.content);
    set_if_some(&mut active.sensitive, update.sensitive);
    set_if_some(&mut active.spoiler_text, update.spoiler_text);
    set_if_some(&mut active.language, update.language);
    active.updated_at = Set(OffsetDateTime::now_utc());
    let status = active.update(&txn).await?;

    txn.commit().await?;
    Ok(Some(local_status_from_model(status)))
}

/// Create local media metadata after the uploaded file has been stored.
pub async fn create_local_media_attachment(
    db: &DbConnection,
    media: NewLocalMediaAttachment,
) -> Result<LocalMediaAttachment> {
    let now = OffsetDateTime::now_utc();
    let model = local_media_attachment::ActiveModel {
        id: Set(Uuid::now_v7()),
        account_id: Set(media.account_id.0),
        status_id: Set(None),
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
    db: &DbConnection,
    account_id: AccountId,
    media_id: Uuid,
) -> Result<Option<LocalMediaAttachment>> {
    let media = local_media_attachment::Entity::find_by_id(media_id)
        .filter(local_media_attachment::Column::AccountId.eq(account_id.0))
        .filter(local_media_attachment::Column::StatusId.is_null())
        .one(db)
        .await?;

    Ok(media.map(local_media_attachment_from_model))
}

/// Update mutable fields on an unattached media attachment owned by a local account.
pub async fn update_owned_unattached_media_attachment(
    db: &DbConnection,
    account_id: AccountId,
    media_id: Uuid,
    update: LocalMediaAttachmentUpdate,
) -> Result<Option<LocalMediaAttachment>> {
    let Some(media) = local_media_attachment::Entity::find_by_id(media_id)
        .filter(local_media_attachment::Column::AccountId.eq(account_id.0))
        .filter(local_media_attachment::Column::StatusId.is_null())
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
    db: &DbConnection,
    account_id: AccountId,
    media_id: Uuid,
) -> Result<Option<LocalMediaAttachment>> {
    let Some(media) = local_media_attachment::Entity::find_by_id(media_id)
        .filter(local_media_attachment::Column::AccountId.eq(account_id.0))
        .filter(local_media_attachment::Column::StatusId.is_null())
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
    db: &DbConnection,
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

/// Return whether a local status has at least one media attachment.
pub async fn local_status_has_media(db: &DbConnection, status_id: StatusId) -> Result<bool> {
    Ok(local_media_attachment::Entity::find()
        .filter(local_media_attachment::Column::StatusId.eq(status_id.0))
        .count(db)
        .await?
        > 0)
}

/// Find a local status by id, excluding soft-deleted statuses.
pub async fn find_local_status_by_id(
    db: &DbConnection,
    status_id: StatusId,
) -> Result<Option<LocalStatus>> {
    let status = local_status::Entity::find_by_id(status_id.0)
        .filter(local_status::Column::DeletedAt.is_null())
        .one(db)
        .await?;

    Ok(status.map(local_status_from_model))
}

/// Count active statuses authored by a local account.
pub async fn count_local_statuses_by_account(
    db: &DbConnection,
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
    db: &DbConnection,
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

/// Count active local replies to a status.
pub async fn count_local_replies(db: &DbConnection, status_id: StatusId) -> Result<u64> {
    Ok(local_status::Entity::find()
        .filter(local_status::Column::InReplyToId.eq(status_id.0))
        .filter(local_status::Column::DeletedAt.is_null())
        .count(db)
        .await?)
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

    Ok(statuses.into_iter().map(local_status_from_model).collect())
}

/// Mark a local status as favourited by an account.
pub async fn favourite_local_status(
    db: &DbConnection,
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
    }

    Ok(())
}

/// Remove a local account's favourite from a status when it exists.
pub async fn unfavourite_local_status(
    db: &DbConnection,
    account_id: AccountId,
    status_id: StatusId,
) -> Result<()> {
    if let Some(model) = local_status_favourite::Entity::find_by_id((account_id.0, status_id.0))
        .one(db)
        .await?
    {
        model.into_active_model().delete(db).await?;
    }

    Ok(())
}

/// Count active local favourites on a status.
pub async fn count_local_favourites(db: &DbConnection, status_id: StatusId) -> Result<u64> {
    Ok(local_status_favourite::Entity::find()
        .filter(local_status_favourite::Column::StatusId.eq(status_id.0))
        .count(db)
        .await?)
}

/// Return whether a local account has favourited a status.
pub async fn is_local_status_favourited(
    db: &DbConnection,
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
        .limit(limit)
        .all(db)
        .await?;
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
    })
}

/// Mark a local status as bookmarked by an account.
pub async fn bookmark_local_status(
    db: &DbConnection,
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
    db: &DbConnection,
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
    db: &DbConnection,
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
    db: &DbConnection,
    account_id: AccountId,
    limit: u64,
    cursor: CollectionCursor,
) -> Result<CollectionPage<LocalStatus>> {
    let rows = local_status_bookmark::Entity::find()
        .filter(local_status_bookmark::Column::AccountId.eq(account_id.0))
        .apply_collection_cursor(cursor)
        .order_by_desc(local_status_bookmark::Column::Id)
        .limit(limit)
        .all(db)
        .await?;
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
    })
}

/// Load active local statuses for ordered status identifiers.
async fn active_statuses_by_id(
    db: &DbConnection,
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
    db: &DbConnection,
    account_ids: Vec<AccountId>,
) -> Result<Vec<LocalAccount>> {
    let mut accounts = Vec::with_capacity(account_ids.len());
    for account_id in account_ids {
        if let Some(account) = find_local_account_by_id(db, account_id).await? {
            accounts.push(account);
        }
    }

    Ok(accounts)
}

/// Soft-delete a local status when the authenticated account owns it.
pub async fn delete_owned_local_status(
    db: &DbConnection,
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
        return Err(RoostError::InvalidInput(
            "status is owned by another account".to_owned(),
        ));
    }

    let mut active = status.into_active_model();
    active.deleted_at = Set(Some(OffsetDateTime::now_utc()));
    active.updated_at = Set(OffsetDateTime::now_utc());

    Ok(Some(local_status_from_model(active.update(db).await?)))
}

/// List public local statuses for the public timeline.
pub async fn public_local_timeline(
    db: &DbConnection,
    limit: u64,
    cursor: TimelineCursor,
) -> Result<Vec<LocalStatus>> {
    let statuses = apply_timeline_cursor(
        local_status::Entity::find()
            .filter(local_status::Column::Visibility.eq("public"))
            .filter(local_status::Column::DeletedAt.is_null()),
        cursor,
    )
    .order_by_desc(local_status::Column::Id)
    .limit(limit)
    .all(db)
    .await?;

    Ok(statuses.into_iter().map(local_status_from_model).collect())
}

/// List statuses visible on an account's profile timeline.
pub async fn local_statuses_by_account(
    db: &DbConnection,
    account_id: AccountId,
    viewer: Option<AccountId>,
    limit: u64,
    cursor: TimelineCursor,
) -> Result<Vec<LocalStatus>> {
    let owner = viewer.is_some_and(|viewer| viewer == account_id);
    let mut query = local_status::Entity::find()
        .filter(local_status::Column::AccountId.eq(account_id.0))
        .filter(local_status::Column::DeletedAt.is_null());
    if !owner {
        query = query.filter(local_status::Column::Visibility.is_in(["public", "unlisted"]));
    }

    let statuses = apply_timeline_cursor(query, cursor)
        .order_by_desc(local_status::Column::Id)
        .limit(limit)
        .all(db)
        .await?;

    Ok(statuses.into_iter().map(local_status_from_model).collect())
}

/// List statuses authored by the account and followed local accounts.
pub async fn home_timeline_for_account(
    db: &DbConnection,
    account_id: AccountId,
    limit: u64,
    cursor: TimelineCursor,
) -> Result<Vec<LocalStatus>> {
    let followed_ids = local_follow::Entity::find()
        .filter(local_follow::Column::FollowerAccountId.eq(account_id.0))
        .all(db)
        .await?
        .into_iter()
        .map(|follow| follow.followed_account_id)
        .collect::<Vec<_>>();

    let statuses = apply_timeline_cursor(
        local_status::Entity::find()
            .filter(
                Condition::any()
                    .add(local_status::Column::AccountId.eq(account_id.0))
                    .add(
                        Condition::all()
                            .add(local_status::Column::AccountId.is_in(followed_ids))
                            .add(local_status::Column::Visibility.is_in(["public", "unlisted"])),
                    ),
            )
            .filter(local_status::Column::DeletedAt.is_null()),
        cursor,
    )
    .order_by_desc(local_status::Column::Id)
    .limit(limit)
    .all(db)
    .await?;

    Ok(statuses.into_iter().map(local_status_from_model).collect())
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
    db: &DbConnection,
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
    db: &DbConnection,
    client_id: &str,
) -> Result<Option<OAuthApplication>> {
    let app = oauth_application::Entity::find()
        .filter(oauth_application::Column::ClientId.eq(client_id))
        .one(db)
        .await?;

    Ok(app.map(oauth_application_from_model))
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
    pub code_challenge_method: &'a str,
}

/// Create a one-time OAuth authorization code.
pub async fn create_authorization_code(
    db: &DbConnection,
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
    db: &DbConnection,
    token_pepper: &str,
    code: &str,
    application_id: Uuid,
    redirect_uri: &str,
) -> Result<Option<(AccountId, String, String, String)>> {
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
        code.code_challenge_method.clone(),
    );
    let mut active_code = code.into_active_model();
    active_code.consumed_at = Set(Some(OffsetDateTime::now_utc()));
    active_code.update(db).await?;

    Ok(Some(grant))
}

/// Create and persist a hashed opaque OAuth access token.
pub async fn create_access_token(
    db: &DbConnection,
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
        token_type: "Bearer",
        scope: scopes.to_owned(),
        created_at: issued_at.unix_timestamp(),
    })
}

/// Resolve a raw OAuth access token to its local account and granted scopes.
pub async fn find_account_by_access_token(
    db: &DbConnection,
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
        .one(db)
        .await?;

    Ok(account.map(|account| (local_account_from_model(account), token.scopes)))
}

/// Revoke an OAuth access token if it exists.
pub async fn revoke_access_token(db: &DbConnection, token_pepper: &str, token: &str) -> Result<()> {
    let token_hash = secret_hash(token_pepper, token)?;
    if let Some(token) = oauth_access_token::Entity::find()
        .filter(oauth_access_token::Column::TokenHash.eq(token_hash))
        .one(db)
        .await?
    {
        let mut active_token = token.into_active_model();
        active_token.revoked_at = Set(Some(OffsetDateTime::now_utc()));
        active_token.update(db).await?;
    }

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
        .map_err(|error| RoostError::InvalidInput(error.to_string()))?;
    mac.update(secret.as_bytes());
    Ok(URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
}

/// Compute the OAuth PKCE S256 challenge for a verifier.
pub fn pkce_s256_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn local_account_from_model(account: local_account::Model) -> LocalAccount {
    LocalAccount {
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

fn local_status_from_model(status: local_status::Model) -> LocalStatus {
    LocalStatus {
        id: StatusId(status.id),
        account_id: AccountId(status.account_id),
        content: status.content,
        visibility: status.visibility,
        sensitive: status.sensitive,
        spoiler_text: status.spoiler_text,
        language: status.language,
        in_reply_to_id: status.in_reply_to_id.map(StatusId),
        created_at: status.created_at,
        updated_at: status.updated_at,
        deleted_at: status.deleted_at,
    }
}

fn local_notification_from_model(notification: local_notification::Model) -> LocalNotification {
    LocalNotification {
        id: notification.id,
        account_id: AccountId(notification.account_id),
        notification_type: LocalNotificationType::from_str(&notification.notification_type)
            .unwrap_or(LocalNotificationType::Mention),
        actor_account_id: AccountId(notification.actor_account_id),
        status_id: notification.status_id.map(StatusId),
        created_at: notification.created_at,
        dismissed_at: notification.dismissed_at,
    }
}

fn local_media_attachment_from_model(media: local_media_attachment::Model) -> LocalMediaAttachment {
    LocalMediaAttachment {
        id: media.id,
        account_id: AccountId(media.account_id),
        status_id: media.status_id.map(StatusId),
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
    /// Application job kind.
    pub kind: String,
    /// JSON job payload.
    pub payload: JsonValue,
    /// Number of prior failed attempts.
    pub attempts: i32,
}

/// Enqueue a durable job, reusing an active deduplicated job when present.
pub async fn enqueue_job(
    db: &DbConnection,
    kind: &str,
    payload: JsonValue,
    deduplication_key: Option<&str>,
    run_after: OffsetDateTime,
) -> Result<JobId> {
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
                kind.to_owned().into(),
                payload.into(),
                deduplication_key.map(str::to_owned).into(),
                run_after.into(),
            ],
        ))
        .await?
        .ok_or_else(|| {
            RoostError::from(DbErr::RecordNotFound(
                "job enqueue returned no row".to_owned(),
            ))
        })?;
    let id: Uuid = row.try_get("", "id")?;

    Ok(JobId(id))
}

/// Claim due jobs using PostgreSQL row locking.
pub async fn claim_due_jobs(
    db: &DbConnection,
    worker_id: &str,
    limit: u64,
    claim_ttl: Duration,
) -> Result<Vec<ClaimedJob>> {
    let expired_before = OffsetDateTime::now_utc() - claim_ttl;
    let rows = db
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
            UPDATE job
            SET locked_at = now(), locked_by = $1
            WHERE id IN (
                SELECT id
                FROM job
                WHERE completed_at IS NULL
                  AND run_after <= now()
                  AND (locked_at IS NULL OR locked_at < $2)
                ORDER BY run_after, created_at
                LIMIT $3
                FOR UPDATE SKIP LOCKED
            )
            RETURNING id, kind, payload, attempts
            "#,
            vec![
                worker_id.to_owned().into(),
                expired_before.into(),
                (limit as i64).into(),
            ],
        ))
        .await?;

    rows.into_iter()
        .map(|row| {
            let id: Uuid = row.try_get("", "id")?;
            let kind: String = row.try_get("", "kind")?;
            let payload: JsonValue = row.try_get("", "payload")?;
            let attempts: i32 = row.try_get("", "attempts")?;

            Ok(ClaimedJob {
                id: JobId(id),
                kind,
                payload,
                attempts,
            })
        })
        .collect()
}

/// Mark a claimed job as completed.
pub async fn mark_job_completed(db: &DbConnection, job_id: JobId) -> Result<()> {
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"
        UPDATE job
        SET completed_at = now(), locked_at = NULL, locked_by = NULL
        WHERE id = $1
        "#,
        vec![job_id.0.into()],
    ))
    .await?;

    Ok(())
}

/// Mark a job failed, release its claim, and schedule its next retry.
pub async fn mark_job_failed(
    db: &DbConnection,
    job_id: JobId,
    error: &str,
    attempts: i32,
) -> Result<OffsetDateTime> {
    let run_after = next_retry_at(attempts);
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"
        UPDATE job
        SET attempts = attempts + 1,
            last_error = $2,
            run_after = $3,
            locked_at = NULL,
            locked_by = NULL
        WHERE id = $1
        "#,
        vec![job_id.0.into(), error.to_owned().into(), run_after.into()],
    ))
    .await?;

    Ok(run_after)
}

/// Release job claims older than the configured claim TTL.
pub async fn release_expired_claims(db: &DbConnection, claim_ttl: Duration) -> Result<u64> {
    let expired_before = OffsetDateTime::now_utc() - claim_ttl;
    let result = db
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
            UPDATE job
            SET locked_at = NULL, locked_by = NULL
            WHERE completed_at IS NULL AND locked_at < $1
            "#,
            vec![expired_before.into()],
        ))
        .await?;

    Ok(result.rows_affected())
}

/// Calculate the next retry timestamp for a failed job.
pub fn next_retry_at(attempts: i32) -> OffsetDateTime {
    let exponent = attempts.clamp(0, 8) as u32;
    let seconds = 2_i64.pow(exponent);
    OffsetDateTime::now_utc() + Duration::seconds(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(late - now <= Duration::seconds(256 + 1));
    }
}
