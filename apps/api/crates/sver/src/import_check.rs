//! Rehearse account import, or explicitly check/apply it to the separate sver.tv database.
//! `profiles` runs the legacy profile import (docs/PROFILES.md, "Legacy profile import").
use aes_gcm::{
    AesGcm, KeyInit,
    aead::{Aead, consts::U16},
    aes::Aes256,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use std::{collections::HashMap, path::PathBuf};
use sver::{App, Config, profile_import, security as sec};

#[derive(Deserialize)]
struct Snapshot {
    users: Vec<Value>,
    profiles: Vec<Value>,
    identities: Vec<Value>,
}

#[derive(Default, Serialize)]
struct Report {
    users: usize,
    profiles: usize,
    identities: usize,
    active_identities: usize,
    preserved_totp: usize,
    preserved_recovery_codes: usize,
    held_deletions: usize,
    mfa_exceptions: Vec<Value>,
}

fn legacy_secret(value: &str, key: &[u8]) -> Result<String, String> {
    let parts = value.split(':').collect::<Vec<_>>();
    if parts.len() != 3 {
        return Err("Invalid legacy TOTP format".into());
    }
    let parts = parts
        .iter()
        .map(|s| STANDARD.decode(s))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "Invalid legacy TOTP encoding")?;
    if parts[0].len() != 16 || parts[1].len() != 16 {
        return Err("Invalid legacy TOTP nonce/tag".into());
    }
    let ciphertext = [parts[2].as_slice(), parts[1].as_slice()].concat();
    let cipher =
        AesGcm::<Aes256, U16>::new_from_slice(key).map_err(|_| "Invalid legacy encryption key")?;
    String::from_utf8(
        cipher
            .decrypt(
                &aes_gcm::Nonce::<U16>::try_from(parts[0].as_slice())
                    .map_err(|_| "Invalid legacy TOTP nonce")?,
                ciphertext.as_slice(),
            )
            .map_err(|_| "Legacy TOTP decryption failed; no account was changed")?,
    )
    .map_err(|_| "Invalid legacy TOTP plaintext".into())
}

