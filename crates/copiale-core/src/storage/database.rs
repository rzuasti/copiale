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
    presage: SqliteStore,
    app: SqlitePool,
}

impl Database {
    pub(crate) async fn open(path: &Path, key: &DatabaseKey) -> Result<Self, StorageError> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true)
            .pragma("key", key.pragma_value());

        verify_key(&options).await?;
        // Identity changes are shown to the user, not blocked, as in
        // Signal's own clients.
        let presage = SqliteStore::open_with_options(options.clone(), OnNewIdentity::Trust)
            .await
            .map_err(|error| StorageError::Database(error.to_string()))?;
        let app = SqlitePool::connect_with(
            options
                .journal_mode(SqliteJournalMode::Wal)
                .synchronous(SqliteSynchronous::Full),
        )
        .await
        .map_err(database_error)?;
        migrator()
            .run(&app)
            .await
            .map_err(|error| StorageError::Database(error.to_string()))?;

        Ok(Self { presage, app })
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

    pub async fn close(self) {
        self.app.close().await;
    }

    #[cfg(test)]
    pub(crate) fn pool(&self) -> &SqlitePool {
        &self.app
    }
}

fn migrator() -> Migrator {
    let mut migrator = sqlx::migrate!();
    migrator.dangerous_set_table_name(MIGRATIONS_TABLE);
    migrator
}

/// Fails with [`StorageError::WrongKey`] before presage touches the file.
async fn verify_key(options: &SqliteConnectOptions) -> Result<(), StorageError> {
    let mut conn = options.connect().await.map_err(database_error)?;
    sqlx::query("SELECT count(*) FROM sqlite_master")
        .execute(&mut conn)
        .await
        .map_err(database_error)?;
    conn.close().await.ok();
    Ok(())
}

fn database_error(error: sqlx::Error) -> StorageError {
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
