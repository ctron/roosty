#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::{net::SocketAddr, time::Duration};

use axum::Router;
use clap::{Parser, Subcommand};
use roosty_core::{Result, RoostyError};
use roosty_migration::Migrator;
use sea_orm_migration::MigratorTrait;
use tokio::{sync::watch, task::JoinSet};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

mod accounts;
mod auth;
mod compat;
mod config;
mod conversations;
mod federation;
mod http;
mod instance;
mod markers;
mod media;
mod notifications;
mod password;
mod search;
mod statuses;
mod streaming;
#[cfg(test)]
mod test_postgres;

use crate::{
    config::{Config, database_url_from_env},
    http::AppState,
};

#[derive(Debug, Parser)]
#[command(name = "roosty")]
#[command(about = "Standalone Rust ActivityPub server")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the HTTP server.
    Serve {
        /// Run database migrations before starting the HTTP server.
        #[arg(long = "with-migrations")]
        migrations: bool,

        /// Run durable background jobs in the same process.
        #[arg(long)]
        with_worker: bool,

        #[arg(long)]
        listen: Option<SocketAddr>,
    },

    /// Run only durable background jobs.
    Worker,

    /// Run database migrations.
    Migrate,

    /// Administrative commands.
    Admin {
        #[command(subcommand)]
        command: AdminCommand,
    },
}

#[derive(Debug, Subcommand)]
enum AdminCommand {
    /// Create the initial local administrator account.
    Bootstrap {
        #[arg(long)]
        username: String,

        #[arg(long)]
        email: String,
    },

    /// Create an additional local account.
    CreateUser {
        #[arg(long)]
        username: String,

        #[arg(long)]
        email: String,

        /// Grant administrator privileges to the new account.
        #[arg(long)]
        admin: bool,
    },