async fn import(
    app: &App,
    snapshot: &Snapshot,
    key: &[u8],
    commit: bool,
) -> Result<Report, String> {
    let mut report = Report {
        users: snapshot.users.len(),
        profiles: snapshot.profiles.len(),
        identities: snapshot.identities.len(),
        ..Default::default()
    };
    if snapshot.users.is_empty() {
        return Err("Refusing an empty account snapshot".into());
    }
    let profiles = snapshot
        .profiles
        .iter()
        .map(|p| {
            p["userId"]
                .as_str()
                .map(|id| (id, p))
                .ok_or("Profile owner missing")
        })
        .collect::<Result<HashMap<_, _>, _>>()?;
    if profiles.len() != snapshot.profiles.len() {
        return Err("Duplicate legacy profile owners".into());
    }
    let mut tx = app
        .db
        .begin()
        .await
        .map_err(|_| "Import transaction failed")?;
    sqlx::query("SET LOCAL lock_timeout='10s'")
        .execute(&mut *tx)
        .await
        .map_err(|_| "Could not set import lock timeout")?;
    sqlx::query("LOCK TABLE users, identities, legacy_account_data, recovery_codes IN SHARE ROW EXCLUSIVE MODE")
        .execute(&mut *tx).await.map_err(|_| "Could not lock import tables; retry when writes settle")?;
    let already_imported: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM legacy_account_data)")
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| "Import history check failed")?;
    if already_imported {
        return Err("Legacy accounts already imported; refusing to overwrite them".into());
    }
    let before: Value = sqlx::query_scalar(
        "SELECT coalesce(jsonb_agg(to_jsonb(u) ORDER BY id),'[]'::jsonb) FROM users u",
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(|_| "Existing-account preservation check failed")?;
    let conflicts: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users u CROSS JOIN jsonb_array_elements($1) s WHERE u.id=s->>'id' OR lower(u.email)=lower(s->>'email') OR lower(u.username)=lower(s->>'username') OR lower(u.email)=lower(s->>'username') OR lower(u.username)=lower(s->>'email'))")
        .bind(json!(snapshot.users)).fetch_one(&mut *tx).await.map_err(|_| "Account conflict check failed")?;
    let identity_conflicts: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM identities i CROSS JOIN jsonb_array_elements($1) s WHERE i.provider=lower(s->>'provider') AND i.subject=s->>'providerUserId')")
        .bind(json!(snapshot.identities)).fetch_one(&mut *tx).await.map_err(|_| "Identity conflict check failed")?;
    if conflicts || identity_conflicts {
        return Err("Existing-account or identity conflict; no accounts changed".into());
    }
    sqlx::query("INSERT INTO users(id,email,username,password_hash,date_of_birth,email_verified,created_at,deleted_at,legacy_deletion_hold) SELECT value->>'id',value->>'email',value->>'username',value->>'passwordHash',(value->>'dateOfBirth')::timestamp::date,(value->>'emailVerified')::boolean,(value->>'createdAt')::timestamptz,(value->>'deletedAt')::timestamptz,value->>'deletedAt' IS NOT NULL FROM jsonb_array_elements($1)")
        .bind(json!(snapshot.users)).execute(&mut *tx).await.map_err(|_| "Account mapping failed; import rolled back")?;
    for account in &snapshot.users {
        let id = account["id"].as_str().ok_or("Account ID missing")?;
        let email = account["email"].as_str().ok_or("Account email missing")?;
        let mut exception = None;
        if account["twoFactorEnabled"] == true {
            if account["twoFactorSmsEnabled"] == true {
                exception = Some("legacy_sms_mfa_not_supported");
            } else if let Some(encrypted) = account["twoFactorSecretEncrypted"]
                .as_str()
                .filter(|s| !s.is_empty())
            {
                let secret = legacy_secret(encrypted, key)?;
                sec::totp(&secret, email).map_err(|_| "Legacy TOTP parameters are invalid")?;
                let purpose = format!("totp:{id}");
                let sealed =
                    sec::seal(app, &purpose, &secret).map_err(|_| "TOTP re-encryption failed")?;
                if sec::unseal(app, &purpose, &sealed).map_err(|_| "TOTP round-trip failed")?
                    != secret
                {
                    return Err("TOTP preservation comparison failed".into());
                }
                sqlx::query("UPDATE users SET mfa_enabled=true,mfa_secret=$2 WHERE id=$1")
                    .bind(id)
                    .bind(sealed)
                    .execute(&mut *tx)
                    .await
                    .map_err(|_| "TOTP import failed")?;
                let stored: String = sqlx::query_scalar("SELECT mfa_secret FROM users WHERE id=$1")
                    .bind(id)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(|_| "Stored TOTP read failed")?;
                if sec::unseal(app, &purpose, &stored)
                    .map_err(|_| "Stored TOTP decryption failed")?
                    != secret
                {
                    return Err("Stored TOTP preservation comparison failed".into());
                }
                report.preserved_totp += 1;
                if let Some(codes) = account["twoFactorBackupCodesHashed"].as_str() {
                    let hashes: Vec<String> = serde_json::from_str(codes)
                        .map_err(|_| "Invalid legacy recovery-code list")?;
                    for hash in hashes {
                        if !hash.starts_with("$2") {
                            return Err(
                                "Unsupported legacy recovery-code hash; import rolled back".into(),
                            );
                        }
                        sqlx::query(
                            "INSERT INTO recovery_codes(id,user_id,code_hash) VALUES($1,$2,$3)",
                        )
                        .bind(uuid::Uuid::new_v4().to_string())
                        .bind(id)
                        .bind(hash)
                        .execute(&mut *tx)
                        .await
                        .map_err(|_| "Recovery-code import failed")?;
                        report.preserved_recovery_codes += 1;
                    }
                }
            } else {
                exception = Some("legacy_mfa_enabled_without_totp_secret");
            }
        }
        if let Some(reason) = exception {
            report
                .mfa_exceptions
                .push(json!({"user_id":id,"reason":reason}));
        }
        if !account["deletedAt"].is_null() {
            report.held_deletions += 1;
        }
        let identities: Vec<&Value> = snapshot
            .identities
            .iter()
            .filter(|identity| identity["userId"] == id)
            .collect();
        sqlx::query("INSERT INTO legacy_account_data(user_id,account,profile,mfa_exception,identities) VALUES($1,$2,$3,$4,$5)")
            .bind(id).bind(account).bind(profiles.get(id).copied()).bind(exception).bind(json!(identities))
            .execute(&mut *tx).await.map_err(|_| "Legacy metadata preservation failed")?;
    }
    // Preserve deferred providers such as Kick without enabling their sign-in adapters.
    report.active_identities = sqlx::query("INSERT INTO identities(provider,subject,user_id) SELECT lower(value->>'provider'),value->>'providerUserId',value->>'userId' FROM jsonb_array_elements($1) WHERE lower(value->>'provider') IN ('google','twitch','discord')")
        .bind(json!(snapshot.identities)).execute(&mut *tx).await.map_err(|_| "Linked identity import failed; import rolled back")?.rows_affected() as usize;
    let same: bool = sqlx::query_scalar("SELECT bool_and(u.id=d.account->>'id' AND u.email=d.account->>'email' AND u.username=d.account->>'username' AND u.password_hash IS NOT DISTINCT FROM d.account->>'passwordHash' AND u.email_verified=(d.account->>'emailVerified')::boolean AND u.date_of_birth IS NOT DISTINCT FROM (d.account->>'dateOfBirth')::timestamp::date AND u.created_at=(d.account->>'createdAt')::timestamptz AND u.deleted_at IS NOT DISTINCT FROM (d.account->>'deletedAt')::timestamptz AND u.legacy_deletion_hold=(d.account->>'deletedAt' IS NOT NULL)) FROM users u JOIN legacy_account_data d ON d.user_id=u.id")
        .fetch_one(&mut *tx).await.map_err(|_| "Account preservation check failed")?;
    let stored_profiles: i64 =
        sqlx::query_scalar("SELECT count(*) FROM legacy_account_data WHERE profile IS NOT NULL")
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| "Profile count check failed")?;
    let stored_identities: i64 = sqlx::query_scalar(
        "SELECT coalesce(sum(jsonb_array_length(identities)),0)::bigint FROM legacy_account_data",
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(|_| "Preserved identity count check failed")?;
    if !same
        || stored_profiles as usize != report.profiles
        || stored_identities as usize != report.identities
    {
        return Err("Preservation comparison failed; import rolled back".into());
    }
    let unchanged: Value = sqlx::query_scalar("SELECT coalesce(jsonb_agg(to_jsonb(u) ORDER BY id),'[]'::jsonb) FROM users u WHERE NOT EXISTS(SELECT 1 FROM legacy_account_data d WHERE d.user_id=u.id)")
        .fetch_one(&mut *tx).await.map_err(|_| "Existing-account comparison failed")?;
    let stored_accounts: i64 = sqlx::query_scalar("SELECT count(*) FROM legacy_account_data")
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| "Imported-account count failed")?;
    if unchanged != before || stored_accounts as usize != report.users {
        return Err(
            "Account count or existing-account comparison failed; import rolled back".into(),
        );
    }
    for account in &snapshot.users {
        let id = account["id"].as_str().ok_or("Account ID missing")?;
        let (stored_account, stored_profile, stored_identities): (Value, Option<Value>, Value) =
            sqlx::query_as(
                "SELECT account,profile,identities FROM legacy_account_data WHERE user_id=$1",
            )
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| "Metadata comparison read failed")?;
        let identities: Vec<&Value> = snapshot
            .identities
            .iter()
            .filter(|i| i["userId"] == id)
            .collect();
        if &stored_account != account
            || stored_profile.as_ref() != profiles.get(id).copied()
            || stored_identities != json!(identities)
        {
            return Err("Full metadata comparison failed; import rolled back".into());
        }
    }
    let supported: i64 = sqlx::query_scalar("SELECT count(*) FROM identities i JOIN legacy_account_data d ON d.user_id=i.user_id WHERE EXISTS(SELECT 1 FROM jsonb_array_elements(d.identities) s WHERE lower(s->>'provider')=i.provider AND s->>'providerUserId'=i.subject)")
        .fetch_one(&mut *tx).await.map_err(|_| "Active-identity comparison failed")?;
    let recovery: i64 = sqlx::query_scalar("SELECT count(*) FROM recovery_codes r JOIN legacy_account_data d ON d.user_id=r.user_id WHERE r.code_hash IN (SELECT jsonb_array_elements_text((d.account->>'twoFactorBackupCodesHashed')::jsonb))")
        .fetch_one(&mut *tx).await.map_err(|_| "Recovery-code comparison failed")?;
    if supported as usize != report.active_identities
        || recovery as usize != report.preserved_recovery_codes
    {
        return Err("Identity or recovery-code comparison failed; import rolled back".into());
    }
    if commit {
        tx.commit()
            .await
            .map_err(|_| "Import commit failed; inspect target before retrying")?;
    } else {
        tx.rollback()
            .await
            .map_err(|_| "Import dry-run rollback failed")?;
    }
    // No mail/jobs/server is started; expired legacy accounts are held for explicit review.
    Ok(report)
}

