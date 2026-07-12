#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::{net::SocketAddr, time::Duration};

use axum::Router;
use clap::{Parser, Subcommand};
use roost_core::{Result, RoostError};
use roost_migration::Migrator;
use sea_orm_migration::MigratorTrait;
use tokio::sync::watch;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

mod accounts;
mod auth;
mod compat;
mod config;
mod http;
mod instance;
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
#[command(name = "roost")]
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
        },
    }
}

/// Initialize tracing with the default formatter and a RUST_LOG override.
fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("roost=info,tower_http=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

async fn migrate() -> Result<()> {
    let database_url = database_url_from_env()?;
    let db = roost_db::connect(&database_url).await?;

    run_migrations(&db).await
}

async fn run_migrations(db: &roost_db::DbConnection) -> Result<()> {
    Ok(Migrator::up(db, None).await?)
}

async fn bootstrap_admin(username: &str, email: &str) -> Result<()> {
    validate_username(username)?;
    validate_email(email)?;

    let database_url = database_url_from_env()?;
    let db = roost_db::connect(&database_url).await?;
    let temporary_password = password::generate_temporary_password();
    let password_hash = password::hash_password(&temporary_password)?;

    let account_id = roost_db::create_bootstrap_admin(&db, username, email, &password_hash).await?;

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
    let db = roost_db::connect(&database_url).await?;
    let temporary_password = password::generate_temporary_password();
    let password_hash = password::hash_password(&temporary_password)?;

    let account_id = if admin {
        roost_db::create_admin_account(&db, username, email, &password_hash).await?
    } else {
        roost_db::create_local_account(&db, username, email, &password_hash).await?
    };
    let role = if admin { "administrator" } else { "user" };

    println!("Created local {role} account {account_id}");
    println!("Username: {username}");
    println!("Email: {email}");
    println!("Temporary password: {temporary_password}");

    Ok(())
}

async fn serve(
    listen_override: Option<SocketAddr>,
    run_startup_migrations: bool,
    with_worker: bool,
) -> Result<()> {
    let config = Config::from_env(listen_override)?;
    let db = roost_db::connect(&config.database_url).await?;
    if run_startup_migrations {
        info!("running database migrations before server startup");
        run_migrations(&db).await?;
    }

    let state = AppState::new(config.clone(), db.clone());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let shutdown_task = tokio::spawn(wait_for_shutdown(shutdown_tx));
    let worker_task = if with_worker {
        info!("starting in-process durable worker");
        Some(tokio::spawn(worker_loop(db, shutdown_rx.clone())))
    } else {
        None
    };

    let main_routes_include_infra = config.infra_listen_addr.is_none();
    let app = http::app_router(state.clone(), main_routes_include_infra);
    let main_server = serve_router(config.listen_addr, app, shutdown_rx.clone());

    if let Some(infra_listen_addr) = config.infra_listen_addr {
        let infra_server = serve_router(
            infra_listen_addr,
            http::infra_router(state),
            shutdown_rx.clone(),
        );
        tokio::try_join!(main_server, infra_server)?;
    } else {
        main_server.await?;
    }

    if let Some(worker_task) = worker_task {
        worker_task
            .await
            .map_err(|error| RoostError::Configuration(error.to_string()))??;
    }
    shutdown_task.abort();

    Ok(())
}

async fn worker() -> Result<()> {
    let database_url = database_url_from_env()?;
    let db = roost_db::connect(&database_url).await?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let shutdown_task = tokio::spawn(wait_for_shutdown(shutdown_tx));
    let result = worker_loop(db, shutdown_rx).await;
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
        .map_err(|error| RoostError::Configuration(error.to_string()))?;

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
        .map_err(|error| RoostError::Configuration(error.to_string()))
}

async fn wait_for_shutdown(shutdown_tx: watch::Sender<bool>) {
    if let Err(error) = tokio::signal::ctrl_c().await {
        warn!(%error, "failed to listen for shutdown signal");
    }
    let _ = shutdown_tx.send(true);
}

async fn worker_loop(
    db: roost_db::DbConnection,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<()> {
    let claim_ttl = time::Duration::minutes(5);

    loop {
        tokio::select! {
            changed = shutdown_rx.changed() => {
                if changed.is_err() || *shutdown_rx.borrow() {
                    info!("worker shutdown requested");
                    return Ok(());
                }
            }
            () = tokio::time::sleep(Duration::from_secs(5)) => {
                let released = roost_db::release_expired_claims(&db, claim_ttl).await?;
                if released > 0 {
                    info!(released, "released expired job claims");
                }
            }
        }
    }
}

fn validate_username(username: &str) -> Result<()> {
    if username.len() < 2
        || username.len() > 30
        || !username
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        return Err(RoostError::InvalidInput(
            "username must be 2-30 ASCII letters, numbers, or underscores".to_owned(),
        ));
    }

    Ok(())
}

fn validate_email(email: &str) -> Result<()> {
    if !email.contains('@') || email.trim() != email {
        return Err(RoostError::InvalidInput(
            "email must contain @ and must not contain surrounding whitespace".to_owned(),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::{SystemTime, UNIX_EPOCH};

    use postgresql_embedded::PostgreSQL;
    use roost_migration::Migrator;
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
            "roost",
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

    /// Verifies that operator-created users can be added after bootstrap with role metadata.
    #[tokio::test]
    async fn creates_additional_local_users_with_roles() {
        let (postgresql, db, database_name, _temp_dir) = migrated_test_database().await;
        let password_hash = password::hash_password("password").unwrap();

        roost_db::create_bootstrap_admin(&db, "admin", "admin@example.com", &password_hash)
            .await
            .unwrap();
        let user_id =
            roost_db::create_local_account(&db, "alice", "alice@example.com", &password_hash)
                .await
                .unwrap();
        let admin_id = roost_db::create_admin_account(
            &db,
            "moderator",
            "moderator@example.com",
            &password_hash,
        )
        .await
        .unwrap();

        let user = roost_db::find_local_account_by_id(&db, roost_core::AccountId(user_id))
            .await
            .unwrap()
            .unwrap();
        let admin = roost_db::find_local_account_by_id(&db, roost_core::AccountId(admin_id))
            .await
            .unwrap()
            .unwrap();
        let duplicate_username =
            roost_db::create_local_account(&db, "alice", "alice2@example.com", &password_hash)
                .await;
        let duplicate_email =
            roost_db::create_local_account(&db, "alice2", "alice@example.com", &password_hash)
                .await;

        assert!(!user.is_admin);
        assert!(admin.is_admin);
        assert!(matches!(
            duplicate_username,
            Err(RoostError::InvalidInput(message)) if message == "username is already in use"
        ));
        assert!(matches!(
            duplicate_email,
            Err(RoostError::InvalidInput(message)) if message == "email is already in use"
        ));

        db.close().await.unwrap();
        postgresql.drop_database(&database_name).await.unwrap();
        postgresql.stop().await.unwrap();
    }

    /// Starts a migrated temporary PostgreSQL database for CLI-adjacent DB tests.
    async fn migrated_test_database() -> (PostgreSQL, roost_db::DbConnection, String, TempDir) {
        let temp_dir = tempfile::Builder::new()
            .prefix("roost-admin-")
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
        let db = roost_db::connect(&database_url).await.unwrap();
        Migrator::up(&db, None).await.unwrap();

        (postgresql, db, database_name, temp_dir)
    }

    /// Builds a database name unique enough for parallel embedded PostgreSQL tests.
    fn unique_name() -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("roost_admin_{nanos}")
    }
}