    /// Reset a local account password and print a temporary replacement.
    ResetPassword {
        #[arg(long)]
        username: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let cli = Cli::parse();

    match cli.command {
        Command::Serve {
            migrations,
            with_worker,
            listen,
        } => serve(listen, migrations, with_worker).await,
        Command::Worker => worker().await,
        Command::Migrate => migrate().await,
        Command::Admin { command } => match command {
            AdminCommand::Bootstrap { username, email } => bootstrap_admin(&username, &email).await,
            AdminCommand::CreateUser {
                username,
                email,
                admin,
            } => create_user(&username, &email, admin).await,
            AdminCommand::ResetPassword { username } => reset_password(&username).await,
        },
    }
}

/// Initialize tracing with the default formatter and a RUST_LOG override.
fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("roosty=info,tower_http=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

async fn migrate() -> Result<()> {
    let database_url = database_url_from_env()?;
    let db = roosty_db::connect(&database_url).await?;

    run_migrations(&db).await
}

async fn run_migrations(db: &roosty_db::DbConnection) -> Result<()> {
    Ok(Migrator::up(db, None).await?)
}

async fn bootstrap_admin(username: &str, email: &str) -> Result<()> {
    validate_username(username)?;
    validate_email(email)?;

    let database_url = database_url_from_env()?;
    let db = roosty_db::connect(&database_url).await?;
    let temporary_password = password::generate_temporary_password();
    let password_hash = password::hash_password(&temporary_password)?;

    let account_id =
        roosty_db::create_bootstrap_admin(&db, username, email, &password_hash).await?;

    println!("Created bootstrap administrator account {account_id}");
    println!("Username: {username}");
    println!("Email: {email}");
    println!("Temporary password: {temporary_password}");
    println!("Change this password after the first login flow is implemented.");

    Ok(())
}

/// Create an additional local account from an operator command.
async fn create_user(username: &str, email: &str, admin: bool) -> Result<()> {
    validate_username(username)?;
    validate_email(email)?;

    let database_url = database_url_from_env()?;
    let db = roosty_db::connect(&database_url).await?;
    let temporary_password = password::generate_temporary_password();
    let password_hash = password::hash_password(&temporary_password)?;

    let account_id = if admin {
        roosty_db::create_admin_account(&db, username, email, &password_hash).await?
    } else {
        roosty_db::create_local_account(&db, username, email, &password_hash).await?
    };
    let role = if admin { "administrator" } else { "user" };

    println!("Created local {role} account {account_id}");
    println!("Username: {username}");
    println!("Email: {email}");
    println!("Temporary password: {temporary_password}");

    Ok(())
}

/// Reset a local account password from an operator command.
async fn reset_password(username: &str) -> Result<()> {
    validate_username(username)?;

    let database_url = database_url_from_env()?;
    let db = roosty_db::connect(&database_url).await?;
    let temporary_password = password::generate_temporary_password();
    let password_hash = password::hash_password(&temporary_password)?;

    let account = roosty_db::update_local_account_password_hash(&db, username, &password_hash)
        .await?
        .ok_or_else(|| RoostyError::InvalidInput("local account does not exist".to_owned()))?;

    println!("Reset password for local account {}", account.id.0);
    println!("Username: {}", account.username);
    println!("Temporary password: {temporary_password}");

    Ok(())
}

async fn serve(
    listen_override: Option<SocketAddr>,
    run_startup_migrations: bool,
    with_worker: bool,
) -> Result<()> {
    let config = Config::from_env(listen_override)?;
    let db = roosty_db::connect(&config.database_url).await?;
    if run_startup_migrations {
        info!("running database migrations before server startup");
        run_migrations(&db).await?;
    }

    let state = AppState::new(config.clone(), db.clone());
    state.streaming_events.initialize_listener().await?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let shutdown_task = tokio::spawn(wait_for_shutdown(shutdown_tx));
    let worker_task = if with_worker {
        info!("starting in-process durable worker");
        Some(tokio::spawn(worker_pool(
            db,
            config.clone(),
            shutdown_rx.clone(),
        )))
    } else {
        None
    };

    let main_routes_include_infra = config.infra_listen_addr.is_none();
    let app = http::app_router(state.clone(), main_routes_include_infra);
    let main_server = serve_router(config.listen_addr, app, shutdown_rx.clone());

    if let Some(infra_listen_addr) = config.infra_listen_addr {
        let infra_server = serve_router(
            infra_listen_addr,
            http::infra_router(state.clone()),
            shutdown_rx.clone(),
        );
        tokio::try_join!(main_server, infra_server)?;
    } else {
        main_server.await?;
    }

    state.streaming_events.shutdown();

    if let Some(worker_task) = worker_task {
        worker_task
            .await
            .map_err(|error| RoostyError::Configuration(error.to_string()))??;
    }
    shutdown_task.abort();

    Ok(())
}

async fn worker() -> Result<()> {
    let config = Config::from_env(None)?;
    let db = roosty_db::connect(&config.database_url).await?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let shutdown_task = tokio::spawn(wait_for_shutdown(shutdown_tx));
    let result = worker_pool(db, config, shutdown_rx).await;
    shutdown_task.abort();
    result
}

async fn serve_router(
    listen: SocketAddr,
    app: Router,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .map_err(|error| RoostyError::Configuration(error.to_string()))?;

    info!(%listen, "listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            while !*shutdown_rx.borrow_and_update() {
                if shutdown_rx.changed().await.is_err() {
                    break;
                }
            }
        })
        .await
        .map_err(|error| RoostyError::Configuration(error.to_string()))
}

async fn wait_for_shutdown(shutdown_tx: watch::Sender<bool>) {
    if let Err(error) = tokio::signal::ctrl_c().await {
        warn!(%error, "failed to listen for shutdown signal");
    }
    let _ = shutdown_tx.send(true);
}

/// Run the configured number of independent durable-job loops.
async fn worker_pool(
    db: roosty_db::DbConnection,
    config: Config,
    shutdown_rx: watch::Receiver<bool>,
) -> Result<()> {
    let process_identity = format!(
        "{}:{}:{}",
        std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown-host".to_owned()),
        std::process::id(),
        uuid::Uuid::now_v7()
    );
    let mut workers = JoinSet::new();
    info!(
        workers = config.worker_concurrency,
        "starting durable worker pool"
    );

    for slot in 0..config.worker_concurrency {
        let worker_id = format!("{process_identity}:{slot}");
        workers.spawn(worker_loop(
            db.clone(),
            config.clone(),
            worker_id,
            shutdown_rx.clone(),
        ));
    }

    while let Some(result) = workers.join_next().await {
        result.map_err(|error| RoostyError::Configuration(error.to_string()))??;
    }

    Ok(())
}