#[tokio::main]
async fn main() -> Result<(), String> {
    let config = Config::from_env()?;
    let database = std::env::var("DATABASE_URL").map_err(|_| "DATABASE_URL required")?;
    let url = url::Url::parse(&database).map_err(|_| "Invalid database URL")?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("profiles") {
        return profiles(config, &database, &url, &args[1..]).await;
    }
    if args.first().map(String::as_str) == Some("factions") {
        return factions(config, &database, &url, &args[1..]).await;
    }
    let mode = args.get(1).map(String::as_str).unwrap_or("rehearsal");
    if args.is_empty()
        || args.len() > 2
        || !matches!(mode, "rehearsal" | "--check-live" | "--apply-live")
    {
        return Err(
            "Usage: sver-import-check EXTERNAL_SNAPSHOT [--check-live|--apply-live]".into(),
        );
    }
    let live = mode != "rehearsal";
    if !matches!(url.host_str(), Some("localhost" | "127.0.0.1"))
        || if live {
            !config.production
                || config.origin != "https://sver.tv"
                || url.path() != "/sver_stage"
                || url.username() != "sver_stage"
                || url.port() != Some(15432)
        } else {
            config.production || url.path() != "/sver_rebuild"
        }
    {
        return Err("Target refused: rehearsal requires local development sver_rebuild; live import requires the loopback sver.tv sver_stage database on port 15432".into());
    }
    let workspace = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../.."))
        .canonicalize()
        .map_err(|_| "Workspace unavailable")?;
    let path = PathBuf::from(&args[0])
        .canonicalize()
        .map_err(|_| "Snapshot unavailable")?;
    if path.starts_with(workspace) {
        return Err("Account snapshots must remain outside the workspace".into());
    }
    let snapshot: Snapshot =
        serde_json::from_slice(&std::fs::read(&path).map_err(|_| "Snapshot unreadable")?)
            .map_err(|_| "Invalid snapshot JSON")?;
    let legacy = std::env::var("LEGACY_TWO_FACTOR_ENCRYPTION_KEY")
        .map_err(|_| "Legacy TOTP key required")?;
    let key = if legacy.len() == 32 {
        legacy.into_bytes()
    } else {
        STANDARD
            .decode(legacy)
            .map_err(|_| "Invalid legacy key encoding")?
    };
    if key.len() != 32 {
        return Err("Legacy TOTP key must contain 32 bytes".into());
    }
    let admin = PgPoolOptions::new()
        .max_connections(2)
        .after_connect(|connection, _| {
            Box::pin(async move {
                sqlx::query("SET search_path TO public")
                    .execute(connection)
                    .await?;
                Ok(())
            })
        })
        .connect(&database)
        .await
        .map_err(|_| "Could not connect to import target")?;
    if live {
        let safe: bool = sqlx::query_scalar("SELECT current_database()='sver_stage' AND to_regclass('public.\"User\"') IS NULL AND to_regclass('public._prisma_migrations') IS NULL AND to_regclass('public.legacy_account_data') IS NOT NULL")
            .fetch_one(&admin).await.map_err(|_| "Live database safety check failed")?;
        if !safe {
            return Err("Refusing an unexpected live database schema".into());
        }
        let report_path = path.with_extension(if mode == "--apply-live" {
            "live-import-report.json"
        } else {
            "live-check-report.json"
        });
        let mut report_file = std::fs::OpenOptions::new().write(true).create_new(true).open(report_path)
            .map_err(|_| "Private report already exists or cannot be created; inspect prior run before retrying")?;
        let app = App::new(admin.clone(), config)
            .await
            .map_err(|_| "Could not initialize importer")?;
        let report = import(&app, &snapshot, &key, mode == "--apply-live").await?;
        serde_json::to_writer_pretty(&mut report_file, &report).map_err(
            |_| "Import finished but private report write failed; inspect database before retrying",
        )?;
        report_file
            .sync_all()
            .map_err(|_| "Import finished but private report sync failed")?;
        println!(
            "Live import {}: {} accounts, {} profiles, {} identities ({} active), {} TOTP setups, {} recovery codes, {} held deletions, {} documented MFA exceptions.",
            if mode == "--apply-live" {
                "COMMITTED"
            } else {
                "CHECKED AND ROLLED BACK"
            },
            report.users,
            report.profiles,
            report.identities,
            report.active_identities,
            report.preserved_totp,
            report.preserved_recovery_codes,
            report.held_deletions,
            report.mfa_exceptions.len()
        );
        return Ok(());
    }
    let schema = format!("login_import_{}", uuid::Uuid::new_v4().simple());
    // The schema identifier is a fixed prefix plus a locally generated simple UUID.
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
        .execute(&admin)
        .await
        .map_err(|_| "Could not create rehearsal schema")?;
    let search_path = format!("SET search_path TO {schema}");
    let result = async {
        let db = PgPoolOptions::new()
            .max_connections(2)
            .after_connect(move |connection, _| {
                let statement = search_path.clone();
                Box::pin(async move {
                    sqlx::query(sqlx::AssertSqlSafe(statement))
                        .execute(connection)
                        .await?;
                    Ok(())
                })
            })
            .connect(&database)
            .await
            .map_err(|_| "Could not connect to rehearsal schema")?;
        let result = async {
            sqlx::migrate!("../../../../migrations")
                .run(&db)
                .await
                .map_err(|_| "Rehearsal schema migration failed")?;
            let app = App::new(db.clone(), config)
                .await
                .map_err(|_| "Could not initialize rehearsal")?;
            import(&app, &snapshot, &key, true).await
        }
        .await;
        db.close().await;
        result
    }
    .await;
    // Only the generated identifier above is interpolated; no account data enters SQL text.
    // The schema identifier is a fixed prefix plus a locally generated simple UUID.
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .execute(&admin)
        .await
        .map_err(|_| "Rehearsal cleanup failed")?;
    let report = result?;
    let report_path = path.with_extension("import-report.json");
    std::fs::write(
        &report_path,
        serde_json::to_vec_pretty(&report).map_err(|_| "Could not encode private report")?,
    )
    .map_err(|_| "Could not save private import report")?;
    println!(
        "Import rehearsal passed: {} accounts, {} profiles, {} preserved identities ({} supported), {} preserved TOTP setups, {} recovery codes, {} held deletions, {} documented MFA exceptions. Temporary schema removed.",
        report.users,
        report.profiles,
        report.identities,
        report.active_identities,
        report.preserved_totp,
        report.preserved_recovery_codes,
        report.held_deletions,
        report.mfa_exceptions.len()
    );
    Ok(())
}

