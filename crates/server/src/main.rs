#![deny(
    clippy::absolute_paths,
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used
)]

use std::{env, net::SocketAddr, path::Path, process, time::Duration};

use axum::Router;
use clap::{Parser, Subcommand};
use roosty_core::{AccountId, Result, RoostyError};
#[cfg(test)]
use roosty_db::{JobKind, NotificationPolicyUpdate};
use roosty_migration::Migrator;
use sea_orm::TransactionTrait;
use sea_orm_migration::MigratorTrait;
use tokio::{
    fs::remove_file,
    net::TcpListener,
    signal::ctrl_c,
    sync::watch,
    task::{JoinSet, yield_now},
    time::sleep,
};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

mod accounts;
mod admin;
mod auth;
mod compat;
mod config;
mod conversations;
mod explore;
mod featured_tags;
mod federation;
mod http;
mod instance;
mod lists;
mod markers;
mod media;
mod notifications;
mod password;
mod polls;
mod preview_cards;
mod push;
mod reports;
mod search;
mod search_discovery;
mod statuses;
mod streaming;
#[cfg(test)]
mod test_postgres;
mod version;
mod web;

const JOB_CLEANUP_BATCH_SIZE: u64 = 1_000;
const JOB_CLEANUP_INTERVAL: Duration = Duration::from_secs(60 * 60);

use crate::{
    config::{Config, DefaultEnabled, database_url_from_env},
    http::{AppState, DatabaseContext},
};
#[cfg(test)]
use crate::{
    config::{ObjectStorageBackend, RegistrationMode, ScheduledStatusConfig, StreamingConfig},
    test_postgres::settings,
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

        /// Enable or disable public search-engine indexing and sitemap discovery.
        #[arg(long, value_name = "true|false", num_args = 1)]
        search_indexing_enabled: Option<DefaultEnabled>,
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

    /// Limit a local or cached remote account in discovery and notification policy.
    LimitAccount {
        /// Local username or remote handle such as user@example.org.
        account: String,
    },

    /// Remove an account limit.
    UnlimitAccount {
        /// Local username or remote handle such as user@example.org.
        account: String,
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
            search_indexing_enabled,
        } => serve(listen, migrations, with_worker, search_indexing_enabled).await,
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
            AdminCommand::LimitAccount { account } => set_account_limited(&account, true).await,
            AdminCommand::UnlimitAccount { account } => set_account_limited(&account, false).await,
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
    let database_url = database_url_from_env()?;
    let db = roosty_db::connect(&database_url).await?;
    let result =
        admin::create_local_account(&db, None, admin::AdminSource::Cli, username, email, admin)
            .await?;
    let role = if admin { "administrator" } else { "user" };

    println!("Created local {role} account {}", result.account.id.0);
    println!("Username: {username}");
    println!("Email: {email}");
    println!("Temporary password: {}", result.temporary_password);

    Ok(())
}

/// Reset a local account password from an operator command.
async fn reset_password(username: &str) -> Result<()> {
    validate_username(username)?;

    let database_url = database_url_from_env()?;
    let db = roosty_db::connect(&database_url).await?;
    let account = roosty_db::find_local_account_by_username(&db, username)
        .await?
        .ok_or_else(|| RoostyError::InvalidInput("local account does not exist".to_owned()))?;
    let result =
        admin::reset_local_password(&db, None, admin::AdminSource::Cli, account.id).await?;

    println!("Reset password for local account {}", result.account.id.0);
    println!("Username: {}", result.account.username);
    println!("Temporary password: {}", result.temporary_password);

    Ok(())
}

/// Apply an operator-managed account limit without changing ActivityPub reachability.
async fn set_account_limited(account: &str, limited: bool) -> Result<()> {
    let account = account.trim().trim_start_matches('@');
    let database_url = database_url_from_env()?;
    let db = roosty_db::connect(&database_url).await?;
    let found = if let Some((username, domain)) = account.split_once('@') {
        roosty_db::find_remote_actor_by_handle(&db, username, domain)
            .await?
            .map(|actor| (actor.id, actor.activitypub_id))
    } else {
        roosty_db::find_local_account_by_username(&db, account)
            .await?
            .map(|account| (account.id, account.username))
    };
    let (account_id, target) = found.ok_or_else(|| {
        RoostyError::InvalidInput("local or cached remote account does not exist".to_owned())
    })?;
    admin::set_account_limited(&db, None, admin::AdminSource::Cli, account_id, limited).await?;
    let action = if limited { "Limited" } else { "Unlimited" };
    println!("{action} account {target}");
    Ok(())
}

