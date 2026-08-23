use rsa::{
    RsaPublicKey,
    pkcs1::{DecodeRsaPublicKey, EncodeRsaPublicKey},
    pkcs8::{DecodePublicKey, EncodePublicKey, LineEnding},
};
use sea_orm_migration::{prelude::*, sea_orm::Statement};
use uuid::Uuid;

/// Normalizes local and remote actor keys for FEP-521a and adds shared maintenance state.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        let local = db
            .query_all(Statement::from_string(
                manager.get_database_backend(),
                "SELECT account_id, public_key_pem, private_key_ciphertext, private_key_nonce, created_at FROM local_actor_key".to_owned(),
            ))
            .await?;
        let remote = db
            .query_all(Statement::from_string(
                manager.get_database_backend(),
                "SELECT id, public_key_id, public_key_pem FROM remote_actor".to_owned(),
            ))
            .await?;

        db.execute_unprepared(
            r#"
            CREATE TYPE actor_key_algorithm AS ENUM ('rsa_pkcs1_sha256', 'ed25519');
            ALTER TABLE local_actor_key RENAME TO local_actor_key_legacy;
            CREATE TABLE local_actor_key (
                id uuid PRIMARY KEY,
                account_id uuid NOT NULL REFERENCES local_account(id) ON DELETE CASCADE,
                key_id text NOT NULL UNIQUE,
                algorithm actor_key_algorithm NOT NULL,
                public_key bytea NOT NULL,
                private_key_ciphertext bytea NOT NULL,
                private_key_nonce bytea NOT NULL,
                activated_at timestamptz NOT NULL,
                retiring_at timestamptz,
                expires_at timestamptz,
                created_at timestamptz NOT NULL DEFAULT now(),
                CHECK ((retiring_at IS NULL AND expires_at IS NULL) OR
                       (retiring_at IS NOT NULL AND expires_at IS NOT NULL AND expires_at > retiring_at))
            );
            CREATE UNIQUE INDEX local_actor_key_active_algorithm_idx
                ON local_actor_key(account_id, algorithm) WHERE retiring_at IS NULL;
            CREATE INDEX local_actor_key_account_publish_idx
                ON local_actor_key(account_id, expires_at);

            CREATE TABLE remote_actor_key (
                key_id text PRIMARY KEY,
                remote_actor_id uuid NOT NULL REFERENCES remote_actor(id) ON DELETE CASCADE,
                algorithm actor_key_algorithm NOT NULL,
                public_key bytea NOT NULL,
                expires_at timestamptz,
                created_at timestamptz NOT NULL DEFAULT now()
            );
            CREATE INDEX remote_actor_key_actor_idx ON remote_actor_key(remote_actor_id);

            CREATE TABLE actor_key_maintenance_schedule (
                id smallint PRIMARY KEY CHECK (id = 1),
                next_run_at timestamptz NOT NULL,
                updated_at timestamptz NOT NULL DEFAULT now()
            );
            INSERT INTO actor_key_maintenance_schedule(id, next_run_at) VALUES (1, now());
            ALTER TYPE job_kind ADD VALUE 'actor_key_maintenance';
            "#,
        )
        .await?;

        for row in local {
            let account_id: Uuid = row.try_get("", "account_id")?;
            let pem: String = row.try_get("", "public_key_pem")?;
            let der = RsaPublicKey::from_public_key_pem(&pem)
                .map_err(|error| DbErr::Migration(format!("invalid local RSA key: {error}")))?
                .to_pkcs1_der()
                .map_err(|error| {
                    DbErr::Migration(format!("could not encode local RSA key: {error}"))
                })?;
            db.execute(Statement::from_sql_and_values(
                manager.get_database_backend(),
                r#"INSERT INTO local_actor_key(
                       id, account_id, key_id, algorithm, public_key,
                       private_key_ciphertext, private_key_nonce, activated_at, created_at)
                   VALUES ($1, $2, $3, 'rsa_pkcs1_sha256', $4, $5, $6, $7, $7)"#,
                vec![
                    Uuid::now_v7().into(),
                    account_id.into(),
                    format!("urn:roosty:account:{account_id}#main-key").into(),
                    der.as_bytes().to_vec().into(),
                    row.try_get::<Vec<u8>>("", "private_key_ciphertext")?.into(),
                    row.try_get::<Vec<u8>>("", "private_key_nonce")?.into(),
                    row.try_get::<time::OffsetDateTime>("", "created_at")?
                        .into(),
                ],
            ))
            .await?;
        }
        for row in remote {
            let pem: String = row.try_get("", "public_key_pem")?;
            // A corrupt legacy cache entry must not block an instance upgrade; it will be
            // refreshed before a future signature can use it.
            let Ok(public_key) = RsaPublicKey::from_public_key_pem(&pem) else {
                continue;
            };
            let der = public_key.to_pkcs1_der().map_err(|error| {
                DbErr::Migration(format!("could not encode remote RSA key: {error}"))
            })?;
            db.execute(Statement::from_sql_and_values(
                manager.get_database_backend(),
                "INSERT INTO remote_actor_key(key_id, remote_actor_id, algorithm, public_key) VALUES ($1, $2, 'rsa_pkcs1_sha256', $3)",
                vec![
                    row.try_get::<String>("", "public_key_id")?.into(),
                    row.try_get::<Uuid>("", "id")?.into(),
                    der.as_bytes().to_vec().into(),
                ],
            ))
            .await?;
        }
        db.execute_unprepared("DROP TABLE local_actor_key_legacy")
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        let rows = db
            .query_all(Statement::from_string(
                manager.get_database_backend(),
                r#"SELECT DISTINCT ON (account_id) account_id, public_key,
                       private_key_ciphertext, private_key_nonce, created_at
                   FROM local_actor_key
                   WHERE algorithm = 'rsa_pkcs1_sha256'
                   ORDER BY account_id, (retiring_at IS NULL) DESC, activated_at DESC"#
                    .to_owned(),
            ))
            .await?;
        db.execute_unprepared(
            r#"
            CREATE TABLE local_actor_key_legacy (
                account_id uuid PRIMARY KEY REFERENCES local_account(id) ON DELETE CASCADE,
                public_key_pem text NOT NULL,
                private_key_ciphertext bytea NOT NULL,
                private_key_nonce bytea NOT NULL,
                created_at timestamptz NOT NULL DEFAULT now()
            );
            "#,
        )
        .await?;
        for row in rows {
            let der: Vec<u8> = row.try_get("", "public_key")?;
            let key = RsaPublicKey::from_pkcs1_der(&der)
                .map_err(|error| DbErr::Migration(format!("could not restore RSA key: {error}")))?;
            let pem = key
                .to_public_key_pem(LineEnding::LF)
                .map_err(|error| DbErr::Migration(format!("could not restore RSA key: {error}")))?;
            db.execute(Statement::from_sql_and_values(
                manager.get_database_backend(),
                "INSERT INTO local_actor_key_legacy VALUES ($1, $2, $3, $4, $5)",
                vec![
                    row.try_get::<Uuid>("", "account_id")?.into(),
                    pem.into(),
                    row.try_get::<Vec<u8>>("", "private_key_ciphertext")?.into(),
                    row.try_get::<Vec<u8>>("", "private_key_nonce")?.into(),
                    row.try_get::<time::OffsetDateTime>("", "created_at")?
                        .into(),
                ],
            ))
            .await?;
        }
        db.execute_unprepared(
            r#"
            DROP TABLE actor_key_maintenance_schedule;
            DROP TABLE remote_actor_key;
            DROP TABLE local_actor_key;
            ALTER TABLE local_actor_key_legacy RENAME TO local_actor_key;
            ALTER TABLE job ALTER COLUMN kind TYPE text USING kind::text;
            CREATE TYPE job_kind_old AS ENUM (
                'federation_follow_response','federation_status_delivery','federation_quote_delivery',
                'federation_follow_delivery','federation_favourite_delivery','federation_reblog_delivery',
                'federation_actor_update_delivery','federation_moderation_delivery','federation_remote_media_fetch',
                'federation_featured_refresh','federation_featured_tags_refresh','federation_thread_resolve',
                'federation_replies_fetch','federation_reply_fetch','web_push_delivery',
                'notification_request_merge','notification_request_cleanup','account_purge',
                'domain_moderation_reconcile','scheduled_status_publish','poll_expiration','poll_update',
                'federation_poll_vote_delivery','trend_maintenance','account_suggestion_maintenance',
                'preview_card_fetch','preview_card_backfill');
            DROP TYPE job_kind;
            ALTER TYPE job_kind_old RENAME TO job_kind;
            ALTER TABLE job ALTER COLUMN kind TYPE job_kind USING kind::job_kind;
            DROP TYPE actor_key_algorithm;
            "#,
        )
        .await?;
        Ok(())
    }
}
