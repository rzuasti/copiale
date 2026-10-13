//! Where the database key is kept when a keychain is available.

use zeroize::Zeroizing;

use crate::APP_ID;

/// One secret in a credential store: the database key.
pub trait SecretStore {
    /// `Ok(None)` if no secret is stored.
    fn get(&self) -> Result<Option<Zeroizing<Vec<u8>>>, SecretError>;
    fn set(&self, secret: &[u8]) -> Result<(), SecretError>;
    /// Succeeds if no secret is stored.
    fn delete(&self) -> Result<(), SecretError>;
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SecretError {
    /// No usable credential store: missing, locked or access denied.
    #[error("{0}")]
    Unavailable(String),
    #[error("{0}")]
    Failed(String),
}

/// The OS keychain: Keychain Services, Credential Manager or Secret Service.
#[derive(Clone, Debug)]
pub struct OsKeychain {
    service: String,
    user: String,
}

impl OsKeychain {
    pub fn new() -> Self {
        Self {
            service: APP_ID.into(),
            user: "database-key".into(),
        }
    }

    fn entry(&self) -> Result<keyring::Entry, SecretError> {
        keyring::Entry::new(&self.service, &self.user).map_err(map_error)
    }
}

impl Default for OsKeychain {
    fn default() -> Self {
        Self::new()
    }
}

impl SecretStore for OsKeychain {
    fn get(&self) -> Result<Option<Zeroizing<Vec<u8>>>, SecretError> {
        match self.entry()?.get_secret() {
            Ok(secret) => Ok(Some(Zeroizing::new(secret))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(map_error(error)),
        }
    }

    fn set(&self, secret: &[u8]) -> Result<(), SecretError> {
        self.entry()?.set_secret(secret).map_err(map_error)
    }

    fn delete(&self) -> Result<(), SecretError> {
        match self.entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(map_error(error)),
        }
    }
}

fn map_error(error: keyring::Error) -> SecretError {
    match error {
        keyring::Error::NoDefaultStore
        | keyring::Error::NoStorageAccess(_)
        | keyring::Error::PlatformFailure(_) => SecretError::Unavailable(error.to_string()),
        error => SecretError::Failed(error.to_string()),
    }
}
