//! The encrypted database: presage's store plus Copiale's tables.

use std::path::Path;

use presage_store_sqlite::{OnNewIdentity, SqliteConnectOptions, SqliteStore};
use sqlx::migrate::Migrator;
use sqlx::sqlite::{SqliteJournalMode, SqlitePool, SqliteSynchronous};
use sqlx::{ConnectOptions as _, Connection as _};

use super::StorageError;
use super::key::DatabaseKey;

/// Tracks Copiale's migrations apart from presage's `_sqlx_migrations`,
/// which rejects versions it doesn't know.
const MIGRATIONS_TABLE: &str = "_copiale_migrations";

/// SQLite result code for "file is not a database": a wrong SQLCipher key.
const SQLITE_NOTADB: &str = "26";

/// Open handle to the encrypted database.
pub struct Database {
    pub(super) presage: SqliteStore,
    pub(super) app: SqlitePool,
}

impl Database {
    pub(crate) async fn open(path: &Path, key: &DatabaseKey) -> Result<Self, StorageError> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true)
            .pragma("key", key.pragma_value())
            // SQLCipher otherwise writes to stderr, e.g. on a wrong key.
            // sqlx sends this before the first read.
            .pragma("cipher_log_level", "NONE")
            // Deleted messages are overwritten, not left in free pages.
            .pragma("secure_delete", "ON");

        initialize_sqlcipher().await?;
        verify_key(&options).await?;
        // Identity changes are shown to the user, not blocked, as in
        // Signal's own clients.
        let presage = SqliteStore::open_with_options(options.clone(), OnNewIdentity::Trust)
            .await
            .map_err(|error| StorageError::Database(error.to_string()))?;
        match open_app_pool(options).await {
            Ok(app) => {
                tracing::info!("database opened");
                Ok(Self { presage, app })
            }
            Err(error) => {
                presage.close().await;
                Err(error)
            }
        }
    }

    /// presage's store, sharing this database.
    pub fn presage_store(&self) -> &SqliteStore {
        &self.presage
    }

    /// When this database was created (ISO 8601, UTC).
    pub async fn created_at(&self) -> Result<String, StorageError> {
        sqlx::query_scalar("SELECT value FROM app_meta WHERE key = 'created_at'")
            .fetch_one(&self.app)
            .await
            .map_err(database_error)
    }

    /// Closes every connection and waits for them.
    ///
    /// Call before the process exits: SQLCipher frees its global state at
    /// exit, which crashes connections still closing in the background.
    pub async fn close(self) {
        self.app.close().await;
        self.presage.close().await;
    }

    #[cfg(test)]
    pub(crate) fn pool(&self) -> &SqlitePool {
        &self.app
    }
}

/// Copiale's pool, migrated.
async fn open_app_pool(options: SqliteConnectOptions) -> Result<SqlitePool, StorageError> {
    let app = SqlitePool::connect_with(
        options
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full),
    )
    .await
    .map_err(database_error)?;
    if let Err(error) = migrator().run(&app).await {
        app.close().await;
        return Err(StorageError::Database(error.to_string()));
    }
    Ok(app)
}

fn migrator() -> Migrator {
    let mut migrator = sqlx::migrate!();
    migrator.dangerous_set_table_name(MIGRATIONS_TABLE);
    migrator
}

/// Completes SQLCipher's one-time setup before any concurrent connections.
///
/// SQLite marks itself initialized before SQLCipher's setup finishes, so a
/// connection racing the first one can fail with "sqlcipher not
/// initialized". Opening one connection alone first avoids that.
async fn initialize_sqlcipher() -> Result<(), StorageError> {
    static INITIALIZED: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    INITIALIZED
        .get_or_try_init(|| async {
            let conn = SqliteConnectOptions::new()
                .in_memory(true)
                .connect()
                .await
                .map_err(database_error)?;
            conn.close().await.map_err(database_error)
        })
        .await
        .copied()
}

/// Fails with [`StorageError::WrongKey`] before presage touches the file.
async fn verify_key(options: &SqliteConnectOptions) -> Result<(), StorageError> {
    let mut conn = options.connect().await.map_err(database_error)?;
    let result = sqlx::query("SELECT count(*) FROM sqlite_master")
        .execute(&mut conn)
        .await;
    // Close explicitly, also on failure: see `Database::close`.
    conn.close().await.ok();
    result.map(drop).map_err(database_error)
}