async fn serve(
    listen_override: Option<SocketAddr>,
    run_startup_migrations: bool,
    with_worker: bool,
    search_indexing_override: Option<DefaultEnabled>,
) -> Result<()> {
    let config = Config::from_env(listen_override, search_indexing_override)?;
    let db = roosty_db::connect(&config.database_url).await?;
    if run_startup_migrations {
        info!("running database migrations before server startup");
        run_migrations(&db).await?;
    }
    roosty_db::configure_trend_refresh_schedule(&db, config.trends_refresh_interval).await?;
    roosty_db::enqueue_preview_backfill_if_needed(&db).await?;

    let state = AppState::new(config.clone(), db.clone());
    let database = DatabaseContext::new(db.clone());
    let mut leptos_options = leptos::config::get_configuration(None)
        .map_err(|error| RoostyError::Configuration(error.to_string()))?
        .leptos_options;
    if !cfg!(debug_assertions) {
        leptos_options.env = leptos::config::Env::PROD;
    }
    if let Ok(site_root) = env::var("ROOSTY_WEB_ROOT") {
        leptos_options.site_root = site_root.into();
    }
    let state = state.with_leptos_options(leptos_options);
    state.streaming_events.initialize_listener().await?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let shutdown_task = tokio::spawn(wait_for_shutdown(shutdown_tx));
    let worker_task = if with_worker {
        info!("starting in-process durable worker");
        Some(tokio::spawn(worker_pool(
            db.clone(),
            state.clone(),
            shutdown_rx.clone(),
        )))
    } else {
        None
    };

    let main_routes_include_infra = config.infra_listen_addr.is_none();
    let app = http::app_router(state.clone(), database.clone(), main_routes_include_infra);
    let main_server = serve_router(config.listen_addr, app, shutdown_rx.clone());

    if let Some(infra_listen_addr) = config.infra_listen_addr {
        let infra_server = serve_router(
            infra_listen_addr,
            http::infra_router(state.clone(), database),
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
    let config = Config::from_env(None, None)?;
    let db = roosty_db::connect(&config.database_url).await?;
    roosty_db::configure_trend_refresh_schedule(&db, config.trends_refresh_interval).await?;
    roosty_db::enqueue_preview_backfill_if_needed(&db).await?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let shutdown_task = tokio::spawn(wait_for_shutdown(shutdown_tx));
    let state = AppState::new(config, db.clone());
    let result = worker_pool(db, state, shutdown_rx).await;
    shutdown_task.abort();
    result
}

async fn serve_router(
    listen: SocketAddr,
    app: Router,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<()> {
    let listener = TcpListener::bind(listen)
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
    if let Err(error) = ctrl_c().await {
        warn!(%error, "failed to listen for shutdown signal");
    }
    let _ = shutdown_tx.send(true);
}

/// Run the configured number of independent durable-job loops.
async fn worker_pool(
    db: roosty_db::DbConnection,
    state: AppState,
    shutdown_rx: watch::Receiver<bool>,
) -> Result<()> {
    let process_identity = format!(
        "{}:{}:{}",
        env::var("HOSTNAME").unwrap_or_else(|_| "unknown-host".to_owned()),
        process::id(),
        uuid::Uuid::now_v7()
    );
    let mut workers = JoinSet::new();
    info!(
        workers = state.config.worker_concurrency,
        "starting durable worker pool"
    );

    workers.spawn(trend_scheduler_loop(db.clone(), shutdown_rx.clone()));
    workers.spawn(account_suggestion_scheduler_loop(
        db.clone(),
        state.config.account_suggestions_refresh_interval,
        shutdown_rx.clone(),
    ));
    workers.spawn(job_cleanup_loop(
        db.clone(),
        state.config.successful_job_retention,
        state.config.permanently_failed_job_retention,
        shutdown_rx.clone(),
    ));
    for slot in 0..state.config.worker_concurrency {
        let worker_id = format!("{process_identity}:{slot}");
        workers.spawn(worker_loop(
            db.clone(),
            state.clone(),
            worker_id,
            shutdown_rx.clone(),
        ));
    }

    while let Some(result) = workers.join_next().await {
        result.map_err(|error| RoostyError::Configuration(error.to_string()))??;
    }

    Ok(())
}

/// Periodically drain expired job diagnostics in bounded, multi-process-safe batches.
async fn job_cleanup_loop(
    db: roosty_db::DbConnection,
    successful_retention: time::Duration,
    permanently_failed_retention: time::Duration,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<()> {
    loop {
        if *shutdown_rx.borrow_and_update() {
            return Ok(());
        }

        let mut successful = 0_u64;
        let mut permanently_failed = 0_u64;
        loop {
            if *shutdown_rx.borrow() {
                return Ok(());
            }
            match roosty_db::cleanup_expired_jobs(
                &db,
                successful_retention,
                permanently_failed_retention,
                JOB_CLEANUP_BATCH_SIZE,
            )
            .await
            {
                Ok(outcome) => {
                    successful += outcome.successful;
                    permanently_failed += outcome.permanently_failed;
                    if outcome.total() < JOB_CLEANUP_BATCH_SIZE {
                        break;
                    }
                }
                Err(error) => {
                    warn!(%error, "durable job cleanup failed; will retry later");
                    break;
                }
            }
            yield_now().await;
        }
        if successful > 0 || permanently_failed > 0 {
            info!(
                successful,
                permanently_failed, "cleaned up expired durable jobs"
            );
        }

        tokio::select! {
            changed = shutdown_rx.changed() => {
                if changed.is_err() || *shutdown_rx.borrow() {
                    return Ok(());
                }
            }
            () = sleep(JOB_CLEANUP_INTERVAL) => {
            }
        }
    }
}

/// Poll the shared trend schedule without electing a process leader.
async fn trend_scheduler_loop(
    db: roosty_db::DbConnection,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<()> {
    loop {
        if *shutdown_rx.borrow_and_update() {
            return Ok(());
        }
        if let Some(job_id) = roosty_db::enqueue_due_trend_refresh(&db).await? {
            info!(job_id = %job_id.0, "enqueued scheduled trend refresh");
        }
        tokio::select! {
            changed = shutdown_rx.changed() => {
                if changed.is_err() || *shutdown_rx.borrow() {
                    return Ok(());
                }
            }
            () = sleep(Duration::from_secs(5)) => {
            }
        }
    }
}

/// Enqueue suggestion refreshes on this process's configured cadence.
async fn account_suggestion_scheduler_loop(
    db: roosty_db::DbConnection,
    refresh_interval: time::Duration,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<()> {
    let refresh_interval = Duration::try_from(refresh_interval).map_err(|_| {
        RoostyError::Configuration(
            "ROOSTY_ACCOUNT_SUGGESTIONS_REFRESH_INTERVAL must be positive".to_owned(),
        )
    })?;
    loop {
        if *shutdown_rx.borrow_and_update() {
            return Ok(());
        }
        tokio::select! {
            changed = shutdown_rx.changed() => {
                if changed.is_err() || *shutdown_rx.borrow() {
                    return Ok(());
                }
            }
            () = sleep(refresh_interval) => {
            }
        }
        let job_id = roosty_db::enqueue_account_suggestion_refresh(&db).await?;
        info!(job_id = %job_id.0, "scheduled account suggestion refresh");
    }
}

/// Repeatedly claim and execute one durable job for a single worker identity.
async fn worker_loop(
    db: roosty_db::DbConnection,
    state: AppState,
    worker_id: String,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<()> {
    loop {
        if *shutdown_rx.borrow_and_update() {
            info!(%worker_id, "worker shutdown requested");
            return Ok(());
        }

        if worker_iteration(&db, &state, &worker_id).await? {
            continue;
        }

        tokio::select! {
            changed = shutdown_rx.changed() => {
                if changed.is_err() || *shutdown_rx.borrow() {
                    info!(%worker_id, "worker shutdown requested");
                    return Ok(());
                }
            }
            () = sleep(Duration::from_secs(5)) => {
            }
        }
    }
}

/// Claim and process one due job, returning whether work was found.
async fn worker_iteration(
    db: &roosty_db::DbConnection,
    state: &AppState,
    worker_id: &str,
) -> Result<bool> {
    let claim_ttl = time::Duration::minutes(5);
    let Some(job) = roosty_db::claim_due_job(db, worker_id, claim_ttl).await? else {
        return Ok(false);
    };

    let database = DatabaseContext::new(db.clone());
    if job.kind == roosty_db::JobKind::FederationRemoteMediaFetch {
        media::execute_claimed_remote_media_job(state, &database, &job).await?;
        return Ok(true);
    }
    let result = match job.kind {
        roosty_db::JobKind::FederationFollowResponse => {
            federation::deliver_follow_response(state, &database, job.payload.clone()).await
        }
        roosty_db::JobKind::FederationStatusDelivery => {
            federation::deliver_status_activity(state, &database, job.payload.clone()).await
        }
        roosty_db::JobKind::FederationQuoteDelivery => {
            federation::deliver_quote_activity(state, &database, job.payload.clone()).await
        }
        roosty_db::JobKind::FederationFollowDelivery => {
            federation::deliver_follow_activity(state, &database, job.payload.clone()).await
        }
        roosty_db::JobKind::FederationFavouriteDelivery => {
            federation::deliver_favourite_activity(state, &database, job.payload.clone()).await
        }
        roosty_db::JobKind::FederationReblogDelivery => {
            federation::deliver_reblog_activity(state, &database, job.payload.clone()).await
        }
        roosty_db::JobKind::FederationActorUpdateDelivery => {
            federation::deliver_actor_update(state, &database, job.payload.clone()).await
        }
        roosty_db::JobKind::FederationModerationDelivery => {
            federation::deliver_moderation_activity(state, &database, job.payload.clone()).await
        }
        roosty_db::JobKind::FederationRemoteMediaFetch => Ok(()),
        roosty_db::JobKind::FederationFeaturedRefresh => {
            federation::refresh_remote_featured(state, &database, job.payload.clone()).await
        }
        roosty_db::JobKind::FederationFeaturedTagsRefresh => {
            federation::refresh_remote_featured_tags(state, &database, job.payload.clone()).await
        }
        roosty_db::JobKind::FederationThreadResolve => {
            federation::resolve_remote_status_thread(state, &database, job.payload.clone()).await
        }
        roosty_db::JobKind::FederationRepliesFetch => {
            federation::fetch_remote_status_replies(state, &database, job.payload.clone()).await
        }
        roosty_db::JobKind::FederationReplyFetch => {
            federation::fetch_remote_status_reply(state, &database, job.payload.clone()).await
        }
        roosty_db::JobKind::WebPushDelivery => state
            .push
            .deliver(job.payload.clone())
            .await
            .map_err(|error| roosty_core::RoostyError::Configuration(error.to_string())),
        roosty_db::JobKind::NotificationRequestMerge => {
            let account_id = job
                .payload
                .get("account_id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    RoostyError::InvalidInput(
                        "notification merge job account_id is missing".to_owned(),
                    )
                })?
                .parse()
                .map(AccountId)
                .map_err(|_| {
                    RoostyError::InvalidInput(
                        "notification merge job account_id is invalid".to_owned(),
                    )
                });
            match account_id {
                Ok(account_id) => roosty_db::merge_notification_requests(db, account_id)
                    .await
                    .map(|()| {
                        state
                            .streaming_events
                            .publish_notifications_merged(account_id);
                    }),
                Err(error) => Err(error),
            }
        }
        roosty_db::JobKind::NotificationRequestCleanup => {
            let account_id = job
                .payload
                .get("account_id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    RoostyError::InvalidInput(
                        "notification cleanup job account_id is missing".to_owned(),
                    )
                })?
                .parse()
                .map(AccountId)
                .map_err(|_| {
                    RoostyError::InvalidInput(
                        "notification cleanup job account_id is invalid".to_owned(),
                    )
                });
            match account_id {
                Ok(account_id) => roosty_db::cleanup_notification_requests(db, account_id).await,
                Err(error) => Err(error),
            }
        }
        roosty_db::JobKind::AccountPurge => {
            let account_id = job
                .payload
                .get("account_id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    RoostyError::InvalidInput("account purge job account_id is missing".to_owned())
                })?
                .parse()
                .map(AccountId)
                .map_err(|_| {
                    RoostyError::InvalidInput("account purge job account_id is invalid".to_owned())
                })?;
            let txn = db.begin().await?;
            let paths = roosty_db::purge_suspended_local_account(&txn, account_id).await?;
            txn.commit().await?;
            for path in paths {
                let _ = remove_file(Path::new(&state.config.media_root).join(path)).await;
            }
            Ok(())
        }
        roosty_db::JobKind::DomainModerationReconcile => {
            let block_id = job
                .payload
                .get("domain_block_id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    RoostyError::InvalidInput(
                        "domain reconciliation job domain_block_id is missing".to_owned(),
                    )
                })?
                .parse()
                .map_err(|_| {
                    RoostyError::InvalidInput(
                        "domain reconciliation job domain_block_id is invalid".to_owned(),
                    )
                })?;
            let txn = db.begin().await?;
            roosty_db::reconcile_federation_domain_block(&txn, block_id).await?;
            txn.commit().await?;
            Ok(())
        }
        roosty_db::JobKind::ScheduledStatusPublish => {
            let database = DatabaseContext::new(db.clone());
            statuses::publish_scheduled_status(state, &database, job.payload.clone()).await
        }
        roosty_db::JobKind::PollExpiration => {
            let database = DatabaseContext::new(db.clone());
            polls::expire_poll_job(state, &database, job.payload.clone()).await
        }
        roosty_db::JobKind::PollUpdate => {
            polls::publish_poll_update_job(state, db, job.payload.clone()).await
        }
        roosty_db::JobKind::FederationPollVoteDelivery => {
            federation::deliver_poll_vote(state, &database, job.payload.clone()).await
        }
        roosty_db::JobKind::TrendMaintenance => {
            let outcome = roosty_db::maintain_trends(db).await?;
            if outcome.has_more {
                roosty_db::enqueue_job(
                    db,
                    roosty_db::JobKind::TrendMaintenance,
                    serde_json::json!({}),
                    Some(&format!("trend-refresh-continuation:{}", job.id.0)),
                    time::OffsetDateTime::now_utc(),
                )
                .await
                .map(|_| ())
            } else {
                let expired_before =
                    time::OffsetDateTime::now_utc() - state.config.remote_media_cache_ttl;
                for path in roosty_db::prune_preview_cards(db, expired_before).await? {
                    media::remove_preview_card_image(state, &path).await;
                }
                Ok(())
            }
        }
        roosty_db::JobKind::AccountSuggestionMaintenance => {
            roosty_db::refresh_account_suggestion_scores(db).await
        }
        roosty_db::JobKind::PreviewCardFetch => {
            let database = DatabaseContext::new(db.clone());
            preview_cards::fetch_preview_card(state, &database, job.payload.clone(), job.attempts)
                .await
        }
        roosty_db::JobKind::PreviewCardBackfill => {
            let outcome = roosty_db::backfill_preview_cards(db).await?;
            if outcome.has_more {
                roosty_db::enqueue_job(
                    db,
                    roosty_db::JobKind::PreviewCardBackfill,
                    serde_json::json!({}),
                    Some(&format!("preview-card-backfill:{}", job.id.0)),
                    time::OffsetDateTime::now_utc(),
                )
                .await
                .map(|_| ())
            } else {
                Ok(())
            }
        }
    };
    match result {
        Ok(()) => {
            if !roosty_db::mark_job_completed(db, &job).await? {
                warn!(job_id = %job.id.0, %worker_id, "discarded stale job completion");
            }
        }
        Err(error) => {
            let is_federation_job = job.kind.as_str().starts_with("federation_");
            let permanent = is_federation_job
                && (roosty_db::job_has_exceeded_max_age(
                    job.created_at,
                    state.config.federation_delivery_max_age,
                ) || error
                    .to_string()
                    .starts_with("permanent federation delivery failure:")
                    || error
                        .to_string()
                        .starts_with("permanent federation fetch failure:"));
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
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use postgresql_embedded::PostgreSQL;
    use roosty_migration::Migrator;
    use sea_orm::{ConnectionTrait, DatabaseBackend, Iterable, Statement, TransactionTrait};
    use sea_orm_migration::MigratorTrait;
    use tempfile::TempDir;
    use tokio::time::{sleep, timeout};

    #[test]
    fn serve_search_indexing_flag_accepts_explicit_booleans() {
        for (value, expected) in [("true", true), ("false", false)] {
            let cli = Cli::try_parse_from(["roosty", "serve", "--search-indexing-enabled", value])
                .unwrap();
            let Command::Serve {
                search_indexing_enabled,
                ..
            } = cli.command
            else {
                unreachable!();
            };
            assert_eq!(
                search_indexing_enabled.map(DefaultEnabled::is_enabled),
                Some(expected)
            );
        }
        assert!(
            Cli::try_parse_from(["roosty", "serve", "--search-indexing-enabled", "sometimes"])
                .is_err()
        );
    }

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
        let (postgresql, db, _temp_dir) = migrated_test_database().await;
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
        let policy = roosty_db::update_notification_policy(
            &db,
            AccountId(user_id),
            NotificationPolicyUpdate {
                for_not_following: None,
                for_not_followers: None,
                for_new_accounts: None,
                for_private_mentions: None,
                for_limited_accounts: None,
            },
        )
        .await;
        let duplicate_username =
            roosty_db::create_local_account(&db, "alice", "alice2@example.com", &password_hash)
                .await;
        let duplicate_email =
            roosty_db::create_local_account(&db, "alice2", "alice@example.com", &password_hash)
                .await;

        assert!(!user.is_admin);
        assert!(admin.is_admin);
        assert!(policy.is_ok());
        assert!(matches!(
            duplicate_username,
            Err(RoostyError::InvalidInput(message)) if message == "username is already in use"
        ));
        assert!(matches!(
            duplicate_email,
            Err(RoostyError::InvalidInput(message)) if message == "email is already in use"
        ));

        db.close().await.unwrap();
        postgresql.stop().await.unwrap();
    }

    /// Given an existing account, replacing its hash makes only the new password valid.
    #[tokio::test]
    async fn resets_local_account_password_hash() {
        let (postgresql, db, _temp_dir) = migrated_test_database().await;
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
        postgresql.stop().await.unwrap();
    }

    /// Given a failed delivery beyond its retry horizon, when the worker polls, then it records a
    /// permanent diagnostic and never makes that job claimable again.
    #[tokio::test]
    async fn permanently_fails_expired_delivery_jobs() {
        let (postgresql, db, _temp_dir) = migrated_test_database().await;
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

        let state = AppState::new(test_worker_config(), db.clone());
        assert!(
            worker_iteration(&db, &state, "permanent-test-worker")
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
        postgresql.stop().await.unwrap();
    }

    /// Given jobs in every terminal and active state, when retention cleanup runs, then only
    /// terminal jobs older than their respective retention periods are removed.
    #[tokio::test]
    async fn cleans_up_only_expired_terminal_jobs() {
        let (postgresql, db, _temp_dir) = migrated_test_database().await;
        let old_success = uuid::Uuid::now_v7();
        let recent_success = uuid::Uuid::now_v7();
        let old_failure = uuid::Uuid::now_v7();
        let recent_failure = uuid::Uuid::now_v7();
        let pending = uuid::Uuid::now_v7();
        db.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
            INSERT INTO job (
                id, kind, payload, run_after, completed_at, permanently_failed_at, locked_at
            ) VALUES
                ($1, 'web_push_delivery', '{}'::jsonb, now(), now() - interval '25 hours', NULL, NULL),
                ($2, 'web_push_delivery', '{}'::jsonb, now(), now() - interval '23 hours', NULL, NULL),
                ($3, 'web_push_delivery', '{}'::jsonb, now(), now() - interval '31 days', now() - interval '31 days', NULL),
                ($4, 'web_push_delivery', '{}'::jsonb, now(), now() - interval '29 days', now() - interval '29 days', NULL),
                ($5, 'web_push_delivery', '{}'::jsonb, now() - interval '40 days', NULL, NULL, now())
            "#,
            vec![
                old_success.into(),
                recent_success.into(),
                old_failure.into(),
                recent_failure.into(),
                pending.into(),
            ],
        ))
        .await
        .unwrap();

        let outcome = roosty_db::cleanup_expired_jobs(
            &db,
            time::Duration::hours(24),
            time::Duration::days(30),
            100,
        )
        .await
        .unwrap();

        assert_eq!(outcome.successful, 1);
        assert_eq!(outcome.permanently_failed, 1);
        let remaining: i64 = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT count(*) AS count FROM job".to_owned(),
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get("", "count")
            .unwrap();
        assert_eq!(remaining, 3);

        db.close().await.unwrap();
        postgresql.stop().await.unwrap();
    }

    /// Given more expired jobs than one batch, concurrent cleaners respect the bound and safely
    /// partition or observe the shared work without double-counting rows.
    #[tokio::test]
    async fn bounds_and_coordinates_concurrent_job_cleanup() {
        let (postgresql, db, _temp_dir) = migrated_test_database().await;
        for _ in 0..5 {
            db.execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO job (id, kind, payload, run_after, completed_at) VALUES ($1, 'web_push_delivery', '{}'::jsonb, now(), now() - interval '2 days')",
                vec![uuid::Uuid::now_v7().into()],
            ))
            .await
            .unwrap();
        }

        let cleanup = || {
            roosty_db::cleanup_expired_jobs(
                &db,
                time::Duration::hours(24),
                time::Duration::days(30),
                3,
            )
        };
        let (first, second) = tokio::join!(cleanup(), cleanup());
        let first = first.unwrap();
        let second = second.unwrap();

        assert!(first.total() <= 3);
        assert!(second.total() <= 3);
        assert_eq!(first.total() + second.total(), 5);

        db.close().await.unwrap();
        postgresql.stop().await.unwrap();
    }

    /// Given a job claimed by a worker that stopped, when its claim expires, then the next poll
    /// reclaims it and records the new attempt.
    #[tokio::test]
    async fn recovers_expired_job_claims() {
        let (postgresql, db, _temp_dir) = migrated_test_database().await;
        let job_id = uuid::Uuid::now_v7();
        db.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
            INSERT INTO job (id, kind, payload, run_after, locked_at, locked_by)
            VALUES ($1, 'federation_follow_delivery', '{}'::jsonb, now() - interval '10 minutes',
                    now() - interval '10 minutes', 'stopped-worker')
            "#,
            vec![job_id.into()],
        ))
        .await
        .unwrap();

        let state = AppState::new(test_worker_config(), db.clone());
        assert!(
            worker_iteration(&db, &state, "recovery-test-worker")
                .await
                .unwrap()
        );

        let job = db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT completed_at, locked_at, locked_by, last_error, attempts FROM job WHERE id = $1",
                vec![job_id.into()],
            ))
            .await
            .unwrap()
            .unwrap();
        let completed_at: Option<time::OffsetDateTime> = job.try_get("", "completed_at").unwrap();
        let locked_at: Option<time::OffsetDateTime> = job.try_get("", "locked_at").unwrap();
        let locked_by: Option<String> = job.try_get("", "locked_by").unwrap();
        let last_error: Option<String> = job.try_get("", "last_error").unwrap();
        let attempts: i32 = job.try_get("", "attempts").unwrap();

        assert!(completed_at.is_none());
        assert!(locked_at.is_none());
        assert!(locked_by.is_none());
        assert_eq!(attempts, 1);
        assert_eq!(
            last_error.as_deref(),
            Some("invalid input: invalid follow delivery payload")
        );

        db.close().await.unwrap();
        postgresql.stop().await.unwrap();
    }

    /// Given the migrated database enum, every Rust job kind is stored and claimable exactly once.
    #[tokio::test]
    async fn database_and_worker_support_every_known_job_kind() {
        let (postgresql, db, _temp_dir) = migrated_test_database().await;
        let expected = JobKind::iter().collect::<HashSet<_>>();
        for kind in &expected {
            roosty_db::enqueue_job(
                &db,
                *kind,
                serde_json::json!({}),
                None,
                time::OffsetDateTime::now_utc(),
            )
            .await
            .unwrap();
        }
        let diagnostic_kinds = roosty_db::admin_job_diagnostics(&db, 100, None)
            .await
            .unwrap()
            .into_iter()
            .map(|job| job.kind)
            .collect::<HashSet<_>>();
        assert_eq!(diagnostic_kinds, expected);

        let mut claimed = HashSet::new();
        while let Some(job) =
            roosty_db::claim_due_job(&db, "all-kinds-worker", time::Duration::minutes(5))
                .await
                .unwrap()
        {
            assert!(claimed.insert(job.kind));
            assert!(roosty_db::mark_job_completed(&db, &job).await.unwrap());
        }

        assert_eq!(claimed, expected);

        db.close().await.unwrap();
        postgresql.stop().await.unwrap();
    }

    /// Given a job introduced by a newer deployment, an older worker leaves it available for a
    /// worker that understands its typed dispatch contract.
    #[tokio::test]
    async fn skips_unknown_future_job_kinds() {
        let (postgresql, db, _temp_dir) = migrated_test_database().await;
        db.execute(Statement::from_string(
            DatabaseBackend::Postgres,
            "ALTER TYPE job_kind ADD VALUE 'future_job'".to_owned(),
        ))
        .await
        .unwrap();
        let job_id = uuid::Uuid::now_v7();
        db.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO job (id, kind, payload, run_after) VALUES ($1, 'future_job', '{}'::jsonb, now())",
            vec![job_id.into()],
        ))
        .await
        .unwrap();

        assert!(
            roosty_db::claim_due_job(&db, "older-worker", time::Duration::minutes(5))
                .await
                .unwrap()
                .is_none()
        );
        let locked_by: Option<String> = db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT locked_by FROM job WHERE id = $1",
                vec![job_id.into()],
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get("", "locked_by")
            .unwrap();
        assert!(locked_by.is_none());

        db.close().await.unwrap();
        postgresql.stop().await.unwrap();
    }

    /// Given concurrent worker identities, when they claim due jobs, then PostgreSQL assigns each
    /// job to at most one worker through `FOR UPDATE SKIP LOCKED`.
    #[tokio::test]
    async fn concurrent_workers_claim_distinct_jobs() {
        let (postgresql, db, _temp_dir) = migrated_test_database().await;
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
        postgresql.stop().await.unwrap();
    }

    /// Given a media job with an unexpired lease, a targeted HTTP claimant cannot steal it; once
    /// expired, the same kind and deduplication key can be reclaimed.
    #[tokio::test]
    async fn targeted_job_claim_respects_active_lease() {
        let (postgresql, db, _temp_dir) = migrated_test_database().await;
        let key = "remote-media:targeted-claim";
        roosty_db::enqueue_job(
            &db,
            roosty_db::JobKind::FederationRemoteMediaFetch,
            serde_json::json!({"attachment_id": uuid::Uuid::now_v7()}),
            Some(key),
            time::OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();

        let first = roosty_db::claim_due_job_by_key(
            &db,
            "media-request-a",
            roosty_db::JobKind::FederationRemoteMediaFetch,
            key,
            time::Duration::minutes(5),
        )
        .await
        .unwrap()
        .unwrap();
        let leased = roosty_db::claim_due_job_by_key(
            &db,
            "media-request-b",
            roosty_db::JobKind::FederationRemoteMediaFetch,
            key,
            time::Duration::minutes(5),
        )
        .await
        .unwrap();
        assert!(leased.is_none());

        db.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE job SET locked_at = now() - interval '10 minutes' WHERE id = $1",
            vec![first.id.0.into()],
        ))
        .await
        .unwrap();
        let reclaimed = roosty_db::claim_due_job_by_key(
            &db,
            "media-request-b",
            roosty_db::JobKind::FederationRemoteMediaFetch,
            key,
            time::Duration::minutes(5),
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(reclaimed.id, first.id);
        assert_ne!(reclaimed.claim_id, first.claim_id);

        db.close().await.unwrap();
        postgresql.stop().await.unwrap();
    }

    /// Given an actor image URL changes while old bytes are downloading, completion for the old
    /// URL cannot publish those bytes into the actor's current cache entry.
    #[tokio::test]
    async fn profile_media_completion_requires_the_fetched_url() {
        let (postgresql, db, _temp_dir) = migrated_test_database().await;
        let actor_id = AccountId(uuid::Uuid::now_v7());
        db.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
            INSERT INTO remote_actor (
                id, activitypub_id, username, domain, inbox_url, public_key_id,
                public_key_pem, expires_at
            )
            VALUES ($1, $2, 'alice', 'remote.test', $3, $4, 'test-key', now() + interval '1 day')
            "#,
            vec![
                actor_id.0.into(),
                "https://remote.test/users/alice".into(),
                "https://remote.test/users/alice/inbox".into(),
                "https://remote.test/users/alice#main-key".into(),
            ],
        ))
        .await
        .unwrap();
        let old_url = "https://remote.test/old-avatar.png";
        let new_url = "https://remote.test/new-avatar.png";
        roosty_db::replace_remote_profile_media(
            &db,
            actor_id,
            roosty_db::NewRemoteProfileMedia {
                avatar_url: Some(old_url.to_owned()),
                header_url: None,
            },
        )
        .await
        .unwrap();
        let media = roosty_db::remote_profile_media_for_actor(&db, actor_id)
            .await
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        roosty_db::replace_remote_profile_media(
            &db,
            actor_id,
            roosty_db::NewRemoteProfileMedia {
                avatar_url: Some(new_url.to_owned()),
                header_url: None,
            },
        )
        .await
        .unwrap();

        let published = roosty_db::mark_remote_profile_media_ready(
            &db,
            media.id,
            old_url,
            "image/png".to_owned(),
            format!("remote/profile/{}.png", media.id),
            4,
            time::OffsetDateTime::now_utc() + time::Duration::days(1),
        )
        .await
        .unwrap();
        let current = roosty_db::find_remote_profile_media(&db, media.id)
            .await
            .unwrap()
            .unwrap();

        assert!(!published);
        assert_eq!(current.remote_url, new_url);
        assert_eq!(current.state, roosty_db::RemoteMediaState::Pending);
        assert!(current.file_path.is_none());

        db.close().await.unwrap();
        postgresql.stop().await.unwrap();
    }

    /// Given several scheduler loops, only one claims and advances the shared due row.
    #[tokio::test]
    async fn concurrent_trend_schedulers_enqueue_one_job() {
        let (postgresql, db, _temp_dir) = migrated_test_database().await;
        roosty_db::configure_trend_refresh_schedule(&db, time::Duration::minutes(5))
            .await
            .unwrap();

        let (first, second, third) = tokio::join!(
            roosty_db::enqueue_due_trend_refresh(&db),
            roosty_db::enqueue_due_trend_refresh(&db),
            roosty_db::enqueue_due_trend_refresh(&db),
        );
        let claims = [first.unwrap(), second.unwrap(), third.unwrap()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        assert_eq!(claims.len(), 1);

        let row = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                r#"SELECT
                     (SELECT count(*) FROM job
                      WHERE kind = 'trend_maintenance' AND completed_at IS NULL)
                       AS active_jobs,
                     interval_milliseconds, next_run_at
                   FROM trend_refresh_schedule WHERE id = 1"#
                    .to_owned(),
            ))
            .await
            .unwrap()
            .unwrap();
        let active_jobs: i64 = row.try_get("", "active_jobs").unwrap();
        let interval_milliseconds: i64 = row.try_get("", "interval_milliseconds").unwrap();
        let next_run_at: time::OffsetDateTime = row.try_get("", "next_run_at").unwrap();
        assert_eq!(active_jobs, 1);
        assert_eq!(interval_milliseconds, 300_000);
        assert_eq!(
            next_run_at
                .unix_timestamp_nanos()
                .div_euclid(1_000_000)
                .rem_euclid(i128::from(interval_milliseconds)),
            0
        );
        assert!(next_run_at > time::OffsetDateTime::now_utc());

        db.close().await.unwrap();
        postgresql.stop().await.unwrap();
    }

    /// Concurrent process schedulers reuse one active refresh and allow work after completion.
    #[tokio::test]
    async fn concurrent_suggestion_schedulers_enqueue_one_job() {
        let (postgresql, db, _temp_dir) = migrated_test_database().await;

        let (first, second, third) = tokio::join!(
            roosty_db::enqueue_account_suggestion_refresh(&db),
            roosty_db::enqueue_account_suggestion_refresh(&db),
            roosty_db::enqueue_account_suggestion_refresh(&db),
        );
        let jobs = [first.unwrap(), second.unwrap(), third.unwrap()];
        assert!(jobs.iter().all(|job_id| job_id == &jobs[0]));

        let row = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT count(*) AS active_jobs FROM job
                 WHERE kind = 'account_suggestion_maintenance' AND completed_at IS NULL"
                    .to_owned(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.try_get::<i64>("", "active_jobs").unwrap(), 1);
        db.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE job SET completed_at = now() WHERE id = $1",
            vec![jobs[0].0.into()],
        ))
        .await
        .unwrap();
        let next = roosty_db::enqueue_account_suggestion_refresh(&db)
            .await
            .unwrap();
        assert_ne!(next, jobs[0]);

        db.close().await.unwrap();
        postgresql.stop().await.unwrap();
    }

    /// A restarted process waits for its configured period before scheduling a refresh.
    #[tokio::test]
    async fn suggestion_scheduler_uses_process_local_interval() {
        let (postgresql, db, _temp_dir) = migrated_test_database().await;
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let scheduler = tokio::spawn(account_suggestion_scheduler_loop(
            db.clone(),
            time::Duration::milliseconds(250),
            shutdown_rx,
        ));

        sleep(Duration::from_millis(50)).await;
        assert_eq!(active_suggestion_refresh_jobs(&db).await, 0);
        sleep(Duration::from_millis(300)).await;
        assert_eq!(active_suggestion_refresh_jobs(&db).await, 1);

        shutdown_tx.send(true).unwrap();
        scheduler.await.unwrap().unwrap();
        db.close().await.unwrap();
        postgresql.stop().await.unwrap();
    }

    async fn active_suggestion_refresh_jobs(db: &roosty_db::DbConnection) -> i64 {
        db.query_one(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT count(*) AS active_jobs FROM job
             WHERE kind = 'account_suggestion_maintenance' AND completed_at IS NULL"
                .to_owned(),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get("", "active_jobs")
        .unwrap()
    }

    /// A scheduler skips a locked due row instead of waiting for another instance.
    #[tokio::test]
    async fn trend_scheduler_skips_a_concurrently_locked_schedule() {
        let (postgresql, db, _temp_dir) = migrated_test_database().await;
        roosty_db::configure_trend_refresh_schedule(&db, time::Duration::minutes(5))
            .await
            .unwrap();
        let txn = db.begin().await.unwrap();
        txn.execute(Statement::from_string(
            DatabaseBackend::Postgres,
            "UPDATE trend_refresh_schedule SET updated_at = now() WHERE id = 1".to_owned(),
        ))
        .await
        .unwrap();

        let claim = timeout(
            Duration::from_secs(1),
            roosty_db::enqueue_due_trend_refresh(&db),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(claim.is_none());
        txn.rollback().await.unwrap();

        db.close().await.unwrap();
        postgresql.stop().await.unwrap();
    }

    /// The database cadence is shared configuration, while active work suppresses overlap.
    #[tokio::test]
    async fn trend_schedule_rejects_mismatches_and_active_overlap() {
        let (postgresql, db, _temp_dir) = migrated_test_database().await;
        roosty_db::configure_trend_refresh_schedule(&db, time::Duration::minutes(5))
            .await
            .unwrap();
        roosty_db::configure_trend_refresh_schedule(&db, time::Duration::minutes(5))
            .await
            .unwrap();
        let mismatch = roosty_db::configure_trend_refresh_schedule(&db, time::Duration::minutes(6))
            .await
            .unwrap_err();
        assert!(matches!(mismatch, RoostyError::Configuration(_)));

        let job_id = roosty_db::enqueue_due_trend_refresh(&db)
            .await
            .unwrap()
            .unwrap();
        db.execute(Statement::from_string(
            DatabaseBackend::Postgres,
            "UPDATE trend_refresh_schedule SET next_run_at = now() - interval '1 minute' WHERE id = 1"
                .to_owned(),
        ))
        .await
        .unwrap();
        assert!(
            roosty_db::enqueue_due_trend_refresh(&db)
                .await
                .unwrap()
                .is_none()
        );
        db.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE job SET completed_at = now() WHERE id = $1",
            vec![job_id.0.into()],
        ))
        .await
        .unwrap();
        assert!(
            roosty_db::enqueue_due_trend_refresh(&db)
                .await
                .unwrap()
                .is_some()
        );

        db.close().await.unwrap();
        postgresql.stop().await.unwrap();
    }

    /// Given a reclaimed job, when its former owner reports any outcome, then the stale writes do
    /// not override the active claim.
    #[tokio::test]
    async fn stale_worker_outcomes_do_not_override_reclaimed_jobs() {
        let (postgresql, db, _temp_dir) = migrated_test_database().await;
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
        postgresql.stop().await.unwrap();
    }

    /// Given mixed account and status eligibility, sitemap queries expose only promotable URLs.
    #[tokio::test]
    async fn search_sitemap_queries_filter_profiles_and_statuses() {
        let (postgresql, db, _temp_dir) = migrated_test_database().await;
        let alice = roosty_db::create_local_account(
            &db,
            "alice",
            "alice@example.com",
            "unused-password-hash",
        )
        .await
        .unwrap();
        let bob =
            roosty_db::create_local_account(&db, "bob", "bob@example.com", "unused-password-hash")
                .await
                .unwrap();
        db.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE local_account SET discoverable = false WHERE id = $1",
            [bob.into()],
        ))
        .await
        .unwrap();
        let public_status = uuid::Uuid::now_v7();
        let sensitive_status = uuid::Uuid::now_v7();
        for (id, sensitive) in [(public_status, false), (sensitive_status, true)] {
            db.execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO local_status (id, account_id, content, visibility, sensitive) VALUES ($1, $2, 'hello', 'public', $3)",
                [id.into(), alice.into(), sensitive.into()],
            ))
            .await
            .unwrap();
        }

        let profile_chunks = roosty_db::search_profile_sitemap_chunks(&db).await.unwrap();
        assert_eq!(profile_chunks.len(), 1);
        let profiles = roosty_db::search_profile_sitemap_urls(&db, profile_chunks[0].cursor)
            .await
            .unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].username, "alice");

        let status_chunks = roosty_db::search_status_sitemap_chunks(&db).await.unwrap();
        assert_eq!(status_chunks.len(), 1);
        let statuses = roosty_db::search_status_sitemap_urls(&db, status_chunks[0].cursor)
            .await
            .unwrap();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].id, public_status);

        db.close().await.unwrap();
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
            vapid_private_key: None,
            object_storage_backend: ObjectStorageBackend::Local,
            media_root: "./media".to_owned(),
            registration_mode: RegistrationMode::Closed,
            search_indexing_enabled: true.into(),
            federation_enabled: true,
            federation_key_encryption_secret: Some(
                "test-federation-key-encryption-secret-000".to_owned(),
            ),
            federation_allowed_domains: vec!["*".to_owned()],
            federation_delivery_max_age: time::Duration::days(7),
            remote_media_cache_ttl: time::Duration::days(30),
            remote_media_max_bytes: 40 * 1024 * 1024,
            remote_media_fetch_concurrency: 5,
            preview_card_fetch_concurrency: 5,
            worker_concurrency: 4,
            successful_job_retention: time::Duration::hours(24),
            permanently_failed_job_retention: time::Duration::days(30),
            trends_refresh_interval: time::Duration::minutes(5),
            account_suggestions_refresh_interval: time::Duration::hours(24),
            scheduled_statuses: ScheduledStatusConfig::default(),
            streaming: StreamingConfig::default(),
            instance_name: "Worker test".to_owned(),
            instance_description: None,
        }
    }

    /// Starts a migrated temporary PostgreSQL database for CLI-adjacent DB tests.
    async fn migrated_test_database() -> (PostgreSQL, roosty_db::DbConnection, TempDir) {
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
            fs::create_dir_all(parent).unwrap();
        }

        let settings = settings(&data_dir, password_file);
        let mut postgresql = PostgreSQL::new(settings);

        postgresql.setup().await.unwrap();
        postgresql.start().await.unwrap();
        postgresql.create_database(&database_name).await.unwrap();

        let database_url = postgresql.settings().url(&database_name);
        let db = roosty_db::connect(&database_url).await.unwrap();
        Migrator::up(&db, None).await.unwrap();

        (postgresql, db, temp_dir)
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
