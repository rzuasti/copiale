//! Encrypted storage: one SQLCipher database for presage and Copiale.
//!
//! The 256-bit key lives in the OS keychain or, without one, is derived from
//! a passphrase with Argon2id. See `docs/spike-sqlcipher-results.md`.
//!
//! Key handling is blocking (keychain I/O, key derivation); opening the
//! database is async.

mod database;
mod key;
mod secrets;

use std::path::{Path, PathBuf};

pub use database::Database;
pub use key::{DatabaseKey, KdfParams, Passphrase};
pub use secrets::{OsKeychain, SecretError, SecretStore};

use key::KeyFile;

/// Locations of the database and the passphrase key file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoragePaths {
    pub database: PathBuf,
    /// Present only for passphrase-encrypted databases.
    pub key_file: PathBuf,
}

impl StoragePaths {
    pub fn in_dir(dir: &Path) -> Self {
        Self {
            database: dir.join("copiale.db"),
            key_file: dir.join("copiale.key"),
        }
    }

    /// Platform data dir, e.g. `~/.local/share/copiale` on Linux.
    pub fn platform() -> Option<Self> {
        Some(Self::in_dir(crate::dirs::project_dirs()?.data_dir()))
    }

    fn dir(&self) -> &Path {
        self.database.parent().unwrap_or(Path::new("."))
    }
}

/// Outcome of [`Storage::find_key`].
#[derive(Debug)]
pub enum KeyLookup {
    Found(DatabaseKey),
    /// Passphrase-encrypted: get the key from [`Storage::key_from_passphrase`].
    NeedsPassphrase,
    /// No keychain and no database yet: set one up with
    /// [`Storage::new_passphrase_key`].
    NeedsNewPassphrase,
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("wrong key or passphrase")]
    WrongKey,
    /// The database exists but its key is gone; it cannot be opened.
    #[error("the database key is missing from the keychain")]
    KeyMissing,
    #[error("keychain unavailable: {0}")]
    KeychainUnavailable(String),
    #[error("keychain error: {0}")]
    Keychain(String),
    #[error("invalid key file: {0}")]
    KeyFile(String),
    #[error("a database already exists")]
    DatabaseExists,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("database error: {0}")]
    Database(String),
}

/// Finds or creates the database key and opens the database.
#[derive(Clone, Debug)]
pub struct Storage {
    paths: StoragePaths,
    kdf: KdfParams,
}

impl Storage {
    pub fn new(paths: StoragePaths) -> Self {
        Self {
            paths,
            kdf: KdfParams::default(),
        }
    }

    /// Overrides the Argon2id cost for new passphrase keys.
    pub fn with_kdf_params(mut self, kdf: KdfParams) -> Self {
        self.kdf = kdf;
        self
    }

    pub fn paths(&self) -> &StoragePaths {
        &self.paths
    }

    /// Looks up the database key. Blocking.
    ///
    /// On first run with a working keychain, generates a key and stores it
    /// there before the database exists.
    pub fn find_key(&self, secrets: &impl SecretStore) -> Result<KeyLookup, StorageError> {
        if self.paths.key_file.exists() {
            tracing::info!("database is passphrase-encrypted");
            return Ok(KeyLookup::NeedsPassphrase);
        }
        if self.paths.database.exists() {
            return match secrets.get() {
                Ok(Some(secret)) => DatabaseKey::from_slice(&secret)
                    .map(KeyLookup::Found)
                    .ok_or_else(|| {
                        StorageError::Keychain("stored key has the wrong length".into())
                    }),
                Ok(None) => Err(StorageError::KeyMissing),
                Err(SecretError::Unavailable(error)) => {
                    Err(StorageError::KeychainUnavailable(error))
                }
                Err(SecretError::Failed(error)) => Err(StorageError::Keychain(error)),
            };
        }
        let key = DatabaseKey::generate();
        match secrets.set(key.as_bytes()) {
            Ok(()) => {
                tracing::info!("new database key stored in the keychain");
                Ok(KeyLookup::Found(key))
            }
            Err(SecretError::Unavailable(error)) => {
                tracing::info!(%error, "no keychain; a passphrase is needed");
                Ok(KeyLookup::NeedsNewPassphrase)
            }
            Err(SecretError::Failed(error)) => Err(StorageError::Keychain(error)),
        }
    }

    /// Derives the key of a passphrase-encrypted database. Blocking.
    ///
    /// A wrong passphrase surfaces as [`StorageError::WrongKey`] from
    /// [`Self::open`].
    pub fn key_from_passphrase(
        &self,
        passphrase: &Passphrase,
    ) -> Result<DatabaseKey, StorageError> {
        KeyFile::read(&self.paths.key_file)?.derive(passphrase)
    }

    /// Sets up passphrase encryption for a new database. Blocking.
    pub fn new_passphrase_key(&self, passphrase: &Passphrase) -> Result<DatabaseKey, StorageError> {
        if self.paths.database.exists() {
            return Err(StorageError::DatabaseExists);
        }
        ensure_private_dir(self.paths.dir())?;
        let key_file = KeyFile::new(self.kdf);
        let key = key_file.derive(passphrase)?;
        key_file.write(&self.paths.key_file)?;
        Ok(key)
    }

    /// Opens the database, creating it if needed, and runs migrations.
    pub async fn open(&self, key: &DatabaseKey) -> Result<Database, StorageError> {
        ensure_private_dir(self.paths.dir())?;
        Database::open(&self.paths.database, key).await
    }
}

/// Creates `dir` if needed and restricts it to the current user.
fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
