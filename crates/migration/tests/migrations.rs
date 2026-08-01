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
    assert!(table_exists(database.connection(), "scheduled_status").await);
    assert!(table_exists(database.connection(), "status_creation_idempotency").await);
    assert!(
        column_exists(
            database.connection(),
            "local_media_attachment",
            "scheduled_status_id"
        )
        .await
    );
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
    assert!(table_exists(database.connection(), "local_account_suggestion_dismissal").await);
    assert!(
        index_exists(
            database.connection(),
            "remote_following_suggestion_follower_idx"
        )
        .await
    );
    assert!(
        index_exists(
            database.connection(),
            "remote_follow_suggestion_follower_idx"
        )
        .await
    );
    assert!(table_exists(database.connection(), "remote_actor").await);
    assert!(table_exists(database.connection(), "local_remote_account_block").await);
    assert!(table_exists(database.connection(), "remote_local_account_block").await);
    assert!(table_exists(database.connection(), "local_remote_account_mute").await);
    assert!(table_exists(database.connection(), "streaming_event").await);
    assert!(table_exists(database.connection(), "push_subscription").await);
    assert!(table_exists(database.connection(), "trend_refresh_schedule").await);
    assert!(table_exists(database.connection(), "preview_card").await);
    assert!(table_exists(database.connection(), "status_preview_card").await);
    assert!(table_exists(database.connection(), "status_preview_scan").await);
    assert!(table_exists(database.connection(), "status_search_document").await);
    assert!(index_exists(database.connection(), "status_search_document_trgm_idx").await);
    assert!(table_exists(database.connection(), "link_daily_usage").await);
    assert!(table_exists(database.connection(), "link_trend").await);
    assert!(table_exists(database.connection(), "preview_fetch_host").await);
    assert!(index_exists(database.connection(), "link_trend_rank_idx").await);
    assert!(table_exists(database.connection(), "status_quote").await);
    assert!(table_exists(database.connection(), "local_status_pin").await);
    assert!(table_exists(database.connection(), "remote_status_pin").await);
    assert!(table_exists(database.connection(), "local_featured_tag").await);
    assert!(table_exists(database.connection(), "local_list").await);
    assert!(table_exists(database.connection(), "local_list_local_member").await);
    assert!(table_exists(database.connection(), "local_list_remote_member").await);
    assert!(table_exists(database.connection(), "remote_featured_tag").await);
    assert!(table_exists(database.connection(), "local_status_local_mention").await);
    assert!(table_exists(database.connection(), "remote_status_local_mention").await);
    assert!(table_exists(database.connection(), "local_status_edit").await);
    assert!(table_exists(database.connection(), "local_status_edit_media").await);
    assert!(table_exists(database.connection(), "remote_status_edit").await);
    assert!(table_exists(database.connection(), "remote_status_edit_media").await);
    assert!(table_exists(database.connection(), "remote_status_tag").await);
    assert!(table_exists(database.connection(), "local_notification_policy").await);
    assert!(!trigger_exists(database.connection(), "local_account_notification_policy").await);
    assert!(
        !function_exists(
            database.connection(),
            "create_default_local_notification_policy"
        )
        .await
    );
    assert!(table_exists(database.connection(), "local_notification_permission").await);
    assert!(table_exists(database.connection(), "local_notification_request").await);
    assert!(table_exists(database.connection(), "admin_audit_log").await);
    assert!(table_exists(database.connection(), "federation_domain_block").await);
    assert!(table_exists(database.connection(), "status_trend_metric").await);
    assert!(table_exists(database.connection(), "tag_daily_actor_usage").await);
    assert!(table_exists(database.connection(), "tag_daily_usage").await);
    assert!(table_exists(database.connection(), "tag_trend").await);
    assert!(table_exists(database.connection(), "trend_dirty").await);
    assert!(table_exists(database.connection(), "trend_refresh_schedule").await);
    assert!(!roosty_trend_routines_exist(database.connection()).await);
    assert!(column_exists(database.connection(), "job", "permanently_failed_at").await);
    assert!(column_exists(database.connection(), "local_notification", "filtered").await);
    assert!(
        column_exists(
            database.connection(),
            "local_notification",
            "notification_request_id"
        )
        .await
    );
    assert!(column_exists(database.connection(), "local_account", "limited_at").await);
    assert!(column_exists(database.connection(), "remote_actor", "limited_at").await);
    assert!(column_exists(database.connection(), "local_account", "suspended_at").await);
    assert!(column_exists(database.connection(), "local_account", "data_purged_at").await);
    assert!(column_exists(database.connection(), "remote_actor", "suspended_at").await);
    assert!(column_exists(database.connection(), "remote_actor", "data_purged_at").await);
    assert!(column_exists(database.connection(), "local_account", "last_status_at").await);
    assert!(column_exists(database.connection(), "remote_actor", "discoverable").await);
    assert!(column_exists(database.connection(), "remote_actor", "last_status_at").await);
    assert!(index_exists(database.connection(), "local_account_directory_active_idx").await);
    assert!(index_exists(database.connection(), "local_account_directory_new_idx").await);
    assert!(index_exists(database.connection(), "remote_actor_directory_active_idx").await);
    assert!(index_exists(database.connection(), "remote_actor_directory_new_idx").await);
    assert!(
        column_exists(
            database.connection(),
            "local_status",
            "quote_approval_policy"
        )
        .await
    );
    assert!(
        column_exists(
            database.connection(),
            "remote_status",
            "quote_automatic_policy"
        )
        .await
    );
    assert!(
        column_exists(
            database.connection(),
            "remote_media_attachment",
            "status_order"
        )
        .await
    );
    assert!(column_exists(database.connection(), "streaming_event", "sequence").await);
    assert!(
        column_exists(
            database.connection(),
            "streaming_event",
            "origin_process_id"
        )
        .await
    );
    assert!(column_exists(database.connection(), "streaming_event", "recipient_ids").await);
    assert!(column_exists(database.connection(), "streaming_event", "status_origin").await);
    assert!(column_exists(database.connection(), "streaming_event", "has_media").await);
    assert!(
        column_exists(
            database.connection(),
            "streaming_event",
            "notification_recipient_ids"
        )
        .await
    );
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
    assert!(column_exists(database.connection(), "remote_actor", "featured_url").await);
    assert!(column_exists(database.connection(), "remote_actor", "featured_tags_url").await);
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
    assert!(column_exists(database.connection(), "remote_following", "show_reblogs").await);
    assert!(column_exists(database.connection(), "remote_following", "notify").await);
}

