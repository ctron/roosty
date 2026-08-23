//! Read-only projections and keyset queries used by search-engine sitemaps.

use sea_orm::{ConnectionTrait, DatabaseBackend, FromQueryResult, Statement, sea_query::Value};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::Result;

/// Maximum URL count in one sitemap document, as required by the sitemap protocol.
pub const SEARCH_SITEMAP_URL_LIMIT: i64 = 50_000;

/// One cursor-addressed sitemap chunk advertised by the sitemap index.
#[derive(Clone, Debug, FromQueryResult, Eq, PartialEq)]
pub struct SearchSitemapChunk {
    pub cursor: Uuid,
    pub last_modified: OffsetDateTime,
}

/// One eligible local profile URL projection.
#[derive(Clone, Debug, FromQueryResult, Eq, PartialEq)]
pub struct SearchProfileUrl {
    pub username: String,
    pub last_modified: OffsetDateTime,
}

/// One eligible public local-status URL projection.
#[derive(Clone, Debug, FromQueryResult, Eq, PartialEq)]
pub struct SearchStatusUrl {
    pub id: Uuid,
    pub username: String,
    pub last_modified: OffsetDateTime,
}

/// Return stable chunk boundaries for all eligible local profile URLs.
pub async fn search_profile_sitemap_chunks<C>(db: &C) -> Result<Vec<SearchSitemapChunk>>
where
    C: ConnectionTrait,
{
    search_sitemap_chunks(db, PROFILE_CHUNKS_SQL).await
}

/// Return stable chunk boundaries for eligible public, non-sensitive local statuses.
pub async fn search_status_sitemap_chunks<C>(db: &C) -> Result<Vec<SearchSitemapChunk>>
where
    C: ConnectionTrait,
{
    search_sitemap_chunks(db, STATUS_CHUNKS_SQL).await
}

async fn search_sitemap_chunks<C>(db: &C, sql: &str) -> Result<Vec<SearchSitemapChunk>>
where
    C: ConnectionTrait,
{
    Ok(
        SearchSitemapChunk::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            [Value::BigInt(Some(SEARCH_SITEMAP_URL_LIMIT))],
        ))
        .all(db)
        .await?,
    )
}

/// Read one protocol-sized profile sitemap page from an inclusive keyset cursor.
pub async fn search_profile_sitemap_urls<C>(db: &C, cursor: Uuid) -> Result<Vec<SearchProfileUrl>>
where
    C: ConnectionTrait,
{
    Ok(
        SearchProfileUrl::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            PROFILE_URLS_SQL,
            [cursor.into(), SEARCH_SITEMAP_URL_LIMIT.into()],
        ))
        .all(db)
        .await?,
    )
}

/// Read one protocol-sized status sitemap page from an inclusive keyset cursor.
pub async fn search_status_sitemap_urls<C>(db: &C, cursor: Uuid) -> Result<Vec<SearchStatusUrl>>
where
    C: ConnectionTrait,
{
    Ok(
        SearchStatusUrl::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            STATUS_URLS_SQL,
            [cursor.into(), SEARCH_SITEMAP_URL_LIMIT.into()],
        ))
        .all(db)
        .await?,
    )
}

const PROFILE_CHUNKS_SQL: &str = r#"
WITH numbered AS (
    SELECT id, updated_at, row_number() OVER (ORDER BY id) - 1 AS row_index
      FROM local_account
     WHERE discoverable AND limited_at IS NULL
       AND suspended_at IS NULL AND data_purged_at IS NULL
), summarized AS (
    SELECT id AS cursor, row_index,
           max(updated_at) OVER (PARTITION BY row_index / $1) AS last_modified
      FROM numbered
)
SELECT cursor, last_modified FROM summarized
 WHERE row_index % $1 = 0 ORDER BY cursor
"#;

const STATUS_CHUNKS_SQL: &str = r#"
WITH numbered AS (
    SELECT status.id, status.updated_at,
           row_number() OVER (ORDER BY status.id) - 1 AS row_index
      FROM local_status status
      JOIN local_account account ON account.id = status.account_id
     WHERE status.visibility = 'public' AND NOT status.sensitive
       AND status.deleted_at IS NULL AND account.discoverable
       AND account.limited_at IS NULL AND account.suspended_at IS NULL
       AND account.data_purged_at IS NULL
), summarized AS (
    SELECT id AS cursor, row_index,
           max(updated_at) OVER (PARTITION BY row_index / $1) AS last_modified
      FROM numbered
)
SELECT cursor, last_modified FROM summarized
 WHERE row_index % $1 = 0 ORDER BY cursor
"#;

const PROFILE_URLS_SQL: &str = r#"
SELECT username, updated_at AS last_modified FROM local_account
 WHERE id >= $1 AND discoverable AND limited_at IS NULL
   AND suspended_at IS NULL AND data_purged_at IS NULL
 ORDER BY id LIMIT $2
"#;

const STATUS_URLS_SQL: &str = r#"
SELECT status.id, account.username, status.updated_at AS last_modified
  FROM local_status status
  JOIN local_account account ON account.id = status.account_id
 WHERE status.id >= $1 AND status.visibility = 'public' AND NOT status.sensitive
   AND status.deleted_at IS NULL AND account.discoverable
   AND account.limited_at IS NULL AND account.suspended_at IS NULL
   AND account.data_purged_at IS NULL
 ORDER BY status.id LIMIT $2
"#;