fn outside_workspace(path: &str, what: &'static str) -> Result<PathBuf, String> {
    let path = PathBuf::from(path)
        .canonicalize()
        .map_err(|_| format!("{what} unavailable"))?;
    // A release image has no source workspace; nothing can be inside one that doesn't exist.
    if let Ok(workspace) =
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../..")).canonicalize()
        && path.starts_with(workspace)
    {
        return Err(format!("{what} must remain outside the workspace"));
    }
    Ok(path)
}

/// Import preserved account selections using a mapping exported from the restored legacy Faction table.
async fn factions(
    config: Config,
    database: &str,
    url: &url::Url,
    args: &[String],
) -> Result<(), String> {
    if args.len() != 2
        || !matches!(
            args[1].as_str(),
            "--check" | "--apply" | "--check-live" | "--apply-live"
        )
    {
        return Err("Usage: sver-import-check factions EXTERNAL_ID_TO_SLUG_JSON --check|--apply|--check-live|--apply-live".into());
    }
    let live = args[1].ends_with("-live");
    if !matches!(url.host_str(), Some("localhost" | "127.0.0.1"))
        || if live {
            !config.production
                || config.origin != "https://sver.tv"
                || url.path() != "/sver_stage"
                || url.username() != "sver_stage"
                || url.port() != Some(15432)
        } else {
            config.production || url.path() != "/sver_rebuild"
        }
    {
        return Err("Target refused: use the loopback development database or the dedicated live rebuild database.".into());
    }
    let path = outside_workspace(&args[0], "Legacy faction mapping")?;
    let map: HashMap<String, String> =
        serde_json::from_slice(&std::fs::read(path).map_err(|_| "Mapping unreadable")?)
            .map_err(|_| "Invalid faction mapping JSON")?;
    if map.is_empty() || map.values().any(|slug| !sver::factions::valid(slug)) {
        return Err("Mapping must contain only known faction slugs.".into());
    }
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(database)
        .await
        .map_err(|_| "Import database unavailable")?;
    let mut tx = pool
        .begin()
        .await
        .map_err(|_| "Import transaction failed")?;
    sqlx::query("SET LOCAL lock_timeout='10s'")
        .execute(&mut *tx)
        .await
        .map_err(|_| "Could not set lock timeout")?;
    let rows = sver::auth::legacy_factions(&mut tx)
        .await
        .map_err(|_| "Preserved membership read failed")?;
    if rows.iter().any(|(_, id, _)| !map.contains_key(id)) {
        return Err("A preserved faction ID is missing from the mapping; nothing changed.".into());
    }
    let mut imported = 0;
    for (user, id, chosen) in &rows {
        if sver::factions::import_membership(&mut tx, user, &map[id], *chosen)
            .await
            .map_err(|_| "Membership import failed; rolled back")?
        {
            imported += 1;
        }
    }
    let apply = args[1].starts_with("--apply");
    if apply {
        tx.commit().await.map_err(|_| "Import commit failed")?;
    } else {
        tx.rollback()
            .await
            .map_err(|_| "Import check rollback failed")?;
    }
    println!(
        "Faction import {}: {} preserved selections; {} new memberships; {} existing choices kept.",
        if apply {
            "applied"
        } else {
            "checked and rolled back"
        },
        rows.len(),
        imported,
        rows.len() - imported
    );
    pool.close().await;
    Ok(())
}