/// The preview-card migration is independently reversible without removing the shared trend cache.
#[test_context(EmbeddedDatabase)]
#[tokio::test]
async fn preview_card_upgrade_and_rollback_are_self_contained(database: &mut EmbeddedDatabase) {
    Migrator::up(database.connection(), Some(71)).await.unwrap();
    Migrator::up(database.connection(), Some(1)).await.unwrap();
    assert!(table_exists(database.connection(), "preview_card").await);
    assert!(table_exists(database.connection(), "status_preview_scan").await);

    Migrator::down(database.connection(), Some(1))
        .await
        .unwrap();
    assert!(!table_exists(database.connection(), "preview_card").await);
    assert!(!table_exists(database.connection(), "status_preview_scan").await);
    assert!(table_exists(database.connection(), "trend_dirty").await);
}

/// Databases upgraded from the trigger-backed policy invariant hand ownership to Rust.
#[test_context(EmbeddedDatabase)]
#[tokio::test]
async fn notification_policy_trigger_upgrade_and_rollback(database: &mut EmbeddedDatabase) {
    Migrator::up(database.connection(), Some(72)).await.unwrap();
    database
        .connection()
        .execute_unprepared(
            r#"
            CREATE FUNCTION create_default_local_notification_policy() RETURNS trigger AS $$
            BEGIN
                INSERT INTO local_notification_policy (account_id) VALUES (NEW.id);
                RETURN NEW;
            END;
            $$ LANGUAGE plpgsql;
            CREATE TRIGGER local_account_notification_policy
                AFTER INSERT ON local_account FOR EACH ROW
                EXECUTE FUNCTION create_default_local_notification_policy();
            "#,
        )
        .await
        .unwrap();

    Migrator::up(database.connection(), Some(1)).await.unwrap();
    assert!(!trigger_exists(database.connection(), "local_account_notification_policy").await);
    assert!(
        !function_exists(
            database.connection(),
            "create_default_local_notification_policy"
        )
        .await
    );

    Migrator::down(database.connection(), Some(1))
        .await
        .unwrap();
    assert!(trigger_exists(database.connection(), "local_account_notification_policy").await);
    assert!(
        function_exists(
            database.connection(),
            "create_default_local_notification_policy"
        )
        .await
    );
}