/// Repeatedly claim and execute one durable job for a single worker identity.
async fn worker_loop(
    db: roosty_db::DbConnection,
    config: Config,
    worker_id: String,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<()> {
    loop {
        if *shutdown_rx.borrow_and_update() {
            info!(%worker_id, "worker shutdown requested");
            return Ok(());
        }

        if worker_iteration(&db, &config, &worker_id).await? {
            continue;
        }

        tokio::select! {
            changed = shutdown_rx.changed() => {
                if changed.is_err() || *shutdown_rx.borrow() {
                    info!(%worker_id, "worker shutdown requested");
                    return Ok(());
                }
            }
            () = tokio::time::sleep(Duration::from_secs(5)) => {
            }
        }
    }
}

/// Claim and process one due job, returning whether work was found.
async fn worker_iteration(
    db: &roosty_db::DbConnection,
    config: &Config,
    worker_id: &str,
) -> Result<bool> {
    let claim_ttl = time::Duration::minutes(5);
    let Some(job) = roosty_db::claim_due_job(db, worker_id, claim_ttl).await? else {
        return Ok(false);
    };

    let state = AppState::new(config.clone(), db.clone());
    let result = match job.kind.as_str() {
        "federation_follow_response" => {
            federation::deliver_follow_response(&state, job.payload.clone()).await
        }
        "federation_status_delivery" => {
            federation::deliver_status_activity(&state, job.payload.clone()).await
        }
        "federation_follow_delivery" => {
            federation::deliver_follow_activity(&state, job.payload.clone()).await
        }
        "federation_favourite_delivery" => {
            federation::deliver_favourite_activity(&state, job.payload.clone()).await
        }
        "federation_reblog_delivery" => {
            federation::deliver_reblog_activity(&state, job.payload.clone()).await
        }
        "federation_actor_update_delivery" => {
            federation::deliver_actor_update(&state, job.payload.clone()).await
        }
        "federation_remote_media_fetch" => {
            media::fetch_remote_media(&state, job.payload.clone()).await
        }
        _ => Ok(()),
    };
    match result {
        Ok(()) => {
            if !roosty_db::mark_job_completed(db, &job).await? {
                warn!(job_id = %job.id.0, %worker_id, "discarded stale job completion");
            }
        }
        Err(error) => {
            let permanent = roosty_db::job_has_exceeded_max_age(
                job.created_at,
                config.federation_delivery_max_age,
            ) || error
                .to_string()
                .starts_with("permanent federation delivery failure:");
            if permanent {
                if roosty_db::mark_job_permanently_failed(db, &job, &error.to_string()).await? {
                    warn!(job_id = %job.id.0, %error, "federation delivery failed permanently");
                } else {
                    warn!(job_id = %job.id.0, %worker_id, "discarded stale permanent job failure");
                }
            } else if roosty_db::mark_job_failed(db, &job, &error.to_string())
                .await?
                .is_none()
            {
                warn!(job_id = %job.id.0, %worker_id, "discarded stale job retry");
            }
        }
    }

    Ok(true)
}

fn validate_username(username: &str) -> Result<()> {
    if username.len() < 2
        || username.len() > 30
        || !username
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        return Err(RoostyError::InvalidInput(
            "username must be 2-30 ASCII letters, numbers, or underscores".to_owned(),
        ));
    }

    Ok(())
}