fn print_counts(title: &str, counts: &profile_import::Counts) {
    println!("{title}");
    for (key, value) in counts {
        println!("  {key}: {value}");
    }
}

fn private_report(outcome: &profile_import::Outcome) -> Value {
    json!({"counts": outcome.counts, "dropped": outcome.dropped})
}

/// `sver-import-check profiles EXPORT_JSON MEDIA_DIR ACCOUNTS_JSON [FLAGS]` (rehearsal) or
/// `sver-import-check profiles EXPORT_JSON MEDIA_DIR --check-live|--apply-live [FLAGS]`.
/// Flags: `--named-internal-only` (any mode), `--preview-extra-internal` (rehearsal only).
async fn profiles(
    mut config: Config,
    database: &str,
    url: &url::Url,
    args: &[String],
) -> Result<(), String> {
    let usage = "Usage: sver-import-check profiles EXPORT_JSON MEDIA_DIR (ACCOUNTS_JSON [--preview-extra-internal] | --check-live | --apply-live) [--named-internal-only]";
    if args.len() < 3 {
        return Err(usage.into());
    }
    let mode = args[2].as_str();
    let live = matches!(mode, "--check-live" | "--apply-live");
    if mode.starts_with("--") && !live {
        return Err(usage.into());
    }
    let flags = &args[3..];
    let preview = flags.iter().any(|f| f == "--preview-extra-internal");
    let named_internal_only = flags.iter().any(|f| f == "--named-internal-only");
    let mut seen = std::collections::HashSet::new();
    if flags.iter().any(|f| {
        !seen.insert(f.as_str())
            || !matches!(
                f.as_str(),
                "--preview-extra-internal" | "--named-internal-only"
            )
    }) || (preview && (live || named_internal_only))
    {
        return Err(usage.into());
    }
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1"));
    if !loopback
        || if live {
            !config.production
                || config.origin != "https://sver.tv"
                || url.path() != "/sver_stage"
                || url.username() != "sver_stage"
                || url.port() != Some(15432)
        } else {
            config.production || url.path() != "/sver_rebuild"
        }
    {
        return Err("Target refused: rehearsal requires local development sver_rebuild; live import requires the loopback sver.tv sver_stage database on port 15432".into());
    }
    let export_path = outside_workspace(&args[0], "Profile export")?;
    let media_dir = outside_workspace(&args[1], "Media snapshot")?;
    let export: profile_import::Export = serde_json::from_slice(
        &std::fs::read(&export_path).map_err(|_| "Profile export unreadable")?,
    )
    .map_err(|_| "Invalid profile export JSON")?;
    let media_files = profile_import::load_media(&media_dir)?;
    let admin = PgPoolOptions::new()
        .max_connections(2)
        .after_connect(|connection, _| {
            Box::pin(async move {
                sqlx::query("SET search_path TO public")
                    .execute(connection)
                    .await?;
                Ok(())
            })
        })
        .connect(database)
        .await
        .map_err(|_| "Could not connect to import target")?;
    if live {
        // Live modes never migrate: 0004 must already be applied by the release.
        let safe: bool = sqlx::query_scalar("SELECT current_database()='sver_stage' AND to_regclass('public.\"User\"') IS NULL AND to_regclass('public.legacy_account_data') IS NOT NULL AND to_regclass('public.import_runs') IS NOT NULL AND EXISTS(SELECT 1 FROM _sqlx_migrations WHERE version=4 AND success)")
            .fetch_one(&admin).await.map_err(|_| "Live database safety check failed")?;
        if !safe {
            return Err(
                "Refusing a live database without migration 0004 or with an unexpected schema"
                    .into(),
            );
        }
        if !config.media.storage.available() {
            return Err("Media storage (bucket or production filesystem) is not configured; refusing the live import".into());
        }
        let apply = mode == "--apply-live";
        let report_path = export_path.with_extension(if apply {
            "profiles-live-import-report.json"
        } else {
            "profiles-live-check-report.json"
        });
        let mut report_file = std::fs::OpenOptions::new().write(true).create_new(true).open(report_path)
            .map_err(|_| "Private report already exists or cannot be created; inspect the prior run before retrying")?;
        let app = App::new(admin.clone(), config)
            .await
            .map_err(|_| "Could not initialize importer")?;
        let mut uploaded = Vec::new();
        let options = profile_import::Options {
            commit: apply,
            named_internal_only,
            ..Default::default()
        };
        let result =
            profile_import::run(&app, &export, &media_files, &options, &mut uploaded).await;
        if !apply || result.is_err() {
            // Check mode removes what it uploaded; a failed apply queues it for the media job.
            let mut leftover = Vec::new();
            for key in &uploaded {
                if apply
                    || app
                        .config
                        .media
                        .storage
                        .delete(&app.http, key)
                        .await
                        .is_err()
                {
                    leftover.push(key.clone());
                }
            }
            profile_import::queue_orphans(&app, &leftover).await?;
        }
        let outcome = result?;
        serde_json::to_writer_pretty(&mut report_file, &private_report(&outcome)).map_err(
            |_| "Import finished but the private report write failed; inspect the database before retrying",
        )?;
        report_file
            .sync_all()
            .map_err(|_| "Import finished but the private report sync failed")?;
        if named_internal_only {
            println!(
                "Operator decision applied: only admin, support and SVER are internal; system-flagged accounts imported as public."
            );
        }
        print_counts(
            if apply {
                "Live profile import COMMITTED:"
            } else {
                "Live profile import CHECKED AND ROLLED BACK:"
            },
            &outcome.counts,
        );
        return Ok(());
    }
    // Rehearsal: generated schema, the existing account-import path, then the profile import
    // with a temporary filesystem media store. Everything is removed afterwards.
    let accounts_path = outside_workspace(&args[2], "Account snapshot")?;
    let snapshot: Snapshot = serde_json::from_slice(
        &std::fs::read(&accounts_path).map_err(|_| "Account snapshot unreadable")?,
    )
    .map_err(|_| "Invalid account snapshot JSON")?;
    let legacy = std::env::var("LEGACY_TWO_FACTOR_ENCRYPTION_KEY")
        .map_err(|_| "Legacy TOTP key required")?;
    let key = if legacy.len() == 32 {
        legacy.into_bytes()
    } else {
        STANDARD
            .decode(legacy)
            .map_err(|_| "Invalid legacy key encoding")?
    };
    if key.len() != 32 {
        return Err("Legacy TOTP key must contain 32 bytes".into());
    }
    let media_store = std::env::temp_dir().join(format!(
        "sver-profile-rehearsal-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&media_store).map_err(|_| "Could not create rehearsal media store")?;
    config.media.storage = sver::media::Storage::Filesystem(media_store.clone());
    let schema = format!("profile_import_{}", uuid::Uuid::new_v4().simple());
    // The schema identifier is a fixed prefix plus a locally generated simple UUID.
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
        .execute(&admin)
        .await
        .map_err(|_| "Could not create rehearsal schema")?;
    let search_path = format!("SET search_path TO {schema}");
    let result = async {
        let db = PgPoolOptions::new()
            .max_connections(2)
            .after_connect(move |connection, _| {
                let statement = search_path.clone();
                Box::pin(async move {
                    sqlx::query(sqlx::AssertSqlSafe(statement))
                        .execute(connection)
                        .await?;
                    Ok(())
                })
            })
            .connect(database)
            .await
            .map_err(|_| "Could not connect to rehearsal schema")?;
        let result = async {
            sqlx::migrate!("../../../../migrations")
                .run(&db)
                .await
                .map_err(|_| "Rehearsal schema migration failed")?;
            let app = App::new(db.clone(), config)
                .await
                .map_err(|_| "Could not initialize rehearsal")?;
            let accounts = import(&app, &snapshot, &key, true).await?;
            let mut uploaded = Vec::new();
            let options = profile_import::Options {
                commit: true,
                preview_extra_internal: preview,
                named_internal_only,
                ..Default::default()
            };
            let outcome =
                profile_import::run(&app, &export, &media_files, &options, &mut uploaded).await?;
            if profile_import::run(&app, &export, &media_files, &options, &mut uploaded)
                .await
                .is_ok()
            {
                return Err("A second profile import was not refused".to_string());
            }
            Ok::<_, String>((accounts.users, outcome))
        }
        .await;
        db.close().await;
        result
    }
    .await;
    // Only the generated identifier above is interpolated; no imported data enters SQL text.
    // The schema identifier is a fixed prefix plus a locally generated simple UUID.
    let dropped = sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .execute(&admin)
        .await;
    let removed = std::fs::remove_dir_all(&media_store);
    dropped.map_err(|_| "Rehearsal cleanup failed")?;
    removed.map_err(|_| "Rehearsal media cleanup failed")?;
    let (accounts, outcome) = result?;
    std::fs::write(
        export_path.with_extension("profiles-rehearsal-report.json"),
        serde_json::to_vec_pretty(&private_report(&outcome))
            .map_err(|_| "Could not encode private report")?,
    )
    .map_err(|_| "Could not save private import report")?;
    if preview {
        println!(
            "PREVIEW ONLY: the internal-account stop was bypassed for this rehearsal; the live import still stops."
        );
    }
    print_counts(
        &format!(
            "Profile import rehearsal passed ({accounts} accounts imported first; second run refused; temporary schema and media removed):"
        ),
        &outcome.counts,
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn import_is_atomic_and_preserves_existing_accounts() {
        let config = Config::from_env().expect("Load external development environment");
        let database = std::env::var("DATABASE_URL").unwrap();
        let url = url::Url::parse(&database).unwrap();
        assert!(!config.production && url.path() == "/sver_rebuild");
        assert!(matches!(url.host_str(), Some("localhost" | "127.0.0.1")));
        let admin = PgPoolOptions::new().connect(&database).await.unwrap();
        let schema = format!("login_import_{}", uuid::Uuid::new_v4().simple());
        // The schema identifier is a fixed prefix plus a locally generated simple UUID.
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .execute(&admin)
            .await
            .unwrap();
        let search_path = format!("SET search_path TO {schema}");
        let db = PgPoolOptions::new()
            .after_connect(move |connection, _| {
                let sql = search_path.clone();
                Box::pin(async move {
                    sqlx::query(sqlx::AssertSqlSafe(sql))
                        .execute(connection)
                        .await?;
                    Ok(())
                })
            })
            .connect(&database)
            .await
            .unwrap();
        sqlx::migrate!("../../../../migrations")
            .run(&db)
            .await
            .unwrap();
        let app = App::new(db.clone(), config).await.unwrap();
        let result = async {
            sqlx::query("INSERT INTO users(id,email,username) VALUES('existing','existing@example.test','existing')")
                .execute(&db).await.map_err(|_| "Fixture setup failed")?;
            let mut snapshot = Snapshot {
                users: vec![json!({"id":"imported","email":"imported@example.test","username":"imported","passwordHash":null,"dateOfBirth":"1990-01-01T00:00:00Z","emailVerified":true,"createdAt":"2020-01-01T00:00:00Z","deletedAt":null,"twoFactorEnabled":false})],
                profiles: vec![json!({"id":"profile","userId":"imported","bio":"preserve"})],
                identities: vec![json!({"provider":"google","providerUserId":"test-subject","userId":"imported"})],
            };
            import(&app, &snapshot, &[0;32], false).await?;
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM users").fetch_one(&db).await.unwrap();
            if count != 1 { return Err("Dry run changed accounts".into()); }
            snapshot.users[0]["email"] = json!("EXISTING@example.test");
            if import(&app, &snapshot, &[0;32], true).await.is_ok() { return Err("Conflict was accepted".into()); }
            snapshot.users[0]["email"] = json!("imported@example.test");
            snapshot.users[0]["twoFactorEnabled"] = json!(true);
            snapshot.users[0]["twoFactorSecretEncrypted"] = json!("invalid");
            if import(&app, &snapshot, &[0;32], true).await.is_ok() { return Err("Broken secret was accepted".into()); }
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM users").fetch_one(&db).await.unwrap();
            if count != 1 { return Err("Failed import left partial accounts".into()); }
            snapshot.users[0]["twoFactorEnabled"] = json!(false);
            import(&app, &snapshot, &[0;32], true).await?;
            if import(&app, &snapshot, &[0;32], true).await.is_ok() { return Err("Duplicate import was accepted".into()); }
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM users").fetch_one(&db).await.unwrap();
            if count != 2 { return Err("Committed account count wrong".into()); }
            Ok::<(), String>(())
        }.await;
        db.close().await;
        // The schema identifier is a fixed prefix plus a locally generated simple UUID.
        sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
            .execute(&admin)
            .await
            .unwrap();
        result.unwrap();
    }
}
