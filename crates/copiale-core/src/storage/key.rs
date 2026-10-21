//! Database keys and passphrase-based key derivation.

use std::fmt::{self, Write as _};
use std::io::Write as _;
use std::path::Path;

use argon2::{Algorithm, Argon2, Params, Version};
use rand::RngCore as _;
use zeroize::Zeroizing;

use super::StorageError;

const KEY_LEN: usize = 32;
const SALT_LEN: usize = 16;

/// 256-bit SQLCipher key. Zeroed on drop; `Debug` hides it.
pub struct DatabaseKey(Zeroizing<[u8; KEY_LEN]>);

impl DatabaseKey {
    pub fn generate() -> Self {
        let mut key = Zeroizing::new([0; KEY_LEN]);
        rand::rng().fill_bytes(key.as_mut());
        Self(key)
    }

    /// `None` unless `bytes` is exactly 32 bytes long.
    pub fn from_slice(bytes: &[u8]) -> Option<Self> {
        let key: [u8; KEY_LEN] = bytes.try_into().ok()?;
        Some(Self(Zeroizing::new(key)))
    }

    pub fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }

    /// Value for `PRAGMA key`: a raw key, so SQLCipher skips its own key
    /// derivation.
    ///
    /// Not zeroed: sqlx keeps it for the pools' lifetime (see
    /// `docs/security.md`). Built in place, leaving no other copies.
    pub(crate) fn pragma_value(&self) -> String {
        let mut value = String::with_capacity(2 * KEY_LEN + 5);
        value.push_str("\"x'");
        for byte in self.as_bytes() {
            write!(value, "{byte:02x}").expect("writing to a String");
        }
        value.push_str("'\"");
        value
    }
}

impl fmt::Debug for DatabaseKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DatabaseKey(..)")
    }
}

/// User passphrase. Zeroed on drop; `Debug` hides it.
pub struct Passphrase(Zeroizing<String>);

impl Passphrase {
    pub fn new(passphrase: impl Into<String>) -> Self {
        Self(Zeroizing::new(passphrase.into()))
    }
}

impl fmt::Debug for Passphrase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Passphrase(..)")
    }
}

/// Argon2id cost parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KdfParams {
    pub memory_kib: u32,
    pub iterations: u32,
    pub parallelism: u32,
}

impl Default for KdfParams {
    /// 256 MiB, 3 passes, 4 lanes: RFC 9106's second recommended option
    /// with 4× the memory, still well under a second.
    fn default() -> Self {
        Self {
            memory_kib: 256 * 1024,
            iterations: 3,
            parallelism: 4,
        }
    }
}

/// Salt and cost of a passphrase-derived key, stored next to the database.
///
/// One line: `argon2id m=<KiB> t=<passes> p=<lanes> salt=<hex>`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct KeyFile {
    params: KdfParams,
    salt: [u8; SALT_LEN],
}

impl KeyFile {
    pub(crate) fn new(params: KdfParams) -> Self {
        let mut salt = [0; SALT_LEN];
        rand::rng().fill_bytes(&mut salt);
        Self { params, salt }
    }

    pub(crate) fn derive(&self, passphrase: &Passphrase) -> Result<DatabaseKey, StorageError> {
        let KdfParams {
            memory_kib,
            iterations,
            parallelism,
        } = self.params;
        let params = Params::new(memory_kib, iterations, parallelism, Some(KEY_LEN))
            .map_err(|error| StorageError::KeyFile(error.to_string()))?;
        let mut key = Zeroizing::new([0; KEY_LEN]);
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            .hash_password_into(passphrase.0.as_bytes(), &self.salt, key.as_mut())
            .map_err(|error| StorageError::KeyFile(error.to_string()))?;
        Ok(DatabaseKey(key))
    }

    pub(crate) fn read(path: &Path) -> Result<Self, StorageError> {
        let text = std::fs::read_to_string(path)?;
        Self::parse(&text).ok_or_else(|| StorageError::KeyFile(path.display().to_string()))
    }

