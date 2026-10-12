//! Storage spike: presage's SQLite store on SQLCipher, sharing one database
//! with Copiale's own tables.
//!
//! Prints one line per check and exits non-zero if any check fails.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use presage_store_sqlite::{OnNewIdentity, SqliteConnectOptions, SqliteStore};
use rand::RngCore;
use sqlx::migrate::Migrator;
use sqlx::{ConnectOptions, Connection};

/// Copiale's migrations, tracked separately from presage's `_sqlx_migrations`.
const APP_MIGRATIONS_TABLE: &str = "_copiale_migrations";
const MARKER: &str = "COPIALE-PLAINTEXT-MARKER";

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[tokio::main]
async fn main() {
    let dir = std::env::temp_dir().join(format!("copiale-spike-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");

    let mut checks = Checks::default();
    if let Err(error) = run(&dir, &mut checks).await {
        checks.record("spike ran to completion", false, &error.to_string());
    }
    std::fs::remove_dir_all(&dir).ok();

    println!("\n{} passed, {} failed", checks.passed, checks.failed.len());
    if !checks.failed.is_empty() {
        std::process::exit(1);
    }
}

async fn run(dir: &Path, checks: &mut Checks) -> Result<()> {
    let key = random_key();
    let db = dir.join("copiale.db");

    // SQLCipher is linked and the raw key is accepted.
    let mut conn = options(&db, Some(&raw_key_pragma(&key))).connect().await?;
    let version: String = sqlx::query_scalar("PRAGMA cipher_version")
        .fetch_one(&mut conn)
        .await?;
    let provider: String = sqlx::query_scalar("PRAGMA cipher_provider")
        .fetch_one(&mut conn)
        .await?;
    checks.record(
        "SQLCipher linked",
        !version.is_empty(),
        &format!("cipher_version {version}, provider {provider}"),
    );
    conn.close().await?;

    // presage opens the encrypted database and runs its migrations.
    let store = open_presage(&db, &key).await;
    checks.record(
        "presage store opens with raw key",
        store.is_ok(),
        &error_text(&store),
    );
    drop(store);

    // App migrations in their own table, then presage again.
    let mut conn = options(&db, Some(&raw_key_pragma(&key))).connect().await?;
    app_migrator(Some(APP_MIGRATIONS_TABLE))
        .run(&mut conn)
        .await?;
    sqlx::query("INSERT INTO app_messages (body) VALUES (?)")
        .bind(MARKER)
        .execute(&mut conn)
        .await?;
    conn.close().await?;
    let store = open_presage(&db, &key).await;
    checks.record(
        "presage reopens after app migrations (separate table)",
        store.is_ok(),
        &error_text(&store),
    );
    drop(store);
    let mut conn = options(&db, Some(&raw_key_pragma(&key))).connect().await?;
    let rerun = app_migrator(Some(APP_MIGRATIONS_TABLE))
        .run(&mut conn)
        .await;
    checks.record(
        "app migrations rerun after presage's",
        rerun.is_ok(),
        &error_text(&rerun),
    );
    let tables: Vec<String> =
        sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .fetch_all(&mut conn)
            .await?;
    checks.record(
        "presage and app tables share the database",
        tables.iter().any(|t| t == "app_messages") && tables.iter().any(|t| t == "kv"),
        &format!("{} tables", tables.len()),
    );
    conn.close().await?;

    // Negative control: app migrations in presage's default table.
    let shared = dir.join("shared-table.db");
    drop(open_presage(&shared, &key).await?);
    let mut conn = options(&shared, Some(&raw_key_pragma(&key)))
        .connect()
        .await?;
    app_migrator(None)
        .set_ignore_missing(true)
        .run(&mut conn)
        .await?;
    conn.close().await?;
    let store = open_presage(&shared, &key).await;
    checks.record(
        "control: shared _sqlx_migrations breaks presage",
        store.is_err(),
        &error_text(&store),
    );
    drop(store);

    // Contents are encrypted at rest, including the WAL.
    let header_plain = read(&db)?.starts_with(b"SQLite format 3\0");
    checks.record("database has no SQLite header", !header_plain, "");
    let leaked = [db.clone(), wal_path(&db)]
        .iter()
        .filter(|path| path.exists())
        .any(|path| contains(&read(path).unwrap_or_default(), MARKER.as_bytes()));
    checks.record("marker not in database or WAL bytes", !leaked, "");

    // Right key reads back; wrong or missing key fails.
    let mut conn = options(&db, Some(&raw_key_pragma(&key))).connect().await?;
    let body: String = sqlx::query_scalar("SELECT body FROM app_messages LIMIT 1")
        .fetch_one(&mut conn)
        .await?;
    checks.record("right key reads data back", body == MARKER, "");
    conn.close().await?;
    let wrong = read_marker(&db, Some(&raw_key_pragma(&random_key()))).await;
    checks.record("wrong key fails", wrong.is_err(), &error_text(&wrong));
    let missing = read_marker(&db, None).await;
    checks.record("missing key fails", missing.is_err(), &error_text(&missing));
    let presage_wrong = open_presage(&db, &random_key()).await;
    checks.record(
        "presage with wrong key fails",
        presage_wrong.is_err(),
        &error_text(&presage_wrong),
    );

    // Open cost: raw key vs SQLCipher's PBKDF2 passphrase derivation.
    let raw = time_open(&db, &raw_key_pragma(&key)).await?;
    let pass_db = dir.join("passphrase.db");
    let pass_pragma = "'correct horse battery staple'".to_string();
    let mut conn = options(&pass_db, Some(&pass_pragma)).connect().await?;
    sqlx::query("CREATE TABLE t (x)").execute(&mut conn).await?;
    conn.close().await?;
    let passphrase = time_open(&pass_db, &pass_pragma).await?;
    checks.record(
        "open cost measured",
        true,
        &format!("raw key {raw:?}, passphrase (PBKDF2) {passphrase:?}"),
    );

    Ok(())
}

fn options(path: &Path, key_pragma: Option<&str>) -> SqliteConnectOptions {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true);
    match key_pragma {
        Some(value) => options.pragma("key", value.to_string()),
        None => options,
    }
}

