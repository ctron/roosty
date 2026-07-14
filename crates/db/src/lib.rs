#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use rand_core::{OsRng, RngCore};
use roosty_core::{AccountId, JobId, Result, RoostyError, StatusId};
use sea_orm::{
    ActiveModelTrait, ActiveValue, ColumnTrait, Condition, ConnectionTrait, Database,
    DatabaseBackend, DatabaseConnection, DbErr, EntityTrait, FromQueryResult, IntoActiveModel,
    PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Select, Set, Statement, TransactionTrait,
    TryInsertResult,
    sea_query::{OnConflict, Query},
};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};
use std::cmp::Reverse;
use std::str::FromStr;
use strum::{EnumString, IntoStaticStr};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

mod entity;

use entity::{
    job, local_account, local_account_block, local_account_mute, local_actor_key,
    local_conversation, local_conversation_account, local_follow, local_media_attachment,
    local_notification, local_status, local_status_bookmark, local_status_favourite,
    local_status_reblog, local_status_remote_mention, local_status_tag, local_tag,
    local_tag_follow, local_timeline_marker, oauth_access_token, oauth_application,
    oauth_authorization_code, processed_inbox_activity, remote_actor, remote_follow,
    remote_following, remote_status,
};

/// Shared database connection type used across Roosty crates.
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
        return Err(RoostyError::InvalidInput(
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
    /// Direct inbox URL.
    pub inbox_url: String,
    /// Optional shared inbox URL.
    pub shared_inbox_url: Option<String>,
    /// Public key identity URL.
    pub public_key_id: String,
    /// Public signing key PEM.
    pub public_key_pem: String,
    /// Cache expiry instant.
    pub expires_at: OffsetDateTime,
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
    pub visibility: String,
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
    /// Original Note object retained for future projection fields.
    pub object: JsonValue,
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
    pub visibility: String,
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
    pub state: String,
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
) -> Result<RemoteFollowing> {
    let row = remote_following::ActiveModel {
        id: Set(Uuid::now_v7()),
        local_account_id: Set(local_account_id.0),
        remote_actor_id: Set(remote_actor_id.0),
        activity_id: Set(activity_id.to_owned()),
        state: Set("pending".to_owned()),
        ..Default::default()
    }
    .insert(db)
    .await?;
    Ok(remote_following_from_model(row))
}

/// Find one local-to-remote follow relationship.
pub async fn find_remote_following(
    db: &DbConnection,
    local_account_id: AccountId,
    remote_actor_id: AccountId,
) -> Result<Option<RemoteFollowing>> {
    Ok(remote_following::Entity::find()
        .filter(remote_following::Column::LocalAccountId.eq(local_account_id.0))
        .filter(remote_following::Column::RemoteActorId.eq(remote_actor_id.0))
        .one(db)
        .await?
        .map(remote_following_from_model))
}

/// List local accounts whose accepted remote follow targets the supplied actor.
pub async fn accepted_local_followers_of_remote_actor(
    db: &DbConnection,
    remote_actor_id: AccountId,
) -> Result<Vec<AccountId>> {
    Ok(remote_following::Entity::find()
        .filter(remote_following::Column::RemoteActorId.eq(remote_actor_id.0))
        .filter(remote_following::Column::State.eq("accepted"))
        .all(db)
        .await?
        .into_iter()
        .map(|follow| AccountId(follow.local_account_id))
        .collect())
}

/// Return a page of local and remote accounts following one local account.
pub async fn followers_for_local_account(
    db: &DbConnection,
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
            .filter(remote_follow::Column::State.eq("accepted")),
        limit,
        cursor,
        |follow| (follow.id, AccountId(follow.follower_account_id)),
        |follow| (follow.id, AccountId(follow.remote_actor_id)),
    )
    .await
}

/// Return a page of local and accepted remote accounts followed by one local account.
pub async fn following_for_local_account(
    db: &DbConnection,
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
            .filter(remote_following::Column::State.eq("accepted")),
        limit,
        cursor,
        |follow| (follow.id, AccountId(follow.followed_account_id)),
        |follow| (follow.id, AccountId(follow.remote_actor_id)),
    )
    .await
}

/// Merge UUIDv7-ordered local and remote relationship rows into one cursor page.
async fn follow_collection_page<L, R, FL, FR>(
    db: &DbConnection,
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

fn collection_cursor_matches(id: Uuid, cursor: CollectionCursor) -> bool {
    cursor.max_id.is_none_or(|max_id| id < max_id)
        && cursor.since_id.is_none_or(|since_id| id > since_id)
        && cursor.min_id.is_none_or(|min_id| id > min_id)
}

/// Count accepted remote actors followed by this local account.
pub async fn count_remote_following(db: &DbConnection, account_id: AccountId) -> Result<u64> {
    Ok(remote_following::Entity::find()
        .filter(remote_following::Column::LocalAccountId.eq(account_id.0))
        .filter(remote_following::Column::State.eq("accepted"))
        .count(db)
        .await?)
}

/// Find one cached remote Note by its canonical ActivityPub ID.
pub async fn find_remote_status_by_activitypub_id(
    db: &DbConnection,
    activitypub_id: &str,
) -> Result<Option<RemoteStatus>> {
    Ok(remote_status::Entity::find()
        .filter(remote_status::Column::ActivitypubId.eq(activitypub_id))
        .filter(remote_status::Column::DeletedAt.is_null())
        .one(db)
        .await?
        .map(remote_status_from_model))
}

/// Find one active cached remote Note by its UUID-backed API identifier.
pub async fn find_remote_status_by_id(
    db: &DbConnection,
    status_id: StatusId,
) -> Result<Option<RemoteStatus>> {
    Ok(remote_status::Entity::find_by_id(status_id.0)
        .filter(remote_status::Column::DeletedAt.is_null())
        .one(db)
        .await?
        .map(remote_status_from_model))
}

/// Mark the locally initiated Follow identified by its activity ID as accepted.
pub async fn accept_remote_following(
    db: &DbConnection,
    remote_actor_id: AccountId,
    activity_id: &str,
) -> Result<bool> {
    let result = db.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres, "UPDATE remote_following SET state = 'accepted', updated_at = now() WHERE remote_actor_id = $1 AND activity_id = $2", vec![remote_actor_id.0.into(), activity_id.to_owned().into()])).await?;
    Ok(result.rows_affected() == 1)
}

