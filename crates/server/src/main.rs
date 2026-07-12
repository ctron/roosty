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

mod auth;
mod compat;
mod config;
mod http;
mod instance;
mod password;
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

    #[test]
    fn validates_usernames() {
        assert!(validate_username("admin_1").is_ok());
        assert!(validate_username("a").is_err());
        assert!(validate_username("bad-name").is_err());
    }

    #[test]
    fn validates_email_shape() {
        assert!(validate_email("admin@example.com").is_ok());
        assert!(validate_email("admin").is_err());
        assert!(validate_email(" admin@example.com").is_err());
    }
}