/// Existing status activity is backfilled for directory ordering and rollback removes only new columns.
#[test_context(EmbeddedDatabase)]
#[tokio::test]
async fn profile_directory_upgrade_backfills_and_rolls_back(database: &mut EmbeddedDatabase) {
    Migrator::up(database.connection(), Some(70)).await.unwrap();
    database
        .connection()
        .execute_unprepared(
            r#"
            INSERT INTO local_account (id, username, email, password_hash)
            VALUES (
                '10000000-0000-0000-0000-000000000071',
                'local_directory', 'directory@example.test', 'hash'
            );
            INSERT INTO local_status (
                id, account_id, content, visibility, created_at
            ) VALUES (
                '20000000-0000-0000-0000-000000000071',
                '10000000-0000-0000-0000-000000000071',
                'directory status', 'public', '2026-07-27 12:00:00+00'
            );
            INSERT INTO remote_actor (
                id, activitypub_id, username, domain, inbox_url,
                public_key_id, public_key_pem, expires_at
            ) VALUES (
                '30000000-0000-0000-0000-000000000071',
                'https://remote.test/users/directory', 'directory', 'remote.test',
                'https://remote.test/users/directory/inbox',
                'https://remote.test/users/directory#main-key', 'key',
                now() + interval '1 day'
            );
            INSERT INTO remote_status (
                id, activitypub_id, remote_actor_id, content, visibility,
                published_at, updated_at, object
            ) VALUES (
                '40000000-0000-0000-0000-000000000071',
                'https://remote.test/statuses/directory',
                '30000000-0000-0000-0000-000000000071',
                'remote directory status', 'public',
                '2026-07-28 12:00:00+00', '2026-07-28 12:00:00+00', '{}'::jsonb
            );
            "#,
        )
        .await
        .unwrap();

    Migrator::up(database.connection(), Some(1)).await.unwrap();
    let local = database
        .connection()
        .query_one(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT last_status_at IS NOT NULL AS populated FROM local_account WHERE username = 'local_directory'",
        ))
        .await
        .unwrap()
        .unwrap();
    let remote = database
        .connection()
        .query_one(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT discoverable, last_status_at IS NOT NULL AS populated FROM remote_actor WHERE username = 'directory'",
        ))
        .await
        .unwrap()
        .unwrap();
    assert!(local.try_get::<bool>("", "populated").unwrap());
    assert!(remote.try_get::<bool>("", "populated").unwrap());
    assert_eq!(
        remote.try_get::<Option<bool>>("", "discoverable").unwrap(),
        None
    );

    Migrator::down(database.connection(), Some(1))
        .await
        .unwrap();
    assert!(!column_exists(database.connection(), "local_account", "last_status_at").await);
    assert!(!column_exists(database.connection(), "remote_actor", "discoverable").await);
    assert!(!column_exists(database.connection(), "remote_actor", "last_status_at").await);
    assert!(table_exists(database.connection(), "remote_actor").await);
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

    Migrator::up(database.connection(), Some(1)).await.unwrap();
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