/// Remove a rejected local-to-remote Follow by the original activity identity.
pub async fn reject_remote_following(
    db: &DbConnection,
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
    row.into_active_model().delete(db).await?;
    Ok(Some(relationship))
}

/// Insert or refresh a verified remote Note by its canonical ActivityPub object ID.
pub async fn upsert_remote_status(
    db: &DbConnection,
    status: NewRemoteStatus,
) -> Result<RemoteStatus> {
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
            object: Set(status.object),
            ..Default::default()
        }
        .insert(db)
        .await?
    };
    Ok(remote_status_from_model(model))
}

/// Soft-delete a remote Note only when its verified author owns it.
pub async fn delete_remote_status(
    db: &DbConnection,
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
    let mut active = status.into_active_model();
    active.deleted_at = Set(Some(OffsetDateTime::now_utc()));
    active.update(db).await?;
    Ok(true)
}

/// Find a remote actor by its canonical ActivityPub ID.
pub async fn find_remote_actor_by_activitypub_id(
    db: &DbConnection,
    activitypub_id: &str,
) -> Result<Option<RemoteActor>> {
    Ok(remote_actor::Entity::find()
        .filter(remote_actor::Column::ActivitypubId.eq(activitypub_id))
        .one(db)
        .await?
        .map(remote_actor_from_model))
}

/// Find a remote actor by its UUID-backed API identifier.
pub async fn find_remote_actor_by_id(
    db: &DbConnection,
    actor_id: AccountId,
) -> Result<Option<RemoteActor>> {
    Ok(remote_actor::Entity::find_by_id(actor_id.0)
        .one(db)
        .await?
        .map(remote_actor_from_model))
}

/// Find a remote actor by its canonical WebFinger handle.
pub async fn find_remote_actor_by_handle(
    db: &DbConnection,
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

/// Insert or refresh a remote actor cache entry by canonical actor ID.
pub async fn upsert_remote_actor(db: &DbConnection, actor: &RemoteActor) -> Result<RemoteActor> {
    let now = OffsetDateTime::now_utc();
    let existing = remote_actor::Entity::find()
        .filter(remote_actor::Column::ActivitypubId.eq(&actor.activitypub_id))
        .one(db)
        .await?;
    let model = if let Some(existing) = existing {
        let mut active = existing.into_active_model();
        active.username = Set(actor.username.clone());
        active.domain = Set(actor.domain.clone());
        active.display_name = Set(actor.display_name.clone());
        active.summary = Set(actor.summary.clone());
        active.inbox_url = Set(actor.inbox_url.clone());
        active.shared_inbox_url = Set(actor.shared_inbox_url.clone());
        active.public_key_id = Set(actor.public_key_id.clone());
        active.public_key_pem = Set(actor.public_key_pem.clone());
        active.fetched_at = Set(now);
        active.expires_at = Set(actor.expires_at);
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
            inbox_url: Set(actor.inbox_url.clone()),
            shared_inbox_url: Set(actor.shared_inbox_url.clone()),
            public_key_id: Set(actor.public_key_id.clone()),
            public_key_pem: Set(actor.public_key_pem.clone()),
            fetched_at: Set(now),
            expires_at: Set(actor.expires_at),
            ..Default::default()
        }
        .insert(db)
        .await?
    };
    Ok(remote_actor_from_model(model))
}

/// Look up the persisted ActivityPub signing key for a local account.
pub async fn find_local_actor_key(
    db: &DbConnection,
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
    db: &DbConnection,
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
}

/// Stored local hashtag metadata.
#[derive(Clone, Debug)]
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
    /// Number of local status uses on this day.
    pub uses: u64,
    /// Number of distinct local accounts using the tag on this day.
    pub accounts: u64,
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
    /// Optional cached remote parent status.
    pub in_reply_to_remote_status_id: Option<StatusId>,
}

