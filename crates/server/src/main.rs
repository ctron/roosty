use std::net::SocketAddr;

use axum::{Router, routing::get};
use clap::{Parser, Subcommand};
use roost_core::{Result, RoostError};
use roost_migration::Migrator;
use sea_orm_migration::MigratorTrait;
use tracing::info;

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
        /// Run durable background jobs in the same process.
        #[arg(long)]
        with_worker: bool,

        #[arg(long, env = "LISTEN_ADDR", default_value = "0.0.0.0:3000")]
        listen: SocketAddr,
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
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    match cli.command {
        Command::Serve {
            with_worker,
            listen,
        } => serve(listen, with_worker).await,
        Command::Worker => Err(RoostError::NotImplemented("worker command")),
        Command::Migrate => migrate().await,
        Command::Admin { command } => match command {
            AdminCommand::Bootstrap {
                username: _,
                email: _,
            } => Err(RoostError::NotImplemented("admin bootstrap command")),
        },
    }
}

async fn migrate() -> Result<()> {
    let database_url = std::env::var("DATABASE_URL")
        .map_err(|_| RoostError::Configuration("DATABASE_URL is required".to_owned()))?;
    let db = roost_db::connect(&database_url).await?;

    Migrator::up(&db, None)
        .await
        .map_err(|error| RoostError::Database(error.to_string()))
}

async fn serve(listen: SocketAddr, with_worker: bool) -> Result<()> {
    if with_worker {
        info!("starting HTTP server with in-process durable worker");
    }

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz));

    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .map_err(|error| RoostError::Configuration(error.to_string()))?;

    info!(%listen, "listening");

    axum::serve(listener, app)
        .await
        .map_err(|error| RoostError::Configuration(error.to_string()))
}

async fn healthz() -> &'static str {
    "ok"
}

async fn readyz() -> &'static str {
    "ok"
}