/// Existing unresolved replies and reply collections are queued once during thread hydration.
#[test_context(EmbeddedDatabase)]
#[tokio::test]
async fn remote_thread_upgrade_queues_bounded_context_repairs(database: &mut EmbeddedDatabase) {
    Migrator::up(database.connection(), Some(65)).await.unwrap();
    database
        .connection()
        .execute_unprepared(
            r#"
            INSERT INTO remote_actor (
                id, activitypub_id, username, domain, inbox_url,
                public_key_id, public_key_pem, expires_at
            ) VALUES (
                '10000000-0000-0000-0000-000000000066',
                'https://remote.test/users/alice', 'alice', 'remote.test',
                'https://remote.test/inbox',
                'https://remote.test/users/alice#main-key', 'key', now() + interval '1 day'
            );
            INSERT INTO remote_status (
                id, activitypub_id, remote_actor_id, content, visibility,
                published_at, updated_at, in_reply_to, object
            ) VALUES (
                '20000000-0000-0000-0000-000000000066',
                'https://remote.test/statuses/66',
                '10000000-0000-0000-0000-000000000066', '', 'public', now(), now(),
                'https://parent.test/statuses/1',
                '{"replies":"https://remote.test/statuses/66/replies"}'::jsonb
            );
            "#,
        )
        .await
        .unwrap();

    Migrator::up(database.connection(), Some(1)).await.unwrap();
    let rows = database
        .connection()
        .query_all(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT kind, payload->>'status_id' AS status_id FROM job ORDER BY kind".to_owned(),
        ))
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0].try_get::<String>("", "kind").unwrap(),
        "federation_replies_fetch"
    );
    assert_eq!(
        rows[1].try_get::<String>("", "kind").unwrap(),
        "federation_thread_resolve"
    );
    assert!(rows.iter().all(|row| {
        row.try_get::<String>("", "status_id").unwrap() == "20000000-0000-0000-0000-000000000066"
    }));

    Migrator::down(database.connection(), Some(1))
        .await
        .unwrap();
    assert!(table_exists(database.connection(), "remote_status").await);
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
    assert!(table_exists(database.connection(), "scheduled_status").await);
    assert!(table_exists(database.connection(), "status_creation_idempotency").await);
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
    assert!(table_exists(database.connection(), "streaming_event").await);
    assert!(table_exists(database.connection(), "push_subscription").await);

    Migrator::down(database.connection(), None).await.unwrap();
    assert!(!table_exists(database.connection(), "job").await);
    assert!(!table_exists(database.connection(), "local_account").await);
    assert!(!table_exists(database.connection(), "oauth_application").await);
    assert!(!table_exists(database.connection(), "local_status").await);
    assert!(!table_exists(database.connection(), "local_status_favourite").await);
    assert!(!table_exists(database.connection(), "local_status_bookmark").await);
    assert!(!table_exists(database.connection(), "local_follow").await);
    assert!(!table_exists(database.connection(), "local_media_attachment").await);
    assert!(!table_exists(database.connection(), "scheduled_status").await);
    assert!(!table_exists(database.connection(), "status_creation_idempotency").await);
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
    assert!(!table_exists(database.connection(), "status_quote").await);
    assert!(!table_exists(database.connection(), "streaming_event").await);
    assert!(!table_exists(database.connection(), "push_subscription").await);
    assert!(!table_exists(database.connection(), "trend_refresh_schedule").await);
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

/// Given the pre-history schema, migration 55 can be applied and rolled back cleanly.
#[test_context(EmbeddedDatabase)]
#[tokio::test]
async fn status_edit_history_upgrade_and_rollback(database: &mut EmbeddedDatabase) {
    Migrator::up(database.connection(), Some(54)).await.unwrap();

    Migrator::up(database.connection(), Some(1)).await.unwrap();
    assert!(table_exists(database.connection(), "local_status_edit").await);
    assert!(table_exists(database.connection(), "local_status_edit_media").await);
    assert!(table_exists(database.connection(), "remote_status_edit").await);
    assert!(table_exists(database.connection(), "remote_status_edit_media").await);

    Migrator::down(database.connection(), Some(1))
        .await
        .unwrap();
    assert!(!table_exists(database.connection(), "local_status_edit").await);
    assert!(!table_exists(database.connection(), "local_status_edit_media").await);
    assert!(!table_exists(database.connection(), "remote_status_edit").await);
    assert!(!table_exists(database.connection(), "remote_status_edit_media").await);
    assert!(table_exists(database.connection(), "local_status").await);
    assert!(table_exists(database.connection(), "remote_status").await);
}

