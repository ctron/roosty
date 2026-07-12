use std::time::{Duration, SystemTime, UNIX_EPOCH};

use postgresql_embedded::{PostgreSQL, Settings, SettingsBuilder, VersionReq};
use roost_migration::Migrator;
use sea_orm::{ConnectionTrait, Database, DatabaseBackend, DatabaseConnection, Statement};
use sea_orm_migration::MigratorTrait;
use tempfile::TempDir;
use test_context::{AsyncTestContext, test_context};

const EMBEDDED_POSTGRES_VERSION: &str = "=18.4.0";

#[test_context(EmbeddedDatabase)]
#[tokio::test]
async fn migrations_run_up(database: &mut EmbeddedDatabase) {
    // A fresh database should receive every table and account settings column
    // required by the current application code.
    Migrator::up(database.connection(), None).await.unwrap();

    assert!(table_exists(database.connection(), "job").await);
    assert!(table_exists(database.connection(), "local_account").await);
    assert!(table_exists(database.connection(), "oauth_application").await);
    assert!(table_exists(database.connection(), "oauth_authorization_code").await);
    assert!(table_exists(database.connection(), "oauth_access_token").await);
    assert!(table_exists(database.connection(), "oauth_refresh_token").await);
    assert!(table_exists(database.connection(), "local_status").await);
    assert!(table_exists(database.connection(), "local_status_favourite").await);
    assert!(table_exists(database.connection(), "local_status_bookmark").await);
    // Account settings are part of the local account schema until profile
    // boundaries justify a separate table.
    assert!(column_exists(database.connection(), "local_account", "display_name").await);
    assert!(column_exists(database.connection(), "local_account", "default_visibility").await);
    assert!(column_exists(database.connection(), "local_account", "profile_fields").await);
    assert!(column_exists(database.connection(), "local_status", "deleted_at").await);
}

#[test_context(EmbeddedDatabase)]
#[tokio::test]
async fn migrations_run_up_and_down(database: &mut EmbeddedDatabase) {
    // Down migrations should leave a disposable test database clean enough for
    // repeated migration runs.
    Migrator::up(database.connection(), None).await.unwrap();
    assert!(table_exists(database.connection(), "job").await);
    assert!(table_exists(database.connection(), "local_account").await);
    assert!(table_exists(database.connection(), "oauth_application").await);
    assert!(table_exists(database.connection(), "local_status").await);
    assert!(table_exists(database.connection(), "local_status_favourite").await);
    assert!(table_exists(database.connection(), "local_status_bookmark").await);

    Migrator::down(database.connection(), None).await.unwrap();
    assert!(!table_exists(database.connection(), "job").await);
    assert!(!table_exists(database.connection(), "local_account").await);
    assert!(!table_exists(database.connection(), "oauth_application").await);
    assert!(!table_exists(database.connection(), "local_status").await);
    assert!(!table_exists(database.connection(), "local_status_favourite").await);
    assert!(!table_exists(database.connection(), "local_status_bookmark").await);
}

struct EmbeddedDatabase {
    postgresql: PostgreSQL,
    connection: DatabaseConnection,
    database_name: String,
    _temp_dir: TempDir,
}

impl AsyncTestContext for EmbeddedDatabase {
    async fn setup() -> Self {
        let temp_dir = tempfile::Builder::new()
            .prefix("roost-migration-")
            .tempdir()
            .unwrap();
        let root = temp_dir.path();
        let database_name = unique_name();
        let data_dir = root.join("data").join(&database_name);
        let password_file = root
            .join("passwords")
            .join(format!("{database_name}.pgpass"));

        if let Some(parent) = password_file.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }

        let settings = embedded_postgres_settings(&data_dir, password_file);
        let mut postgresql = PostgreSQL::new(settings);

        postgresql.setup().await.unwrap();
        postgresql.start().await.unwrap();
        postgresql.create_database(&database_name).await.unwrap();

        let database_url = postgresql.settings().url(&database_name);
        let connection = Database::connect(&database_url).await.unwrap();

        Self {
            postgresql,
            connection,
            database_name,
            _temp_dir: temp_dir,
        }
    }

    async fn teardown(self) {
        self.connection.close().await.unwrap();
        self.postgresql
            .drop_database(&self.database_name)
            .await
            .unwrap();
        self.postgresql.stop().await.unwrap();
    }
}

/// Build embedded PostgreSQL settings with a fixed reusable installation.
fn embedded_postgres_settings(
    data_dir: &std::path::Path,
    password_file: std::path::PathBuf,
) -> Settings {
    SettingsBuilder::new()
        .version(VersionReq::parse(EMBEDDED_POSTGRES_VERSION).unwrap())
        .data_dir(data_dir)
        .password_file(password_file)
        .timeout(Some(Duration::from_secs(30)))
        .build()
}

impl EmbeddedDatabase {
    fn connection(&self) -> &DatabaseConnection {
        &self.connection
    }
}

fn unique_name() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time is before the Unix epoch")
        .as_nanos();

    format!("roost_migration_{}_{}", std::process::id(), timestamp)
}

async fn table_exists(connection: &DatabaseConnection, table_name: &str) -> bool {
    let row = connection
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
            SELECT EXISTS (
                SELECT 1
                FROM information_schema.tables
                WHERE table_schema = 'public'
                  AND table_name = $1
            ) AS table_exists
            "#,
            vec![table_name.to_owned().into()],
        ))
        .await
        .unwrap()
        .expect("table existence query returned no rows");

    row.try_get::<bool>("", "table_exists").unwrap()
}

async fn column_exists(
    connection: &DatabaseConnection,
    table_name: &str,
    column_name: &str,
) -> bool {
    let row = connection
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
            SELECT EXISTS (
                SELECT 1
                FROM information_schema.columns
                WHERE table_schema = 'public'
                  AND table_name = $1
                  AND column_name = $2
            ) AS column_exists
            "#,
            vec![table_name.to_owned().into(), column_name.to_owned().into()],
        ))
        .await
        .unwrap()
        .expect("column existence query returned no rows");

    row.try_get::<bool>("", "column_exists").unwrap()
}