/// Stored local direct-message conversation.
#[derive(Clone, Debug)]
pub struct LocalConversation {
    /// Internal conversation identifier.
    pub id: Uuid,
    /// Most recent status in the conversation, when still available.
    pub last_status_id: Option<StatusId>,
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

/// Filters supported by Mastodon's local hashtag timeline request.
#[derive(Clone, Debug, Default)]
pub struct LocalTagTimelineOptions {
    /// Return statuses that include at least one of these additional tags.
    pub any: Vec<String>,
    /// Return statuses that include every one of these additional tags.
    pub all: Vec<String>,
    /// Exclude statuses that include any of these tags.
    pub none: Vec<String>,
    /// Return only statuses with at least one media attachment.
    pub only_media: bool,
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
    /// A local account boosted one of the recipient's statuses.
    Reblog,
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

/// A home timeline row, either an authored status or a boost entry.
#[derive(Clone, Debug)]
pub enum HomeTimelineItem {
    /// Authored local status.
    Status(LocalStatus),
    /// Local boost of an authored status.
    Reblog(LocalStatusReblog),
    /// Cached status from an accepted remote follow.
    RemoteStatus(RemoteStatus),
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
    /// Creation timestamp.
    pub created_at: OffsetDateTime,
    /// Soft-dismiss timestamp.
    pub dismissed_at: Option<OffsetDateTime>,
}

/// Persisted inbound remote follow request or accepted remote follower.
#[derive(Clone, Debug)]
pub struct RemoteFollow {
    pub id: Uuid,
    pub remote_actor_id: AccountId,
    pub local_account_id: AccountId,
    pub activity_id: String,
    pub activity: JsonValue,
    pub state: String,
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
#[derive(Clone, Copy, Debug, EnumString, Eq, IntoStaticStr, PartialEq)]
#[strum(serialize_all = "snake_case")]
pub enum JobKind {
    /// Deliver an Accept or Reject response for an inbound remote Follow.
    FederationFollowResponse,
    /// Deliver a public or unlisted local status lifecycle activity.
    FederationStatusDelivery,
    /// Deliver a locally initiated Follow or Undo(Follow).
    FederationFollowDelivery,
}

impl JobKind {
    /// Return the persisted worker kind name.
    pub fn as_str(self) -> &'static str {
        self.into()
    }
}

#[derive(FromQueryResult)]
struct RemoteFollowRow {
    id: Uuid,
    remote_actor_id: Uuid,
    local_account_id: Uuid,
    activity_id: String,
    activity: JsonValue,
    state: String,
}

/// Store or refresh an inbound remote Follow request.
pub async fn upsert_remote_follow(
    db: &DbConnection,
    remote_actor_id: AccountId,
    local_account_id: AccountId,
    activity_id: &str,
    activity: JsonValue,
    state: &str,
) -> Result<RemoteFollow> {
    let row = RemoteFollowRow::find_by_statement(Statement::from_sql_and_values(DatabaseBackend::Postgres, r#"
        INSERT INTO remote_follow (id, remote_actor_id, local_account_id, activity_id, activity, state)
        VALUES ($1, $2, $3, $4, $5, $6)
        ON CONFLICT (remote_actor_id, local_account_id) DO UPDATE
        SET activity_id = EXCLUDED.activity_id, activity = EXCLUDED.activity, state = EXCLUDED.state, updated_at = now()
        RETURNING id, remote_actor_id, local_account_id, activity_id, activity, state
    "#, vec![Uuid::now_v7().into(), remote_actor_id.0.into(), local_account_id.0.into(), activity_id.to_owned().into(), activity.into(), state.to_owned().into()])).one(db).await?
        .ok_or_else(|| RoostyError::InvalidInput("remote follow could not be saved".to_owned()))?;
    Ok(remote_follow_from_row(row))
}

/// Persist one newly validated automatic remote Follow and its durable Accept job atomically.
pub async fn upsert_processed_remote_follow_with_response_job(
    db: &DbConnection,
    remote_actor_id: AccountId,
    local_account_id: AccountId,
    activity_id: &str,
    activity: JsonValue,
    response_job: RemoteFollowResponseJob,
) -> Result<bool> {
    let txn = db.begin().await?;
    let processed =
        processed_inbox_activity::Entity::insert(processed_inbox_activity::ActiveModel {
            activity_id: Set(activity_id.to_owned()),
            remote_actor_id: Set(remote_actor_id.0),
            processed_at: Set(OffsetDateTime::now_utc()),
        })
        .on_conflict_do_nothing()
        .exec(&txn)
        .await?;
    if matches!(processed, TryInsertResult::Conflicted) {
        txn.commit().await?;
        return Ok(false);
    }

    remote_follow::Entity::insert(remote_follow::ActiveModel {
        id: Set(Uuid::now_v7()),
        remote_actor_id: Set(remote_actor_id.0),
        local_account_id: Set(local_account_id.0),
        activity_id: Set(activity_id.to_owned()),
        activity: Set(activity),
        state: Set("accepted".to_owned()),
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
    .exec(&txn)
    .await?;
    insert_response_job(&txn, response_job).await?;
    txn.commit().await?;
    Ok(true)
}

/// Insert a deduplicated follow-response job within a caller-owned transaction.
async fn insert_response_job(
    txn: &sea_orm::DatabaseTransaction,
    response_job: RemoteFollowResponseJob,
) -> Result<()> {
    let _ = job::Entity::insert(job::ActiveModel {
        id: Set(Uuid::now_v7()),
        kind: Set(response_job.kind.as_str().to_owned()),
        payload: Set(response_job.payload),
        deduplication_key: Set(Some(response_job.deduplication_key)),
        run_after: Set(OffsetDateTime::now_utc()),
        attempts: Set(0),
        locked_at: Set(None),
        locked_by: Set(None),
        last_error: Set(None),
        created_at: Set(OffsetDateTime::now_utc()),
        completed_at: Set(None),
    })
    .on_conflict_do_nothing()
    .exec(txn)
    .await?;
    Ok(())
}

/// Accept a pending remote Follow and create its Accept delivery job atomically.
pub async fn accept_remote_follow_with_response_job(
    db: &DbConnection,
    local_account_id: AccountId,
    remote_actor_id: AccountId,
    activity_id: &str,
    response_job: RemoteFollowResponseJob,
) -> Result<bool> {
    let txn = db.begin().await?;
    let follow = remote_follow::Entity::find()
        .filter(remote_follow::Column::LocalAccountId.eq(local_account_id.0))
        .filter(remote_follow::Column::RemoteActorId.eq(remote_actor_id.0))
        .filter(remote_follow::Column::ActivityId.eq(activity_id))
        .filter(remote_follow::Column::State.eq("pending"))
        .one(&txn)
        .await?;
    let Some(follow) = follow else {
        txn.commit().await?;
        return Ok(false);
    };
    let mut follow = follow.into_active_model();
    follow.state = Set("accepted".to_owned());
    follow.updated_at = Set(OffsetDateTime::now_utc());
    follow.update(&txn).await?;
    insert_response_job(&txn, response_job).await?;
    txn.commit().await?;
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

/// Reject a pending remote Follow and create its Reject delivery job atomically.
pub async fn delete_remote_follow_with_response_job(
    db: &DbConnection,
    local_account_id: AccountId,
    remote_actor_id: AccountId,
    activity_id: &str,
    response_job: RemoteFollowResponseJob,
) -> Result<bool> {
    let txn = db.begin().await?;
    let follow = remote_follow::Entity::find()
        .filter(remote_follow::Column::LocalAccountId.eq(local_account_id.0))
        .filter(remote_follow::Column::RemoteActorId.eq(remote_actor_id.0))
        .filter(remote_follow::Column::ActivityId.eq(activity_id))
        .filter(remote_follow::Column::State.eq("pending"))
        .one(&txn)
        .await?;
    let Some(follow) = follow else {
        txn.commit().await?;
        return Ok(false);
    };
    follow.into_active_model().delete(&txn).await?;
    insert_response_job(&txn, response_job).await?;
    txn.commit().await?;
    Ok(true)
}

/// List pending remote follows for internal approval and rejection lookup.
pub async fn pending_remote_follows(
    db: &DbConnection,
    local_account_id: AccountId,
) -> Result<Vec<RemoteFollow>> {
    Ok(remote_follow::Entity::find()
        .filter(remote_follow::Column::LocalAccountId.eq(local_account_id.0))
        .filter(remote_follow::Column::State.eq("pending"))
        .order_by_desc(remote_follow::Column::Id)
        .all(db)
        .await?
        .into_iter()
        .map(remote_follow_from_model)
        .collect())
}

/// List pending remote follow-request actors for a local account with Mastodon cursor pagination.
pub async fn pending_remote_follow_requests(
    db: &DbConnection,
    local_account_id: AccountId,
    limit: u64,
    cursor: CollectionCursor,
) -> Result<CollectionPage<RemoteActor>> {
    let rows = remote_follow::Entity::find()
        .filter(remote_follow::Column::LocalAccountId.eq(local_account_id.0))
        .filter(remote_follow::Column::State.eq("pending"))
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
    db: &DbConnection,
    remote_actor_id: AccountId,
    local_account_id: AccountId,
) -> Result<bool> {
    Ok(db.query_one(Statement::from_sql_and_values(DatabaseBackend::Postgres, "SELECT 1 FROM remote_follow WHERE remote_actor_id = $1 AND local_account_id = $2 AND state = 'accepted'", vec![remote_actor_id.0.into(), local_account_id.0.into()])).await?.is_some())
}

/// Record a successfully validated inbox activity, returning false for duplicates.
pub async fn record_processed_inbox_activity(
    db: &DbConnection,
    activity_id: &str,
    remote_actor_id: AccountId,
) -> Result<bool> {
    let result = db.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres, "INSERT INTO processed_inbox_activity (activity_id, remote_actor_id) VALUES ($1, $2) ON CONFLICT DO NOTHING", vec![activity_id.to_owned().into(), remote_actor_id.0.into()])).await?;
    Ok(result.rows_affected() == 1)
}

/// Create a follow notification attributable to a remote actor.
pub async fn notify_remote_actor_follow(
    db: &DbConnection,
    account_id: AccountId,
    remote_actor_id: AccountId,
) -> Result<LocalNotification> {
    let model = local_notification::ActiveModel {
        id: Set(Uuid::now_v7()),
        account_id: Set(account_id.0),
        notification_type: Set("follow".to_owned()),
        actor_account_id: Set(None),
        remote_actor_id: Set(Some(remote_actor_id.0)),
        status_id: Set(None),
        remote_status_id: Set(None),
        created_at: Set(OffsetDateTime::now_utc()),
        dismissed_at: Set(None),
    }
    .insert(db)
    .await?;
    Ok(local_notification_from_model(model))
}

/// Create an idempotent mention notification caused by a cached remote Note.
pub async fn notify_remote_status_mention(
    db: &DbConnection,
    account_id: AccountId,
    remote_actor_id: AccountId,
    remote_status_id: StatusId,
) -> Result<LocalNotification> {
    let model = local_notification::ActiveModel {
        id: Set(Uuid::now_v7()),
        account_id: Set(account_id.0),
        notification_type: Set("mention".to_owned()),
        actor_account_id: Set(None),
        remote_actor_id: Set(Some(remote_actor_id.0)),
        status_id: Set(None),
        remote_status_id: Set(Some(remote_status_id.0)),
        created_at: Set(OffsetDateTime::now_utc()),
        dismissed_at: Set(None),
    }
    .insert(db)
    .await?;
    Ok(local_notification_from_model(model))
}

/// Timelines that support persisted Mastodon read markers.
#[derive(Clone, Copy, Debug, EnumString, Eq, IntoStaticStr, PartialEq)]
#[strum(serialize_all = "snake_case")]
pub enum LocalTimeline {
    /// The authenticated account's home timeline.
    Home,
    /// The authenticated account's notification timeline.
    Notifications,
}

impl LocalTimeline {
    /// Return the Mastodon wire value for this timeline.
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
    viewer_account_id: AccountId,
    query: &str,
    limit: u64,
    offset: u64,
) -> Result<Vec<LocalAccount>> {
    if query.trim().is_empty() || limit == 0 {
        return Ok(Vec::new());
    }

    let hidden_account_ids = blocked_local_account_ids_for_account(db, viewer_account_id).await?;
    let mut accounts = local_account::Entity::find().filter(
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

    Ok(accounts.into_iter().map(local_account_from_model).collect())
}

/// Count local accounts following this account.
pub async fn count_local_followers(db: &DbConnection, account_id: AccountId) -> Result<u64> {
    Ok(local_follow::Entity::find()
        .filter(local_follow::Column::FollowedAccountId.eq(account_id.0))
        .count(db)
        .await?)
}

/// Count accepted remote actors following this local account.
pub async fn count_remote_followers(db: &DbConnection, account_id: AccountId) -> Result<u64> {
    Ok(db.query_one(Statement::from_sql_and_values(DatabaseBackend::Postgres, "SELECT count(*) AS count FROM remote_follow WHERE local_account_id = $1 AND state = 'accepted'", vec![account_id.0.into()])).await?.map(|row| row.try_get::<i64>("", "count")).transpose()?.unwrap_or(0) as u64)
}

/// List accepted remote followers that must receive activities from a local actor.
pub async fn accepted_remote_followers(
    db: &DbConnection,
    account_id: AccountId,
) -> Result<Vec<RemoteActor>> {
    let rows = db
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT remote_actor_id FROM remote_follow WHERE local_account_id = $1 AND state = 'accepted'",
            vec![account_id.0.into()],
        ))
        .await?;
    let mut actors = Vec::with_capacity(rows.len());
    for row in rows {
        let id: Uuid = row.try_get("", "remote_actor_id")?;
        if let Some(actor) = find_remote_actor_by_id(db, AccountId(id)).await? {
            actors.push(actor);
        }
    }
    Ok(actors)
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

/// List local follower ids for streaming delivery.
pub async fn local_follower_ids_for_account(
    db: &DbConnection,
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

/// Follow a local account, updating follow options when it already exists.
pub async fn follow_local_account(
    db: &DbConnection,
    follower_account_id: AccountId,
    followed_account_id: AccountId,
    show_reblogs: bool,
    notify: bool,
) -> Result<LocalFollow> {
    if follower_account_id == followed_account_id {
        return Err(RoostyError::InvalidInput(
            "accounts cannot follow themselves".to_owned(),
        ));
    }
    if find_local_account_by_id(db, followed_account_id)
        .await?
        .is_none()
    {
        return Err(RoostyError::InvalidInput(
            "followed account does not exist".to_owned(),
        ));
    }
    if local_accounts_are_blocked(db, follower_account_id, followed_account_id).await? {
        return Err(RoostyError::InvalidInput(
            "follow is blocked by an account relationship".to_owned(),
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

/// Block a local account and sever any follow relationships between the accounts.
pub async fn block_local_account(
    db: &DbConnection,
    account_id: AccountId,
    target_account_id: AccountId,
) -> Result<()> {
    ensure_local_relation_target(db, account_id, target_account_id).await?;

    if local_account_block::Entity::find_by_id((account_id.0, target_account_id.0))
        .one(db)
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
        .insert(db)
        .await?;
    }

    unfollow_local_account(db, account_id, target_account_id).await?;
    unfollow_local_account(db, target_account_id, account_id).await
}

/// Remove a local account block when it exists.
pub async fn unblock_local_account(
    db: &DbConnection,
    account_id: AccountId,
    target_account_id: AccountId,
) -> Result<()> {
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
pub async fn mute_local_account(
    db: &DbConnection,
    account_id: AccountId,
    target_account_id: AccountId,
    notifications: bool,
    duration_seconds: u64,
) -> Result<LocalAccountMute> {
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
pub async fn unmute_local_account(
    db: &DbConnection,
    account_id: AccountId,
    target_account_id: AccountId,
) -> Result<()> {
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
    db: &DbConnection,
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
    db: &DbConnection,
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
    db: &DbConnection,
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
    db: &DbConnection,
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
    db: &DbConnection,
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
    db: &DbConnection,
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
    db: &DbConnection,
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

/// Validate that a local relation has an existing, distinct target account.
async fn ensure_local_relation_target(
    db: &DbConnection,
    account_id: AccountId,
    target_account_id: AccountId,
) -> Result<()> {
    if account_id == target_account_id {
        return Err(RoostyError::InvalidInput(
            "accounts cannot moderate themselves".to_owned(),
        ));
    }
    if find_local_account_by_id(db, target_account_id)
        .await?
        .is_none()
    {
        return Err(RoostyError::InvalidInput(
            "target account does not exist".to_owned(),
        ));
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
        return Err(RoostyError::InvalidInput(
            "accounts cannot notify themselves".to_owned(),
        ));
    }
    let type_value = notification_type.as_str();
    let status_uuid = status_id.map(|id| id.0);
    if let Some(existing) = local_notification::Entity::find()
        .filter(local_notification::Column::AccountId.eq(account_id.0))
        .filter(local_notification::Column::NotificationType.eq(type_value))
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
        notification_type: Set(type_value.to_owned()),
        actor_account_id: Set(Some(actor_account_id.0)),
        remote_actor_id: Set(None),
        status_id: Set(status_uuid),
        remote_status_id: Set(None),
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
        .limit(page_query_limit(limit));

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

/// Return saved timeline markers for an account and a requested set of timelines.
pub async fn local_timeline_markers_for_account(
    db: &DbConnection,
    account_id: AccountId,
    timelines: &[LocalTimeline],
) -> Result<Vec<LocalTimelineMarker>> {
    if timelines.is_empty() {
        return Ok(Vec::new());
    }

    let markers = local_timeline_marker::Entity::find()
        .filter(local_timeline_marker::Column::AccountId.eq(account_id.0))
        .filter(
            local_timeline_marker::Column::Timeline
                .is_in(timelines.iter().map(|timeline| timeline.as_str())),
        )
        .all(db)
        .await?;

    markers
        .into_iter()
        .map(local_timeline_marker_from_model)
        .collect()
}

/// Save a local account's read position for a Mastodon timeline.
pub async fn save_local_timeline_marker(
    db: &DbConnection,
    account_id: AccountId,
    timeline: LocalTimeline,
    last_read_id: Uuid,
) -> Result<LocalTimelineMarker> {
    let now = OffsetDateTime::now_utc();
    let marker =
        local_timeline_marker::Entity::find_by_id((account_id.0, timeline.as_str().to_owned()))
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
                timeline: Set(timeline.as_str().to_owned()),
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
pub async fn update_local_account_settings(
    db: &DbConnection,
    account_id: AccountId,
    update: LocalAccountSettingsUpdate,
) -> Result<LocalAccount> {
    let account = local_account::Entity::find_by_id(account_id.0)
        .one(db)
        .await?
        .ok_or_else(|| RoostyError::InvalidInput("local account does not exist".to_owned()))?;
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

/// Replace a local account password hash by its stable account identifier.
pub async fn update_local_account_password_hash_by_id(
    db: &DbConnection,
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

    Ok(local_account_from_model(active.update(db).await?))
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
        in_reply_to_remote_status_id: Set(new_status.in_reply_to_remote_status_id.map(|id| id.0)),
        conversation_id: Set(None),
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
            return Err(RoostyError::InvalidInput(
                "media attachment not found".to_owned(),
            ));
        };
        if media.account_id != account_id.0 || media.status_id.is_some() {
            return Err(RoostyError::InvalidInput(
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
        in_reply_to_remote_status_id: Set(new_status.in_reply_to_remote_status_id.map(|id| id.0)),
        conversation_id: Set(None),
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
            return Err(RoostyError::InvalidInput(
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

/// Replace all local hashtag links for one status, creating tag rows as needed.
pub async fn replace_local_status_tags(
    db: &DbConnection,
    status_id: StatusId,
    tag_names: &[String],
) -> Result<()> {
    let txn = db.begin().await?;
    local_status_tag::Entity::delete_many()
        .filter(local_status_tag::Column::StatusId.eq(status_id.0))
        .exec(&txn)
        .await?;

    let now = OffsetDateTime::now_utc();
    let mut names = tag_names.to_vec();
    names.sort();
    names.dedup();

    for name in names {
        let tag = find_or_create_local_tag(&txn, &name, now).await?;
        local_status_tag::ActiveModel {
            status_id: Set(status_id.0),
            tag_id: Set(tag.id),
            created_at: Set(now),
        }
        .insert(&txn)
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
    db: &DbConnection,
    status_id: StatusId,
) -> Result<Vec<RemoteActor>> {
    let rows = local_status_remote_mention::Entity::find()
        .filter(local_status_remote_mention::Column::StatusId.eq(status_id.0))
        .all(db)
        .await?;
    let mut actors = Vec::with_capacity(rows.len());
    for row in rows {
        if let Some(actor) = remote_actor::Entity::find_by_id(row.remote_actor_id)
            .one(db)
            .await?
        {
            actors.push(remote_actor_from_model(actor));
        }
    }
    Ok(actors)
}

/// List tags attached to a local status in normalized name order.
pub async fn local_tags_for_status(
    db: &DbConnection,
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

/// Search local tags by normalized prefix with offset pagination.
pub async fn search_local_tags(
    db: &DbConnection,
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
pub async fn find_local_tag_by_name(db: &DbConnection, name: &str) -> Result<Option<LocalTag>> {
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
    db: &DbConnection,
    account_id: AccountId,
    name: &str,
) -> Result<LocalTag> {
    let txn = db.begin().await?;
    let now = OffsetDateTime::now_utc();
    let tag = find_or_create_local_tag(&txn, name, now).await?;

    let existing = local_tag_follow::Entity::find()
        .filter(local_tag_follow::Column::AccountId.eq(account_id.0))
        .filter(local_tag_follow::Column::TagId.eq(tag.id))
        .one(&txn)
        .await?;
    match existing {
        Some(follow) => {
            let mut active = follow.into_active_model();
            active.updated_at = Set(now);
            active.update(&txn).await?;
        }
        None => {
            local_tag_follow::ActiveModel {
                account_id: Set(account_id.0),
                tag_id: Set(tag.id),
                created_at: Set(now),
                updated_at: Set(now),
            }
            .insert(&txn)
            .await?;
        }
    }

    txn.commit().await?;
    Ok(local_tag_from_model(tag))
}

/// Stop following a local hashtag for one account and return the local tag when it exists.
pub async fn unfollow_local_tag(
    db: &DbConnection,
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
    db: &DbConnection,
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
    db: &DbConnection,
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

/// Return recent local usage history for a tag.
pub async fn local_tag_history(db: &DbConnection, tag_id: Uuid) -> Result<Vec<LocalTagHistory>> {
    let rows = local_status_tag::Entity::find()
        .filter(local_status_tag::Column::TagId.eq(tag_id))
        .all(db)
        .await?;
    let mut buckets = std::collections::BTreeMap::<u64, (u64, Vec<Uuid>)>::new();

    for row in rows {
        let Some(status) = local_status::Entity::find_by_id(row.status_id)
            .filter(local_status::Column::DeletedAt.is_null())
            .one(db)
            .await?
        else {
            continue;
        };
        let day = (status.created_at.unix_timestamp() / 86_400 * 86_400).max(0) as u64;
        let (uses, accounts) = buckets.entry(day).or_default();
        *uses += 1;
        accounts.push(status.account_id);
    }

    let mut history = buckets
        .into_iter()
        .rev()
        .take(7)
        .map(|(day, (uses, mut accounts))| {
            accounts.sort();
            accounts.dedup();
            LocalTagHistory {
                day,
                uses,
                accounts: accounts.len() as u64,
            }
        })
        .collect::<Vec<_>>();
    history.sort_by_key(|bucket| std::cmp::Reverse(bucket.day));

    Ok(history)
}

/// List public local statuses containing a tag and optional tag filters.
pub async fn local_tag_timeline(
    db: &DbConnection,
    tag: &str,
    options: LocalTagTimelineOptions,
    limit: u64,
    cursor: TimelineCursor,
) -> Result<TimelinePage<LocalStatus>> {
    let Some(primary) = find_local_tag_by_name(db, tag).await? else {
        return Ok(TimelinePage {
            items: Vec::new(),
            first_cursor: None,
            last_cursor: None,
            has_more: false,
        });
    };
    let mut query = local_status::Entity::find()
        .filter(local_status::Column::Visibility.eq("public"))
        .filter(local_status::Column::DeletedAt.is_null())
        .filter(local_status::Column::Id.in_subquery(status_tag_subquery(primary.id)));

    for tag in &options.all {
        if let Some(tag) = find_local_tag_by_name(db, tag).await? {
            query = query.filter(local_status::Column::Id.in_subquery(status_tag_subquery(tag.id)));
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
    if !options.any.is_empty() {
        if any_tags.is_empty() {
            return Ok(TimelinePage {
                items: Vec::new(),
                first_cursor: None,
                last_cursor: None,
                has_more: false,
            });
        }
        query = query.filter(local_status::Column::Id.in_subquery(status_tags_subquery(
            any_tags.iter().map(|tag| tag.id).collect(),
        )));
    }

    let none_tags = local_tags_by_names(db, &options.none).await?;
    if !none_tags.is_empty() {
        query = query.filter(
            local_status::Column::Id.not_in_subquery(status_tags_subquery(
                none_tags.iter().map(|tag| tag.id).collect(),
            )),
        );
    }

    if options.only_media {
        query = query.filter(local_status::Column::Id.in_subquery(media_status_subquery()));
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
        items: statuses.into_iter().map(local_status_from_model).collect(),
        first_cursor,
        last_cursor,
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

    Ok(local_tag::ActiveModel {
        id: Set(Uuid::now_v7()),
        name: Set(name),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(db)
    .await?)
}

async fn local_tags_by_names(db: &DbConnection, names: &[String]) -> Result<Vec<LocalTag>> {
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
                return Err(RoostyError::InvalidInput(
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

/// List an actor's public statuses for its ActivityPub outbox.
pub async fn public_local_statuses_by_account(
    db: &DbConnection,
    account_id: AccountId,
    limit: u64,
) -> Result<Vec<LocalStatus>> {
    let statuses = local_status::Entity::find()
        .filter(local_status::Column::AccountId.eq(account_id.0))
        .filter(local_status::Column::Visibility.eq("public"))
        .filter(local_status::Column::DeletedAt.is_null())
        .order_by_desc(local_status::Column::CreatedAt)
        .limit(limit)
        .all(db)
        .await?;
    Ok(statuses.into_iter().map(local_status_from_model).collect())
}

/// Count an actor's public statuses for its ActivityPub outbox metadata.
pub async fn count_public_local_statuses_by_account(
    db: &DbConnection,
    account_id: AccountId,
) -> Result<u64> {
    Ok(local_status::Entity::find()
        .filter(local_status::Column::AccountId.eq(account_id.0))
        .filter(local_status::Column::Visibility.eq("public"))
        .filter(local_status::Column::DeletedAt.is_null())
        .count(db)
        .await?)
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

/// Attach a direct status to a local conversation and update participant views.
pub async fn attach_direct_status_to_conversation(
    db: &DbConnection,
    status_id: StatusId,
    author_id: AccountId,
    parent_id: Option<StatusId>,
    participant_ids: &[AccountId],
) -> Result<Uuid> {
    let txn = db.begin().await?;
    let now = OffsetDateTime::now_utc();
    let parent_conversation_id = match parent_id {
        Some(parent_id) => local_status::Entity::find_by_id(parent_id.0)
            .one(&txn)
            .await?
            .and_then(|status| status.conversation_id),
        None => None,
    };
    let conversation_id = match parent_conversation_id {
        Some(conversation_id) => conversation_id,
        None => {
            local_conversation::ActiveModel {
                id: Set(Uuid::now_v7()),
                last_status_id: Set(Some(status_id.0)),
                created_at: Set(now),
                updated_at: Set(now),
            }
            .insert(&txn)
            .await?
            .id
        }
    };

    let mut status = local_status::Entity::find_by_id(status_id.0)
        .one(&txn)
        .await?
        .ok_or_else(|| RoostyError::InvalidInput("conversation status not found".to_owned()))?
        .into_active_model();
    status.conversation_id = Set(Some(conversation_id));
    status.updated_at = Set(now);
    status.update(&txn).await?;

    let mut conversation = local_conversation::Entity::find_by_id(conversation_id)
        .one(&txn)
        .await?
        .ok_or_else(|| RoostyError::InvalidInput("conversation not found".to_owned()))?
        .into_active_model();
    conversation.last_status_id = Set(Some(status_id.0));
    conversation.updated_at = Set(now);
    conversation.update(&txn).await?;

    let existing_participants = local_conversation_account::Entity::find()
        .filter(local_conversation_account::Column::ConversationId.eq(conversation_id))
        .all(&txn)
        .await?;
    let mut account_ids = existing_participants
        .iter()
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
                active.updated_at = Set(now);
                active.update(&txn).await?;
            }
            None => {
                local_conversation_account::ActiveModel {
                    id: Set(Uuid::now_v7()),
                    cursor_id: Set(Uuid::now_v7()),
                    conversation_id: Set(conversation_id),
                    account_id: Set(account_id.0),
                    unread: Set(unread),
                    hidden_at: Set(None),
                    created_at: Set(now),
                    updated_at: Set(now),
                }
                .insert(&txn)
                .await?;
            }
        }
    }

    txn.commit().await?;
    Ok(conversation_id)
}

/// Return whether an account participates in a status's direct conversation.
pub async fn local_status_visible_to_account(
    db: &DbConnection,
    status: &LocalStatus,
    account_id: AccountId,
) -> Result<bool> {
    if matches!(status.visibility.as_str(), "public" | "unlisted")
        || status.account_id == account_id
    {
        return Ok(true);
    }
    if status.visibility != "direct" {
        return Ok(false);
    }
    let Some(conversation_id) = status.conversation_id else {
        return Ok(false);
    };

    Ok(local_conversation_account::Entity::find()
        .filter(local_conversation_account::Column::ConversationId.eq(conversation_id))
        .filter(local_conversation_account::Column::AccountId.eq(account_id.0))
        .one(db)
        .await?
        .is_some())
}

/// List visible local direct conversations for an account.
pub async fn local_conversations_for_account(
    db: &DbConnection,
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
    db: &DbConnection,
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

/// Mark a local conversation as read for one account.
pub async fn mark_local_conversation_read(
    db: &DbConnection,
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
    db: &DbConnection,
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
    db: &DbConnection,
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
    db: &DbConnection,
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

    Ok(local_status_reblog_from_model(model))
}

/// Remove a local account's boost from a status when it exists.
pub async fn unreblog_local_status(
    db: &DbConnection,
    account_id: AccountId,
    status_id: StatusId,
) -> Result<Option<LocalStatusReblog>> {
    if let Some(model) = local_status_reblog::Entity::find_by_id((account_id.0, status_id.0))
        .one(db)
        .await?
    {
        let reblog = local_status_reblog_from_model(model.clone());
        model.into_active_model().delete(db).await?;
        return Ok(Some(reblog));
    }

    Ok(None)
}

/// Count active local boosts on a status.
pub async fn count_local_reblogs(db: &DbConnection, status_id: StatusId) -> Result<u64> {
    Ok(local_status_reblog::Entity::find()
        .filter(local_status_reblog::Column::StatusId.eq(status_id.0))
        .count(db)
        .await?)
}

/// Return whether a local account has boosted a status.
pub async fn is_local_status_reblogged(
    db: &DbConnection,
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

/// List local boost rows for an original status.
pub async fn local_reblogs_for_status(
    db: &DbConnection,
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
        return Err(RoostyError::InvalidInput(
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
) -> Result<TimelinePage<LocalStatus>> {
    let statuses = apply_timeline_cursor(
        local_status::Entity::find()
            .filter(local_status::Column::Visibility.eq("public"))
            .filter(local_status::Column::DeletedAt.is_null()),
        cursor,
    )
    .order_by_desc(local_status::Column::Id)
    .limit(page_query_limit(limit))
    .all(db)
    .await?;
    let (statuses, has_more) = trim_to_page(statuses, limit);
    let first_cursor = statuses.first().map(|status| status.id);
    let last_cursor = statuses.last().map(|status| status.id);

    Ok(TimelinePage {
        items: statuses.into_iter().map(local_status_from_model).collect(),
        first_cursor,
        last_cursor,
        has_more,
    })
}

/// List statuses visible on an account's profile timeline.
pub async fn local_statuses_by_account(
    db: &DbConnection,
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
        query = query.filter(local_status::Column::Visibility.is_in(["public", "unlisted"]));
    }
    if options.exclude_replies {
        query = query.filter(local_status::Column::InReplyToId.is_null());
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
        items: statuses.into_iter().map(local_status_from_model).collect(),
        first_cursor,
        last_cursor,
        has_more,
    })
}

/// List statuses authored by the account and followed local accounts.
pub async fn home_timeline_for_account(
    db: &DbConnection,
    account_id: AccountId,
    limit: u64,
    cursor: TimelineCursor,
) -> Result<TimelinePage<HomeTimelineItem>> {
    let hidden_account_ids = hidden_local_account_ids_for_account(db, account_id)
        .await?
        .into_iter()
        .map(|account_id| account_id.0)
        .collect::<Vec<_>>();
    let follows = local_follow::Entity::find()
        .filter(local_follow::Column::FollowerAccountId.eq(account_id.0))
        .all(db)
        .await?;
    let followed_ids = follows
        .iter()
        .map(|follow| follow.followed_account_id)
        .collect::<Vec<_>>();
    let reblog_followed_ids = follows
        .iter()
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
                .add(local_status::Column::Visibility.is_in(["public", "unlisted"])),
        );
    if !followed_tag_ids.is_empty() {
        status_condition = status_condition.add(
            Condition::all()
                .add(local_status::Column::Visibility.eq("public"))
                .add(local_status::Column::Id.in_subquery(status_tags_subquery(followed_tag_ids))),
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
            .filter(local_status_reblog::Column::AccountId.is_in(reblog_account_ids)),
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
    let mut remote_query = remote_status::Entity::find()
        .filter(
            remote_status::Column::RemoteActorId.in_subquery(
                Query::select()
                    .column(remote_following::Column::RemoteActorId)
                    .from(remote_following::Entity)
                    .and_where(remote_following::Column::LocalAccountId.eq(account_id.0))
                    .and_where(remote_following::Column::State.eq("accepted"))
                    .to_owned(),
            ),
        )
        .filter(remote_status::Column::DeletedAt.is_null());
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
    let mut items = statuses
        .into_iter()
        .map(local_status_from_model)
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
                .map(remote_status_from_model)
                .map(HomeTimelineItem::RemoteStatus),
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
        .map_err(|error| RoostyError::InvalidInput(error.to_string()))?;
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

/// Convert a persisted remote actor cache model into the shared projection.
fn remote_actor_from_model(actor: remote_actor::Model) -> RemoteActor {
    RemoteActor {
        id: AccountId(actor.id),
        activitypub_id: actor.activitypub_id,
        username: actor.username,
        domain: actor.domain,
        display_name: actor.display_name,
        summary: actor.summary,
        inbox_url: actor.inbox_url,
        shared_inbox_url: actor.shared_inbox_url,
        public_key_id: actor.public_key_id,
        public_key_pem: actor.public_key_pem,
        expires_at: actor.expires_at,
    }
}

/// Convert a persisted remote Note cache model into the shared projection.
fn remote_status_from_model(status: remote_status::Model) -> RemoteStatus {
    RemoteStatus {
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
        object: status.object,
    }
}

fn remote_following_from_model(follow: remote_following::Model) -> RemoteFollowing {
    RemoteFollowing {
        local_account_id: AccountId(follow.local_account_id),
        remote_actor_id: AccountId(follow.remote_actor_id),
        activity_id: follow.activity_id,
        state: follow.state,
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
        in_reply_to_remote_status_id: status.in_reply_to_remote_status_id.map(StatusId),
        conversation_id: status.conversation_id,
        created_at: status.created_at,
        updated_at: status.updated_at,
        deleted_at: status.deleted_at,
    }
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
        notification_type: LocalNotificationType::from_str(&notification.notification_type)
            .unwrap_or(LocalNotificationType::Mention),
        actor_account_id: notification.actor_account_id.map(AccountId),
        remote_actor_id: notification.remote_actor_id.map(AccountId),
        status_id: notification.status_id.map(StatusId),
        remote_status_id: notification.remote_status_id.map(StatusId),
        created_at: notification.created_at,
        dismissed_at: notification.dismissed_at,
    }
}

/// Convert a SeaORM timeline marker row into its database API representation.
fn local_timeline_marker_from_model(
    marker: local_timeline_marker::Model,
) -> Result<LocalTimelineMarker> {
    Ok(LocalTimelineMarker {
        timeline: LocalTimeline::from_str(&marker.timeline).map_err(|_| {
            RoostyError::InvalidInput("stored timeline marker type is invalid".to_owned())
        })?,
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
    pub attempts: u32,
    /// Time the job was first enqueued.
    pub created_at: OffsetDateTime,
}

/// Enqueue a durable job, reusing an active deduplicated job when present.
pub async fn enqueue_job(
    db: &DbConnection,
    kind: JobKind,
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
                kind.as_str().to_owned().into(),
                payload.into(),
                deduplication_key.map(str::to_owned).into(),
                run_after.into(),
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
            RETURNING id, kind, payload, attempts, created_at
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
            let attempts = u32::try_from(attempts).map_err(|_| {
                RoostyError::InvalidInput("stored job attempts must not be negative".to_owned())
            })?;
            let created_at: OffsetDateTime = row.try_get("", "created_at")?;

            Ok(ClaimedJob {
                id: JobId(id),
                kind,
                payload,
                attempts,
                created_at,
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
    attempts: u32,
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

/// Mark a job as permanently failed while retaining its diagnostic error.
pub async fn mark_job_permanently_failed(
    db: &DbConnection,
    job_id: JobId,
    error: &str,
) -> Result<()> {
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "UPDATE job SET last_error = $2, completed_at = now(), locked_at = NULL, locked_by = NULL WHERE id = $1",
        vec![job_id.0.into(), error.to_owned().into()],
    )).await?;
    Ok(())
}

/// Return whether a job has exceeded its configured retry age.
pub fn job_has_exceeded_max_age(created_at: OffsetDateTime, max_age: Duration) -> bool {
    OffsetDateTime::now_utc() - created_at >= max_age
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
pub fn next_retry_at(attempts: u32) -> OffsetDateTime {
    let exponent = attempts.min(12);
    let seconds = 2_i64.pow(exponent).min(3_600);
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
        assert!(late - now <= Duration::hours(1) + Duration::seconds(1));
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
    }
}