/// Given the pinned-post schema, migration 59 adds and cleanly removes featured hashtags.
#[test_context(EmbeddedDatabase)]
#[tokio::test]
async fn featured_tags_upgrade_and_rollback(database: &mut EmbeddedDatabase) {
    Migrator::up(database.connection(), Some(58)).await.unwrap();

    Migrator::up(database.connection(), Some(1)).await.unwrap();
    assert!(table_exists(database.connection(), "local_featured_tag").await);
    assert!(table_exists(database.connection(), "remote_featured_tag").await);
    assert!(column_exists(database.connection(), "remote_actor", "featured_tags_url").await);

    Migrator::down(database.connection(), Some(1))
        .await
        .unwrap();
    assert!(!table_exists(database.connection(), "local_featured_tag").await);
    assert!(!table_exists(database.connection(), "remote_featured_tag").await);
    assert!(!column_exists(database.connection(), "remote_actor", "featured_tags_url").await);
    assert!(table_exists(database.connection(), "local_status_pin").await);
}

/// Given cached legacy hashtags, migration 56 indexes valid tags and rolls back its join table.
#[test_context(EmbeddedDatabase)]
#[tokio::test]
async fn remote_status_tag_upgrade_backfill_and_rollback(database: &mut EmbeddedDatabase) {
    Migrator::up(database.connection(), Some(55)).await.unwrap();
    database
        .connection()
        .execute_unprepared(
            r##"
            INSERT INTO remote_actor (
                id, activitypub_id, username, domain, inbox_url,
                public_key_id, public_key_pem, expires_at
            ) VALUES (
                '10000000-0000-0000-0000-000000000001',
                'https://remote.test/users/alice', 'alice', 'remote.test',
                'https://remote.test/users/alice/inbox',
                'https://remote.test/users/alice#main-key', 'key', now() + interval '1 day'
            );
            INSERT INTO remote_status (
                id, activitypub_id, remote_actor_id, content, visibility,
                published_at, updated_at, object
            ) VALUES (
                '20000000-0000-0000-0000-000000000001',
                'https://remote.test/statuses/1',
                '10000000-0000-0000-0000-000000000001', '', 'public', now(), now(),
                '{"tag":[
                    {"type":"Hashtag","name":"#Rust"},
                    {"type":"https://www.w3.org/ns/activitystreams#Hashtag","name":"#Fediverse"},
                    {"type":"Hashtag","name":"invalid tag"}
                ]}'::jsonb
            );
            "##,
        )
        .await
        .unwrap();

    Migrator::up(database.connection(), Some(1)).await.unwrap();
    let row = database
        .connection()
        .query_one(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT count(*)::bigint AS count FROM remote_status_tag".to_owned(),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.try_get::<i64>("", "count").unwrap(), 2);

    Migrator::down(database.connection(), Some(1))
        .await
        .unwrap();
    assert!(!table_exists(database.connection(), "remote_status_tag").await);
    assert!(table_exists(database.connection(), "local_tag").await);
}

