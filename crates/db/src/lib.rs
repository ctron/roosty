use roost_core::{JobId, Result, RoostError};
use sea_orm::{ConnectionTrait, Database, DatabaseBackend, DatabaseConnection, Statement};
use serde_json::Value as JsonValue;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

pub type DbConnection = DatabaseConnection;

pub async fn connect(database_url: &str) -> Result<DbConnection> {
    Database::connect(database_url)
        .await
        .map_err(|error| RoostError::Database(error.to_string()))
}

pub async fn ping(db: &DbConnection) -> Result<()> {
    db.query_one(Statement::from_string(
        DatabaseBackend::Postgres,
        "SELECT 1".to_owned(),
    ))
    .await
    .map_err(|error| RoostError::Database(error.to_string()))?;

    Ok(())
}

pub async fn create_bootstrap_admin(
    db: &DbConnection,
    username: &str,
    email: &str,
    password_hash: &str,
) -> Result<Uuid> {
    let existing = db
        .query_one(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT COUNT(*) AS count FROM local_account".to_owned(),
        ))
        .await
        .map_err(|error| RoostError::Database(error.to_string()))?
        .ok_or_else(|| {
            RoostError::Database("bootstrap account count returned no row".to_owned())
        })?;
    let count: i64 = existing
        .try_get("", "count")
        .map_err(|error| RoostError::Database(error.to_string()))?;

    if count != 0 {
        return Err(RoostError::InvalidInput(
            "bootstrap is only allowed before local accounts exist".to_owned(),
        ));
    }

    let account_id = Uuid::now_v7();
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"
        INSERT INTO local_account (id, username, email, password_hash, is_admin)
        VALUES ($1, $2, $3, $4, true)
        "#,
        vec![
            account_id.into(),
            username.to_owned().into(),
            email.to_owned().into(),
            password_hash.to_owned().into(),
        ],
    ))
    .await
    .map_err(|error| RoostError::Database(error.to_string()))?;

    Ok(account_id)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimedJob {
    pub id: JobId,
    pub kind: String,
    pub payload: JsonValue,
    pub attempts: i32,
}

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

pub fn next_retry_at(attempts: i32) -> OffsetDateTime {
    let exponent = attempts.clamp(0, 8) as u32;
    let seconds = 2_i64.pow(exponent);
    OffsetDateTime::now_utc() + Duration::seconds(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_backoff_is_capped() {
        let now = OffsetDateTime::now_utc();
        let early = next_retry_at(1);
        let late = next_retry_at(100);

        assert!(early > now);
        assert!(late - now <= Duration::seconds(256 + 1));
    }
}