async fn open_presage(path: &Path, key: &[u8; 32]) -> Result<SqliteStore> {
    let options = options(path, Some(&raw_key_pragma(key))).foreign_keys(true);
    Ok(SqliteStore::open_with_options(options, OnNewIdentity::Trust).await?)
}

fn app_migrator(table: Option<&'static str>) -> Migrator {
    let mut migrator = sqlx::migrate!();
    if let Some(table) = table {
        migrator.dangerous_set_table_name(table);
    }
    migrator
}

async fn read_marker(path: &Path, key_pragma: Option<&str>) -> Result<String> {
    let mut conn = options(path, key_pragma).connect().await?;
    Ok(sqlx::query_scalar("SELECT body FROM app_messages LIMIT 1")
        .fetch_one(&mut conn)
        .await?)
}

/// Median time to connect and run a first query.
async fn time_open(path: &Path, key_pragma: &str) -> Result<Duration> {
    let mut samples = Vec::new();
    for _ in 0..5 {
        let started = Instant::now();
        let mut conn = options(path, Some(key_pragma)).connect().await?;
        sqlx::query("SELECT count(*) FROM sqlite_master")
            .execute(&mut conn)
            .await?;
        samples.push(started.elapsed());
        conn.close().await?;
    }
    samples.sort();
    Ok(samples[samples.len() / 2])
}

fn random_key() -> [u8; 32] {
    let mut key = [0; 32];
    rand::rng().fill_bytes(&mut key);
    key
}

/// SQLCipher raw key syntax: skips key derivation.
fn raw_key_pragma(key: &[u8; 32]) -> String {
    format!("\"x'{}'\"", hex::encode(key))
}

fn wal_path(db: &Path) -> PathBuf {
    let mut name = db.as_os_str().to_owned();
    name.push("-wal");
    name.into()
}

fn read(path: &Path) -> std::io::Result<Vec<u8>> {
    std::fs::read(path)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn error_text<T, E: std::fmt::Display>(result: &std::result::Result<T, E>) -> String {
    match result {
        Ok(_) => String::new(),
        Err(error) => error.to_string(),
    }
}

#[derive(Default)]
struct Checks {
    passed: usize,
    failed: Vec<String>,
}

impl Checks {
    fn record(&mut self, name: &str, ok: bool, detail: &str) {
        let status = if ok { "PASS" } else { "FAIL" };
        if detail.is_empty() {
            println!("{status}  {name}");
        } else {
            println!("{status}  {name}: {detail}");
        }
        if ok {
            self.passed += 1;
        } else {
            self.failed.push(name.to_string());
        }
    }
}