/// Given statuses created before search support, migration 74 builds local and remote documents.
#[test_context(EmbeddedDatabase)]
#[tokio::test]
async fn status_search_upgrade_backfills_and_rolls_back(database: &mut EmbeddedDatabase) {
    Migrator::up(database.connection(), Some(73)).await.unwrap();
    database
        .connection()
        .execute_unprepared(
            r#"
            INSERT INTO local_account (id, username, email, password_hash)
            VALUES (
                '10000000-0000-0000-0000-000000000074',
                'search_local', 'search-local@example.test', 'hash'
            );
            INSERT INTO local_status (
                id, account_id, content, spoiler_text, visibility, created_at
            ) VALUES (
                '20000000-0000-0000-0000-000000000074',
                '10000000-0000-0000-0000-000000000074',
                'local platypus body', 'local warning', 'public', now()
            );
            INSERT INTO remote_actor (
                id, activitypub_id, username, domain, inbox_url,
                public_key_id, public_key_pem, expires_at
            ) VALUES (
                '30000000-0000-0000-0000-000000000074',
                'https://remote.test/users/search', 'search', 'remote.test',
                'https://remote.test/users/search/inbox',
                'https://remote.test/users/search#main-key', 'key',
                now() + interval '1 day'
            );
            INSERT INTO remote_status (
                id, activitypub_id, remote_actor_id, content, visibility,
                published_at, updated_at, object
            ) VALUES (
                '40000000-0000-0000-0000-000000000074',
                'https://remote.test/statuses/search',
                '30000000-0000-0000-0000-000000000074',
                '<p>remote capybara body</p>', 'public', now(), now(),
                '{"summary":"remote warning"}'::jsonb
            );
            "#,
        )
        .await
        .unwrap();

    Migrator::up(database.connection(), Some(1)).await.unwrap();
    let local = database
        .connection()
        .query_one(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT document FROM status_search_document WHERE local_status_id = '20000000-0000-0000-0000-000000000074'".to_owned(),
        ))
        .await
        .unwrap()
        .unwrap();
    let remote = database
        .connection()
        .query_one(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT document FROM status_search_document WHERE remote_status_id = '40000000-0000-0000-0000-000000000074'".to_owned(),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        local.try_get::<String>("", "document").unwrap(),
        "local warning local platypus body"
    );
    assert_eq!(
        remote.try_get::<String>("", "document").unwrap(),
        "remote warning  remote capybara body "
    );

    Migrator::down(database.connection(), Some(1))
        .await
        .unwrap();
    assert!(!table_exists(database.connection(), "status_search_document").await);
}

struct EmbeddedDatabase {
    postgresql: PostgreSQL,
    connection: DatabaseConnection,
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
            _temp_dir: temp_dir,
        }
    }

    async fn teardown(self) {
        self.connection.close().await.unwrap();
        // The temporary directory removes the isolated cluster after the server has stopped.
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

async fn index_exists(connection: &DatabaseConnection, index_name: &str) -> bool {
    let row = connection
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
            SELECT EXISTS (
                SELECT 1
                  FROM pg_indexes
                 WHERE schemaname = 'public'
                   AND indexname = $1
            ) AS index_exists
            "#,
            vec![index_name.to_owned().into()],
        ))
        .await
        .unwrap()
        .expect("index existence query returned no rows");

    row.try_get::<bool>("", "index_exists").unwrap()
}

async fn trigger_exists(connection: &DatabaseConnection, trigger_name: &str) -> bool {
    let row = connection
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"SELECT EXISTS (
                 SELECT 1 FROM pg_trigger
                 WHERE NOT tgisinternal AND tgname = $1
               ) AS trigger_exists"#,
            vec![trigger_name.to_owned().into()],
        ))
        .await
        .unwrap()
        .expect("trigger existence query returned no rows");
    row.try_get::<bool>("", "trigger_exists").unwrap()
}

async fn function_exists(connection: &DatabaseConnection, function_name: &str) -> bool {
    let row = connection
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"SELECT EXISTS (
                 SELECT 1 FROM pg_proc
                 WHERE pronamespace = 'public'::regnamespace AND proname = $1
               ) AS function_exists"#,
            vec![function_name.to_owned().into()],
        ))
        .await
        .unwrap()
        .expect("function existence query returned no rows");
    row.try_get::<bool>("", "function_exists").unwrap()
}

async fn roosty_trend_routines_exist(connection: &DatabaseConnection) -> bool {
    let row = connection
        .query_one(Statement::from_string(
            DatabaseBackend::Postgres,
            r#"SELECT EXISTS (
                 SELECT 1 FROM pg_proc
                 WHERE pronamespace = 'public'::regnamespace
                   AND (proname LIKE '%trend%' OR proname = 'adjust_tag_daily_usage')
                 UNION ALL
                 SELECT 1 FROM pg_trigger
                 WHERE NOT tgisinternal AND tgname LIKE '%trend%'
               ) AS routines_exist"#
                .to_owned(),
        ))
        .await
        .unwrap()
        .expect("trend routine query returned no rows");
    row.try_get::<bool>("", "routines_exist").unwrap()
}
