use std::time::{Duration, SystemTime, UNIX_EPOCH};

use postgresql_embedded::{PostgreSQL, Settings, SettingsBuilder, VersionReq};
use roosty_migration::Migrator;
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
    assert!(table_exists(database.connection(), "local_follow").await);
    assert!(table_exists(database.connection(), "local_media_attachment").await);
    assert!(table_exists(database.connection(), "local_notification").await);
    assert!(table_exists(database.connection(), "local_status_reblog").await);
    assert!(table_exists(database.connection(), "local_conversation").await);
    assert!(table_exists(database.connection(), "local_conversation_account").await);
    assert!(table_exists(database.connection(), "local_tag").await);
    assert!(table_exists(database.connection(), "local_status_tag").await);
    assert!(table_exists(database.connection(), "local_tag_follow").await);
    assert!(table_exists(database.connection(), "local_timeline_marker").await);
    assert!(table_exists(database.connection(), "local_account_block").await);
    assert!(table_exists(database.connection(), "local_account_mute").await);
    assert!(table_exists(database.connection(), "remote_actor").await);
    // Account settings are part of the local account schema until profile
    // boundaries justify a separate table.
    assert!(column_exists(database.connection(), "local_account", "display_name").await);
    assert!(column_exists(database.connection(), "local_account", "default_visibility").await);
    assert!(column_exists(database.connection(), "local_account", "profile_fields").await);
    assert!(column_exists(database.connection(), "local_account", "avatar_file_path").await);
    assert!(column_exists(database.connection(), "local_account", "header_file_path").await);
    assert!(column_exists(database.connection(), "local_status", "deleted_at").await);
    assert!(column_exists(database.connection(), "local_status", "conversation_id").await);
    assert!(column_exists(database.connection(), "local_status_favourite", "id").await);
    assert!(column_exists(database.connection(), "local_status_bookmark", "id").await);
    assert!(column_exists(database.connection(), "local_follow", "id").await);
    assert!(column_exists(database.connection(), "local_media_attachment", "file_path").await);
    assert!(
        column_exists(
            database.connection(),
            "local_media_attachment",
            "preview_file_path"
        )
        .await
    );
    assert!(
        column_exists(
            database.connection(),
            "local_media_attachment",
            "preview_width"
        )
        .await
    );
    assert!(
        column_exists(
            database.connection(),
            "local_media_attachment",
            "preview_height"
        )
        .await
    );
    assert!(column_exists(database.connection(), "local_media_attachment", "blurhash").await);
    assert!(
        column_exists(
            database.connection(),
            "local_media_attachment",
            "status_order"
        )
        .await
    );
    assert!(column_exists(database.connection(), "local_notification", "id").await);
    assert!(column_exists(database.connection(), "local_notification", "account_id").await);
    assert!(
        column_exists(
            database.connection(),
            "local_notification",
            "notification_type"
        )
        .await
    );
    assert!(
        column_exists(
            database.connection(),
            "local_notification",
            "actor_account_id"
        )
        .await
    );
    assert!(column_exists(database.connection(), "local_notification", "status_id").await);
    assert!(column_exists(database.connection(), "local_notification", "dismissed_at").await);
    assert!(
        column_exists(
            database.connection(),
            "local_timeline_marker",
            "last_read_id"
        )
        .await
    );
    assert!(column_exists(database.connection(), "local_timeline_marker", "version").await);
    assert!(
        column_exists(
            database.connection(),
            "local_account_block",
            "target_account_id"
        )
        .await
    );
    assert!(column_exists(database.connection(), "local_account_mute", "notifications").await);
    assert!(column_exists(database.connection(), "local_account_mute", "expires_at").await);
    assert!(column_exists(database.connection(), "remote_actor", "profile_created_at").await);
    assert!(column_exists(database.connection(), "remote_actor", "followers_url").await);
    assert!(
        column_exists(
            database.connection(),
            "processed_inbox_activity",
            "payload_digest"
        )
        .await
    );
    assert!(
        column_exists(
            database.connection(),
            "processed_inbox_activity",
            "activity_type"
        )
        .await
    );
    assert!(column_exists(database.connection(), "processed_inbox_activity", "outcome").await);
    assert!(column_exists(database.connection(), "local_status_reblog", "id").await);
    assert!(column_exists(database.connection(), "local_status_reblog", "account_id").await);
    assert!(column_exists(database.connection(), "local_status_reblog", "status_id").await);
    assert!(
        column_exists(
            database.connection(),
            "local_conversation",
            "last_status_id"
        )
        .await
    );
    assert!(
        column_exists(
            database.connection(),
            "local_conversation_account",
            "cursor_id"
        )
        .await
    );
    assert!(
        column_exists(
            database.connection(),
            "local_conversation_account",
            "unread"
        )
        .await
    );
    assert!(
        column_exists(
            database.connection(),
            "local_conversation_account",
            "hidden_at"
        )
        .await
    );
    assert!(column_exists(database.connection(), "local_tag", "name").await);
    assert!(column_exists(database.connection(), "local_status_tag", "status_id").await);
    assert!(column_exists(database.connection(), "local_status_tag", "tag_id").await);
    assert!(column_exists(database.connection(), "local_tag_follow", "account_id").await);
    assert!(column_exists(database.connection(), "local_tag_follow", "tag_id").await);
}