    /// Writes the file readable by the current user only; never overwrites.
    pub(crate) fn write(&self, path: &Path) -> std::io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        file.write_all(self.format().as_bytes())?;
        file.sync_all()
    }

    fn format(&self) -> String {
        let KdfParams {
            memory_kib,
            iterations,
            parallelism,
        } = self.params;
        format!(
            "argon2id m={memory_kib} t={iterations} p={parallelism} salt={}\n",
            to_hex(&self.salt)
        )
    }

    fn parse(text: &str) -> Option<Self> {
        let mut fields = text.trim().split(' ');
        if fields.next()? != "argon2id" {
            return None;
        }
        let mut field = |name: &str| fields.next()?.strip_prefix(name)?.strip_prefix('=');
        let memory_kib = field("m")?.parse().ok()?;
        let iterations = field("t")?.parse().ok()?;
        let parallelism = field("p")?.parse().ok()?;
        let salt = from_hex(field("salt")?)?.try_into().ok()?;
        if fields.next().is_some() {
            return None;
        }
        Some(Self {
            params: KdfParams {
                memory_kib,
                iterations,
                parallelism,
            },
            salt,
        })
    }
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHEAP: KdfParams = KdfParams {
        memory_kib: 64,
        iterations: 1,
        parallelism: 1,
    };

    #[test]
    fn debug_hides_secrets() {
        assert_eq!(format!("{:?}", DatabaseKey::generate()), "DatabaseKey(..)");
        assert_eq!(
            format!("{:?}", Passphrase::new("hunter2")),
            "Passphrase(..)"
        );
    }

    #[test]
    fn generated_keys_differ() {
        assert_ne!(
            DatabaseKey::generate().as_bytes(),
            DatabaseKey::generate().as_bytes()
        );
    }

    #[test]
    fn key_requires_32_bytes() {
        assert!(DatabaseKey::from_slice(&[7; 32]).is_some());
        assert!(DatabaseKey::from_slice(&[7; 31]).is_none());
        assert!(DatabaseKey::from_slice(&[7; 33]).is_none());
    }

    #[test]
    fn pragma_value_is_raw_hex_key() {
        let key = DatabaseKey::from_slice(&[0xab; 32]).unwrap();
        assert_eq!(key.pragma_value(), format!("\"x'{}'\"", "ab".repeat(32)));
    }

    #[test]
    fn key_file_round_trips() {
        let key_file = KeyFile::new(KdfParams::default());
        assert_eq!(KeyFile::parse(&key_file.format()), Some(key_file));
    }

    #[test]
    fn key_file_rejects_malformed_text() {
        let salt = "00".repeat(SALT_LEN);
        for text in [
            "".to_string(),
            format!("scrypt m=64 t=1 p=1 salt={salt}"),
            format!("argon2id m=64 t=1 salt={salt}"),
            format!("argon2id m=64 t=1 p=1 salt={}", "00".repeat(SALT_LEN - 1)),
            "argon2id m=64 t=1 p=1 salt=zz".to_string(),
            format!("argon2id m=64 t=1 p=1 salt={salt} extra"),
        ] {
            assert_eq!(KeyFile::parse(&text), None, "{text:?}");
        }
    }

    #[test]
    fn derivation_depends_on_passphrase_and_salt() {
        let key_file = KeyFile::new(CHEAP);
        let derive = |key_file: &KeyFile, passphrase: &str| {
            *key_file
                .derive(&Passphrase::new(passphrase))
                .unwrap()
                .as_bytes()
        };

        assert_eq!(derive(&key_file, "one"), derive(&key_file, "one"));
        assert_ne!(derive(&key_file, "one"), derive(&key_file, "two"));
        assert_ne!(
            derive(&key_file, "one"),
            derive(&KeyFile::new(CHEAP), "one")
        );
    }

    #[test]
    fn invalid_params_fail_derivation() {
        let key_file = KeyFile {
            params: KdfParams {
                memory_kib: 1,
                iterations: 1,
                parallelism: 1,
            },
            salt: [0; SALT_LEN],
        };
        assert!(matches!(
            key_file.derive(&Passphrase::new("x")),
            Err(StorageError::KeyFile(_))
        ));
    }
}
