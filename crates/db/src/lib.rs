#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use rand_core::{OsRng, RngCore};
use roost_core::{AccountId, JobId, Result, RoostError};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, Database, DatabaseBackend, DatabaseConnection,
    EntityTrait, PaginatorTrait, QueryFilter, Set, Statement,
};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

mod entity {
    use sea_orm::entity::prelude::*;
    use time::OffsetDateTime;

    pub mod local_account {
        use super::*;

        #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
        #[sea_orm(table_name = "local_account")]
        pub struct Model {
            #[sea_orm(primary_key, auto_increment = false)]
            pub id: Uuid,
            pub username: String,
            pub email: String,
            pub password_hash: String,
            pub is_admin: bool,
            pub created_at: OffsetDateTime,
            pub updated_at: OffsetDateTime,
        }

        #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
        pub enum Relation {}

        impl ActiveModelBehavior for ActiveModel {}
    }

    pub mod oauth_application {
        use super::*;

        #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
        #[sea_orm(table_name = "oauth_application")]
        pub struct Model {
            #[sea_orm(primary_key, auto_increment = false)]
            pub id: Uuid,
            pub client_id: String,
            pub client_secret_hash: String,
            pub name: String,
            pub redirect_uri: String,
            pub scopes: String,
            pub website: Option<String>,
            pub created_at: OffsetDateTime,
        }

        #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
        pub enum Relation {}

        impl ActiveModelBehavior for ActiveModel {}
    }

    pub mod oauth_authorization_code {
        use super::*;

        #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
        #[sea_orm(table_name = "oauth_authorization_code")]
        pub struct Model {
            #[sea_orm(primary_key, auto_increment = false)]
            pub id: Uuid,
            pub code_hash: String,
            pub account_id: Uuid,
            pub application_id: Uuid,
            pub redirect_uri: String,
            pub scopes: String,
            pub code_challenge: String,
            pub code_challenge_method: String,
            pub expires_at: OffsetDateTime,
            pub consumed_at: Option<OffsetDateTime>,
            pub created_at: OffsetDateTime,
        }

        #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
        pub enum Relation {}

        impl ActiveModelBehavior for ActiveModel {}
    }

    pub mod oauth_access_token {
        use super::*;

        #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
        #[sea_orm(table_name = "oauth_access_token")]
        pub struct Model {
            #[sea_orm(primary_key, auto_increment = false)]
            pub id: Uuid,
            pub token_hash: String,
            pub account_id: Uuid,
            pub application_id: Uuid,
            pub scopes: String,
            pub issued_at: OffsetDateTime,
            pub expires_at: Option<OffsetDateTime>,
            pub revoked_at: Option<OffsetDateTime>,
        }

        #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
        pub enum Relation {}

        impl ActiveModelBehavior for ActiveModel {}
    }
}

/// Shared database connection type used across Roost crates.
pub type DbConnection = DatabaseConnection;

/// Open a database connection using SeaORM's PostgreSQL driver.
pub async fn connect(database_url: &str) -> Result<DbConnection> {
    Database::connect(database_url)
        .await
        .map_err(|error| RoostError::Database(error.to_string()))
}

/// Verify that the database connection can execute a trivial query.
pub async fn ping(db: &DbConnection) -> Result<()> {
    db.query_one(Statement::from_string(
        DatabaseBackend::Postgres,
        "SELECT 1".to_owned(),
    ))
    .await
    .map_err(|error| RoostError::Database(error.to_string()))?;

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
    let count = entity::local_account::Entity::find()
        .count(db)
        .await
        .map_err(|error| RoostError::Database(error.to_string()))?;
    if count != 0 {
        return Err(RoostError::InvalidInput(
            "bootstrap is only allowed before local accounts exist".to_owned(),
        ));
    }

    let account_id = Uuid::now_v7();
    entity::local_account::ActiveModel {
        id: Set(account_id),
        username: Set(username.to_owned()),
        email: Set(email.to_owned()),
        password_hash: Set(password_hash.to_owned()),
        is_admin: Set(true),
        ..Default::default()
    }
    .insert(db)
    .await
    .map_err(|error| RoostError::Database(error.to_string()))?;

    Ok(account_id)
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
    let account = entity::local_account::Entity::find()
        .filter(
            entity::local_account::Column::Username
                .eq(login)
                .or(entity::local_account::Column::Email.eq(login)),
        )
        .one(db)
        .await
        .map_err(|error| RoostError::Database(error.to_string()))?;

    Ok(account.map(local_account_from_model))
}