/// Existing cached actors retain an unknown followers collection across the nullable upgrade.
#[test_context(EmbeddedDatabase)]
#[tokio::test]
async fn followers_url_upgrade_and_rollback_preserve_legacy_actors(
    database: &mut EmbeddedDatabase,
) {
    Migrator::up(database.connection(), Some(46)).await.unwrap();
    database
        .connection()
        .execute_unprepared(
            r#"
            INSERT INTO remote_actor (
                id, activitypub_id, username, domain, inbox_url,
                public_key_id, public_key_pem, expires_at
            ) VALUES (
                '00000000-0000-0000-0000-000000000047',
                'https://remote.test/users/legacy', 'legacy', 'remote.test',
                'https://remote.test/inbox',
                'https://remote.test/users/legacy#main-key', 'key', now() + interval '1 day'
            )
            "#,
        )
        .await
        .unwrap();

    Migrator::up(database.connection(), None).await.unwrap();
    let row = database
        .connection()
        .query_one(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT followers_url FROM remote_actor WHERE username = 'legacy'",
        ))
        .await
        .unwrap()
        .unwrap();
    assert!(
        row.try_get::<Option<String>>("", "followers_url")
            .unwrap()
            .is_none()
    );

    Migrator::down(database.connection(), Some(1))
        .await
        .unwrap();
    assert!(!column_exists(database.connection(), "remote_actor", "followers_url").await);
    assert!(table_exists(database.connection(), "remote_actor").await);
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
    assert!(table_exists(database.connection(), "local_follow").await);
    assert!(table_exists(database.connection(), "local_media_attachment").await);
    assert!(table_exists(database.connection(), "local_notification").await);
    assert!(table_exists(database.connection(), "local_status_reblog").await);
    assert!(table_exists(database.connection(), "local_conversation").await);
    assert!(table_exists(database.connection(), "local_conversation_account").await);
    assert!(table_exists(database.connection(), "local_tag").await);
    assert!(table_exists(database.connection(), "local_status_tag").await);
    assert!(table_exists(database.connection(), "local_tag_follow").await);
    assert!(table_exists(database.connection(), "local_timeline_marker").await);
    assert!(table_exists(database.connection(), "local_account_block").await);
    assert!(table_exists(database.connection(), "local_account_mute").await);

    Migrator::down(database.connection(), None).await.unwrap();
    assert!(!table_exists(database.connection(), "job").await);
    assert!(!table_exists(database.connection(), "local_account").await);
    assert!(!table_exists(database.connection(), "oauth_application").await);
    assert!(!table_exists(database.connection(), "local_status").await);
    assert!(!table_exists(database.connection(), "local_status_favourite").await);
    assert!(!table_exists(database.connection(), "local_status_bookmark").await);
    assert!(!table_exists(database.connection(), "local_follow").await);
    assert!(!table_exists(database.connection(), "local_media_attachment").await);
    assert!(!table_exists(database.connection(), "local_notification").await);
    assert!(!table_exists(database.connection(), "local_status_reblog").await);
    assert!(!table_exists(database.connection(), "local_conversation").await);
    assert!(!table_exists(database.connection(), "local_conversation_account").await);
    assert!(!table_exists(database.connection(), "local_tag").await);
    assert!(!table_exists(database.connection(), "local_status_tag").await);
    assert!(!table_exists(database.connection(), "local_tag_follow").await);
    assert!(!table_exists(database.connection(), "local_timeline_marker").await);
    assert!(!table_exists(database.connection(), "local_account_block").await);
    assert!(!table_exists(database.connection(), "local_account_mute").await);
}

/// A legacy ID-only replay marker survives the payload-aware ledger upgrade.
#[test_context(EmbeddedDatabase)]
#[tokio::test]
async fn replay_ledger_upgrade_preserves_legacy_rows(database: &mut EmbeddedDatabase) {
    Migrator::up(database.connection(), Some(45)).await.unwrap();
    database
        .connection()
        .execute_unprepared(
            r#"
            INSERT INTO remote_actor (
                id, activitypub_id, username, domain, inbox_url,
                public_key_id, public_key_pem, expires_at
            ) VALUES (
                '00000000-0000-0000-0000-000000000001',
                'https://remote.test/users/alice', 'alice', 'remote.test',
                'https://remote.test/inbox',
                'https://remote.test/users/alice#main-key', 'key', now() + interval '1 day'
            );
            INSERT INTO processed_inbox_activity (activity_id, remote_actor_id)
            VALUES (
                'https://remote.test/activities/legacy',
                '00000000-0000-0000-0000-000000000001'
            );
            "#,
        )
        .await
        .unwrap();

    Migrator::up(database.connection(), None).await.unwrap();
    let row = database
        .connection()
        .query_one(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT payload_digest, activity_type, outcome FROM processed_inbox_activity WHERE activity_id = 'https://remote.test/activities/legacy'",
        ))
        .await
        .unwrap()
        .unwrap();
    assert!(
        row.try_get::<Option<Vec<u8>>>("", "payload_digest")
            .unwrap()
            .is_none()
    );
    assert!(
        row.try_get::<Option<String>>("", "activity_type")
            .unwrap()
            .is_none()
    );
    assert!(
        row.try_get::<Option<String>>("", "outcome")
            .unwrap()
            .is_none()
    );
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
            .prefix("roosty-migration-")
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

    format!("roosty_migration_{}_{}", std::process::id(), timestamp)
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