pub(super) fn database_error(error: sqlx::Error) -> StorageError {
    match &error {
        sqlx::Error::Database(db) if db.code().as_deref() == Some(SQLITE_NOTADB) => {
            StorageError::WrongKey
        }
        _ => StorageError::Database(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn tables(db: &Database) -> Vec<String> {
        sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .fetch_all(db.pool())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn presage_and_app_tables_share_the_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let key = DatabaseKey::generate();

        let db = Database::open(&path, &key).await.unwrap();
        let names = tables(&db).await;
        db.close().await;

        for table in [
            "kv",
            "thread_messages",
            "_sqlx_migrations",
            "app_meta",
            MIGRATIONS_TABLE,
        ] {
            assert!(names.iter().any(|name| name == table), "missing {table}");
        }
    }

    #[tokio::test]
    async fn reopens_after_both_migration_sets_ran() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let key = DatabaseKey::generate();

        let db = Database::open(&path, &key).await.unwrap();
        let created_at = db.created_at().await.unwrap();
        db.close().await;

        let db = Database::open(&path, &key).await.unwrap();
        assert_eq!(db.created_at().await.unwrap(), created_at);
        db.close().await;
    }

    #[tokio::test]
    async fn wrong_key_is_reported_as_such() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        Database::open(&path, &DatabaseKey::generate())
            .await
            .unwrap()
            .close()
            .await;

        let result = Database::open(&path, &DatabaseKey::generate()).await;

        assert!(matches!(result, Err(StorageError::WrongKey)));
    }

    #[tokio::test]
    async fn sqlcipher_logging_is_off() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("test.db"), &DatabaseKey::generate())
            .await
            .unwrap();

        let level: String = sqlx::query_scalar("PRAGMA cipher_log_level")
            .fetch_one(db.pool())
            .await
            .unwrap();

        assert_eq!(level, "NONE");
        db.close().await;
    }

    /// Runs migrations on a plain database: all, or the first `count`.
    async fn migrate(pool: &SqlitePool, count: Option<usize>) {
        let mut migrator = migrator();
        if let Some(count) = count {
            migrator.migrations = migrator.migrations[..count].to_vec().into();
        }
        migrator.run(pool).await.unwrap();
    }

    #[tokio::test]
    async fn group_migration_keeps_conversations_and_messages() {
        initialize_sqlcipher().await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let options = SqliteConnectOptions::new()
            .filename(dir.path().join("plain.db"))
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePool::connect_with(options).await.unwrap();
        // Up to and including message status, before groups.
        migrate(&pool, Some(3)).await;
        let peer = [7u8; 16];
        sqlx::query("INSERT INTO conversations (id, peer, last_activity) VALUES (1, ?, 5)")
            .bind(peer.as_slice())
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO messages
                 (id, conversation_id, outgoing, sent_at, received_at, body, read, status)
             VALUES (1, 1, 0, 100, 101, 'in', 0, NULL), (2, 1, 1, 100, 102, 'out', 1, 'sent')",
        )
        .execute(&pool)
        .await
        .unwrap();

        migrate(&pool, None).await;

        let authors: Vec<(i64, Vec<u8>, String)> =
            sqlx::query_as("SELECT id, author, body FROM messages ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            authors,
            [
                (1, peer.to_vec(), "in".to_string()),
                (2, Vec::new(), "out".to_string())
            ]
        );
        let conversation: (Vec<u8>, Option<i64>) =
            sqlx::query_as("SELECT peer, last_activity FROM conversations WHERE id = 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(conversation, (peer.to_vec(), Some(5)));
        let broken: Vec<(String,)> = sqlx::query_as("PRAGMA foreign_key_check")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert!(broken.is_empty());
        // Deleting a conversation still deletes its messages.
        sqlx::query("DELETE FROM conversations")
            .execute(&pool)
            .await
            .unwrap();
        let left: i64 = sqlx::query_scalar("SELECT count(*) FROM messages")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(left, 0);
        pool.close().await;
    }

    #[tokio::test]
    async fn file_has_no_plaintext_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        Database::open(&path, &DatabaseKey::generate())
            .await
            .unwrap()
            .close()
            .await;

        let bytes = std::fs::read(&path).unwrap();
        assert!(!bytes.starts_with(b"SQLite format 3\0"));
    }
}