/// Find a local account by internal id.
pub async fn find_local_account_by_id(
    db: &DbConnection,
    account_id: AccountId,
) -> Result<Option<LocalAccount>> {
    let account = entity::local_account::Entity::find_by_id(account_id.0)
        .one(db)
        .await
        .map_err(|error| RoostError::Database(error.to_string()))?;

    Ok(account.map(local_account_from_model))
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

    entity::oauth_application::ActiveModel {
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
    .await
    .map_err(|error| RoostError::Database(error.to_string()))?;

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
    let app = entity::oauth_application::Entity::find()
        .filter(entity::oauth_application::Column::ClientId.eq(client_id))
        .one(db)
        .await
        .map_err(|error| RoostError::Database(error.to_string()))?;

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

    entity::oauth_authorization_code::ActiveModel {
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
    .await
    .map_err(|error| RoostError::Database(error.to_string()))?;

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
    let Some(code) = entity::oauth_authorization_code::Entity::find()
        .filter(entity::oauth_authorization_code::Column::CodeHash.eq(code_hash))
        .filter(entity::oauth_authorization_code::Column::ApplicationId.eq(application_id))
        .filter(entity::oauth_authorization_code::Column::RedirectUri.eq(redirect_uri))
        .filter(entity::oauth_authorization_code::Column::ConsumedAt.is_null())
        .one(db)
        .await
        .map_err(|error| RoostError::Database(error.to_string()))?
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
    let mut active_code: entity::oauth_authorization_code::ActiveModel = code.into();
    active_code.consumed_at = Set(Some(OffsetDateTime::now_utc()));
    active_code
        .update(db)
        .await
        .map_err(|error| RoostError::Database(error.to_string()))?;

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

    entity::oauth_access_token::ActiveModel {
        id: Set(Uuid::now_v7()),
        token_hash: Set(token_hash),
        account_id: Set(account_id.0),
        application_id: Set(application_id),
        scopes: Set(scopes.to_owned()),
        issued_at: Set(issued_at),
        ..Default::default()
    }
    .insert(db)
    .await
    .map_err(|error| RoostError::Database(error.to_string()))?;

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
    let Some(token) = entity::oauth_access_token::Entity::find()
        .filter(entity::oauth_access_token::Column::TokenHash.eq(token_hash))
        .filter(entity::oauth_access_token::Column::RevokedAt.is_null())
        .one(db)
        .await
        .map_err(|error| RoostError::Database(error.to_string()))?
    else {
        return Ok(None);
    };
    if token
        .expires_at
        .is_some_and(|expires_at| expires_at <= OffsetDateTime::now_utc())
    {
        return Ok(None);
    }

    let account = entity::local_account::Entity::find_by_id(token.account_id)
        .one(db)
        .await
        .map_err(|error| RoostError::Database(error.to_string()))?;

    Ok(account.map(|account| (local_account_from_model(account), token.scopes)))
}

/// Revoke an OAuth access token if it exists.
pub async fn revoke_access_token(db: &DbConnection, token_pepper: &str, token: &str) -> Result<()> {
    let token_hash = secret_hash(token_pepper, token)?;
    if let Some(token) = entity::oauth_access_token::Entity::find()
        .filter(entity::oauth_access_token::Column::TokenHash.eq(token_hash))
        .one(db)
        .await
        .map_err(|error| RoostError::Database(error.to_string()))?
    {
        let mut active_token: entity::oauth_access_token::ActiveModel = token.into();
        active_token.revoked_at = Set(Some(OffsetDateTime::now_utc()));
        active_token
            .update(db)
            .await
            .map_err(|error| RoostError::Database(error.to_string()))?;
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

fn local_account_from_model(account: entity::local_account::Model) -> LocalAccount {
    LocalAccount {
        id: AccountId(account.id),
        username: account.username,
        email: account.email,
        password_hash: account.password_hash,
        is_admin: account.is_admin,
    }
}

fn oauth_application_from_model(app: entity::oauth_application::Model) -> OAuthApplication {
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
        .await
        .map_err(|error| RoostError::Database(error.to_string()))?
        .ok_or_else(|| RoostError::Database("job enqueue returned no row".to_owned()))?;
    let id: Uuid = row
        .try_get("", "id")
        .map_err(|error| RoostError::Database(error.to_string()))?;

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
        .await
        .map_err(|error| RoostError::Database(error.to_string()))?;

    rows.into_iter()
        .map(|row| {
            let id: Uuid = row
                .try_get("", "id")
                .map_err(|error| RoostError::Database(error.to_string()))?;
            let kind: String = row
                .try_get("", "kind")
                .map_err(|error| RoostError::Database(error.to_string()))?;
            let payload: JsonValue = row
                .try_get("", "payload")
                .map_err(|error| RoostError::Database(error.to_string()))?;
            let attempts: i32 = row
                .try_get("", "attempts")
                .map_err(|error| RoostError::Database(error.to_string()))?;

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
    .await
    .map_err(|error| RoostError::Database(error.to_string()))?;

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
    .await
    .map_err(|error| RoostError::Database(error.to_string()))?;

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
        .await
        .map_err(|error| RoostError::Database(error.to_string()))?;

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