fn validate_email(email: &str) -> Result<()> {
    if !email.contains('@') || email.trim() != email {
        return Err(RoostyError::InvalidInput(
            "email must contain @ and must not contain surrounding whitespace".to_owned(),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::{
        collections::HashSet,
        time::{SystemTime, UNIX_EPOCH},
    };

    use postgresql_embedded::PostgreSQL;
    use roosty_migration::Migrator;
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
    use sea_orm_migration::MigratorTrait;
    use tempfile::TempDir;

    /// Protects the local username rules used by admin account creation commands.
    #[test]
    fn validates_usernames() {
        assert!(validate_username("admin_1").is_ok());
        assert!(validate_username("a").is_err());
        assert!(validate_username("bad-name").is_err());
    }

    /// Protects the coarse email shape check used before account inserts.
    #[test]
    fn validates_email_shape() {
        assert!(validate_email("admin@example.com").is_ok());
        assert!(validate_email("admin").is_err());
        assert!(validate_email(" admin@example.com").is_err());
    }

    /// Keeps the operator-facing create-user CLI shape stable.
    #[test]
    fn parses_create_user_command() {
        let cli = Cli::parse_from([
            "roosty",
            "admin",
            "create-user",
            "--username",
            "alice",
            "--email",
            "alice@example.com",
            "--admin",
        ]);

        let parsed = match cli.command {
            Command::Admin {
                command:
                    AdminCommand::CreateUser {
                        username,
                        email,
                        admin,
                    },
            } => Some((username, email, admin)),
            _ => None,
        };

        assert_eq!(
            parsed,
            Some(("alice".to_owned(), "alice@example.com".to_owned(), true))
        );
    }

    /// Keeps the operator-facing password reset CLI shape stable.
    #[test]
    fn parses_reset_password_command() {
        let cli = Cli::parse_from(["roosty", "admin", "reset-password", "--username", "alice"]);

        let parsed = match cli.command {
            Command::Admin {
                command: AdminCommand::ResetPassword { username },
            } => Some(username),
            _ => None,
        };

        assert_eq!(parsed, Some("alice".to_owned()));
    }

    /// Verifies that operator-created users can be added after bootstrap with role metadata.
    #[tokio::test]
    async fn creates_additional_local_users_with_roles() {
        let (postgresql, db, database_name, _temp_dir) = migrated_test_database().await;
        let password_hash = password::hash_password("password").unwrap();

        roosty_db::create_bootstrap_admin(&db, "admin", "admin@example.com", &password_hash)
            .await
            .unwrap();
        let user_id =
            roosty_db::create_local_account(&db, "alice", "alice@example.com", &password_hash)
                .await
                .unwrap();
        let admin_id = roosty_db::create_admin_account(
            &db,
            "moderator",
            "moderator@example.com",
            &password_hash,
        )
        .await
        .unwrap();

        let user = roosty_db::find_local_account_by_id(&db, roosty_core::AccountId(user_id))
            .await
            .unwrap()
            .unwrap();
        let admin = roosty_db::find_local_account_by_id(&db, roosty_core::AccountId(admin_id))
            .await
            .unwrap()
            .unwrap();
        let duplicate_username =
            roosty_db::create_local_account(&db, "alice", "alice2@example.com", &password_hash)
                .await;
        let duplicate_email =
            roosty_db::create_local_account(&db, "alice2", "alice@example.com", &password_hash)
                .await;

        assert!(!user.is_admin);
        assert!(admin.is_admin);
        assert!(matches!(
            duplicate_username,
            Err(RoostyError::InvalidInput(message)) if message == "username is already in use"
        ));
        assert!(matches!(
            duplicate_email,
            Err(RoostyError::InvalidInput(message)) if message == "email is already in use"
        ));

        db.close().await.unwrap();
        postgresql.drop_database(&database_name).await.unwrap();
        postgresql.stop().await.unwrap();
    }

    /// Given an existing account, replacing its hash makes only the new password valid.
    #[tokio::test]
    async fn resets_local_account_password_hash() {
        let (postgresql, db, database_name, _temp_dir) = migrated_test_database().await;
        let old_hash = password::hash_password("old-password").unwrap();
        roosty_db::create_bootstrap_admin(&db, "admin", "admin@example.com", &old_hash)
            .await
            .unwrap();
        let new_hash = password::hash_password("new-password").unwrap();

        let account = roosty_db::update_local_account_password_hash(&db, "admin", &new_hash)
            .await
            .unwrap()
            .unwrap();
        let missing = roosty_db::update_local_account_password_hash(&db, "missing", &new_hash)
            .await
            .unwrap();

        assert_eq!(account.username, "admin");
        assert!(password::verify_password("new-password", &account.password_hash).unwrap());
        assert!(!password::verify_password("old-password", &account.password_hash).unwrap());
        assert!(missing.is_none());

        db.close().await.unwrap();
        postgresql.drop_database(&database_name).await.unwrap();
        postgresql.stop().await.unwrap();
    }

    /// Given a failed delivery beyond its retry horizon, when the worker polls, then it records a
    /// permanent diagnostic and never makes that job claimable again.
    #[tokio::test]
    async fn permanently_fails_expired_delivery_jobs() {
        let (postgresql, db, database_name, _temp_dir) = migrated_test_database().await;
        let job_id = roosty_db::enqueue_job(
            &db,
            roosty_db::JobKind::FederationFollowDelivery,
            serde_json::json!({}),
            None,
            time::OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();
        db.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE job SET created_at = now() - interval '8 days' WHERE id = $1",
            vec![job_id.0.into()],
        ))
        .await
        .unwrap();

        assert!(
            worker_iteration(&db, &test_worker_config(), "permanent-test-worker")
                .await
                .unwrap()
        );

        let job = db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT completed_at, locked_at, locked_by, last_error FROM job WHERE id = $1",
                vec![job_id.0.into()],
            ))
            .await
            .unwrap()
            .unwrap();
        let completed_at: Option<time::OffsetDateTime> = job.try_get("", "completed_at").unwrap();
        let locked_at: Option<time::OffsetDateTime> = job.try_get("", "locked_at").unwrap();
        let locked_by: Option<String> = job.try_get("", "locked_by").unwrap();
        let last_error: Option<String> = job.try_get("", "last_error").unwrap();

        assert!(completed_at.is_some());
        assert!(locked_at.is_none());
        assert!(locked_by.is_none());
        assert_eq!(
            last_error.as_deref(),
            Some("invalid input: invalid follow delivery payload")
        );
        assert!(
            roosty_db::claim_due_job(&db, "verification-worker", time::Duration::minutes(5),)
                .await
                .unwrap()
                .is_none()
        );

        db.close().await.unwrap();
        postgresql.drop_database(&database_name).await.unwrap();
        postgresql.stop().await.unwrap();
    }

    /// Given a job claimed by a worker that stopped, when its claim expires, then the next poll
    /// reclaims and completes it.
    #[tokio::test]
    async fn recovers_expired_job_claims() {
        let (postgresql, db, database_name, _temp_dir) = migrated_test_database().await;
        let job_id = uuid::Uuid::now_v7();
        db.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
            INSERT INTO job (id, kind, payload, run_after, locked_at, locked_by)
            VALUES ($1, 'unknown_worker_job', '{}'::jsonb, now() - interval '10 minutes',
                    now() - interval '10 minutes', 'stopped-worker')
            "#,
            vec![job_id.into()],
        ))
        .await
        .unwrap();

        assert!(
            worker_iteration(&db, &test_worker_config(), "recovery-test-worker")
                .await
                .unwrap()
        );

        let job = db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT completed_at, locked_at, locked_by, last_error FROM job WHERE id = $1",
                vec![job_id.into()],
            ))
            .await
            .unwrap()
            .unwrap();
        let completed_at: Option<time::OffsetDateTime> = job.try_get("", "completed_at").unwrap();
        let locked_at: Option<time::OffsetDateTime> = job.try_get("", "locked_at").unwrap();
        let locked_by: Option<String> = job.try_get("", "locked_by").unwrap();
        let last_error: Option<String> = job.try_get("", "last_error").unwrap();

        assert!(completed_at.is_some());
        assert!(locked_at.is_none());
        assert!(locked_by.is_none());
        assert!(last_error.is_none());

        db.close().await.unwrap();
        postgresql.drop_database(&database_name).await.unwrap();
        postgresql.stop().await.unwrap();
    }

    /// Given concurrent worker identities, when they claim due jobs, then PostgreSQL assigns each
    /// job to at most one worker through `FOR UPDATE SKIP LOCKED`.
    #[tokio::test]
    async fn concurrent_workers_claim_distinct_jobs() {
        let (postgresql, db, database_name, _temp_dir) = migrated_test_database().await;
        for _ in 0..3 {
            roosty_db::enqueue_job(
                &db,
                roosty_db::JobKind::FederationFollowDelivery,
                serde_json::json!({}),
                None,
                time::OffsetDateTime::now_utc(),
            )
            .await
            .unwrap();
        }

        let (first, second, third) = tokio::join!(
            roosty_db::claim_due_job(&db, "worker-a", time::Duration::minutes(5)),
            roosty_db::claim_due_job(&db, "worker-b", time::Duration::minutes(5)),
            roosty_db::claim_due_job(&db, "worker-c", time::Duration::minutes(5)),
        );
        let jobs = [
            first.unwrap().unwrap(),
            second.unwrap().unwrap(),
            third.unwrap().unwrap(),
        ];
        let ids: HashSet<_> = jobs.iter().map(|job| job.id).collect();
        let claims: HashSet<_> = jobs.iter().map(|job| job.claim_id).collect();

        assert_eq!(ids.len(), 3);
        assert_eq!(claims.len(), 3);

        db.close().await.unwrap();
        postgresql.drop_database(&database_name).await.unwrap();
        postgresql.stop().await.unwrap();
    }

    /// Given a reclaimed job, when its former owner reports any outcome, then the stale writes do
    /// not override the active claim.
    #[tokio::test]
    async fn stale_worker_outcomes_do_not_override_reclaimed_jobs() {
        let (postgresql, db, database_name, _temp_dir) = migrated_test_database().await;
        let job_id = roosty_db::enqueue_job(
            &db,
            roosty_db::JobKind::FederationFollowDelivery,
            serde_json::json!({}),
            None,
            time::OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();
        let original = roosty_db::claim_due_job(&db, "original-worker", time::Duration::minutes(5))
            .await
            .unwrap()
            .unwrap();
        db.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE job SET locked_at = now() - interval '10 minutes' WHERE id = $1",
            vec![job_id.0.into()],
        ))
        .await
        .unwrap();
        let replacement =
            roosty_db::claim_due_job(&db, "replacement-worker", time::Duration::minutes(5))
                .await
                .unwrap()
                .unwrap();

        assert_ne!(original.claim_id, replacement.claim_id);
        assert!(!roosty_db::mark_job_completed(&db, &original).await.unwrap());
        assert!(
            roosty_db::mark_job_failed(&db, &original, "stale failure")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            !roosty_db::mark_job_permanently_failed(&db, &original, "stale permanent failure")
                .await
                .unwrap()
        );

        let job = db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT claim_id, completed_at, last_error FROM job WHERE id = $1",
                vec![job_id.0.into()],
            ))
            .await
            .unwrap()
            .unwrap();
        let claim_id: Option<uuid::Uuid> = job.try_get("", "claim_id").unwrap();
        let completed_at: Option<time::OffsetDateTime> = job.try_get("", "completed_at").unwrap();
        let last_error: Option<String> = job.try_get("", "last_error").unwrap();

        assert_eq!(claim_id, Some(replacement.claim_id.0));
        assert!(completed_at.is_none());
        assert!(last_error.is_none());
        assert!(
            roosty_db::mark_job_completed(&db, &replacement)
                .await
                .unwrap()
        );

        db.close().await.unwrap();
        postgresql.drop_database(&database_name).await.unwrap();
        postgresql.stop().await.unwrap();
    }

    fn test_worker_config() -> Config {
        Config {
            database_url: "postgres://unused".to_owned(),
            public_base_url: "https://worker.test".parse().unwrap(),
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            infra_listen_addr: None,
            session_secret: "test-session-secret-change-me-000".to_owned(),
            token_pepper: "test-token-pepper-change-me-0000".to_owned(),
            object_storage_backend: "local".to_owned(),
            media_root: "./media".to_owned(),
            registration_mode: "closed".to_owned(),
            federation_enabled: true,
            federation_key_encryption_secret: Some(
                "test-federation-key-encryption-secret-000".to_owned(),
            ),
            federation_allowed_domains: vec!["*".to_owned()],
            federation_blocked_domains: Vec::new(),
            federation_delivery_max_age: time::Duration::days(7),
            remote_media_cache_ttl: time::Duration::days(30),
            remote_media_max_bytes: 40 * 1024 * 1024,
            remote_media_fetch_concurrency: 5,
            worker_concurrency: 4,
            streaming: crate::config::StreamingConfig::default(),
            instance_name: "Worker test".to_owned(),
            instance_description: None,
        }
    }

    /// Starts a migrated temporary PostgreSQL database for CLI-adjacent DB tests.
    async fn migrated_test_database() -> (PostgreSQL, roosty_db::DbConnection, String, TempDir) {
        let temp_dir = tempfile::Builder::new()
            .prefix("roosty-admin-")
            .tempdir()
            .unwrap();
        let database_name = unique_name();
        let data_dir = temp_dir.path().join("data").join(&database_name);
        let password_file = temp_dir
            .path()
            .join("passwords")
            .join(format!("{database_name}.pgpass"));

        if let Some(parent) = password_file.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }

        let settings = crate::test_postgres::settings(&data_dir, password_file);
        let mut postgresql = PostgreSQL::new(settings);

        postgresql.setup().await.unwrap();
        postgresql.start().await.unwrap();
        postgresql.create_database(&database_name).await.unwrap();

        let database_url = postgresql.settings().url(&database_name);
        let db = roosty_db::connect(&database_url).await.unwrap();
        Migrator::up(&db, None).await.unwrap();

        (postgresql, db, database_name, temp_dir)
    }

    /// Builds a database name unique enough for parallel embedded PostgreSQL tests.
    fn unique_name() -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("roosty_admin_{nanos}")
    }
}
