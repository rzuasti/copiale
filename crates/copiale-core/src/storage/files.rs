//! Downloaded attachments on disk, each encrypted with its own key, which
//! the database keeps: once the key is deleted, the file can't be read.
//!
//! Blocking, except [`Files::store`].

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use aes_gcm::aead::{AeadInOut as _, KeyInit as _};
use aes_gcm::{Aes256Gcm, Nonce};
use rand::RngCore as _;
use zeroize::Zeroizing;

/// The directory next to the database that holds the files.
pub(super) const DIR: &str = "attachments";

/// Bytes of the random nonce each file starts with.
const NONCE_BYTES: usize = 12;
/// Suffix of a file still being written.
const PART: &str = ".part";

/// Where the encrypted files live.
#[derive(Clone, Debug)]
pub(crate) struct Files {
    dir: PathBuf,
}

/// A file just written, not yet recorded in the database.
#[derive(Debug)]
pub(crate) struct StoredFile {
    pub(super) name: String,
    pub(super) key: FileKey,
}

/// One file's AES-256-GCM key. `Debug` hides it.
pub(super) struct FileKey(Zeroizing<[u8; 32]>);

impl FileKey {
    fn generate() -> Self {
        let mut key = Zeroizing::new([0; 32]);
        rand::rng().fill_bytes(key.as_mut());
        Self(key)
    }

    pub(super) fn from_slice(bytes: &[u8]) -> Option<Self> {
        let key: [u8; 32] = bytes.try_into().ok()?;
        Some(Self(Zeroizing::new(key)))
    }

    pub(super) fn as_bytes(&self) -> &[u8] {
        self.0.as_ref()
    }

    fn cipher(&self) -> Aes256Gcm {
        let key: &[u8; 32] = &self.0;
        Aes256Gcm::new(key.into())
    }
}

impl fmt::Debug for FileKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FileKey")
    }
}

impl Files {
    /// The files kept next to the database at `database`.
    pub(crate) fn beside(database: &Path) -> Self {
        let dir = database.parent().unwrap_or(Path::new(".")).join(DIR);
        Self { dir }
    }

    /// [`Self::write`] on the blocking pool.
    pub(crate) async fn store(&self, contents: Vec<u8>) -> io::Result<StoredFile> {
        let files = self.clone();
        tokio::task::spawn_blocking(move || files.write(contents))
            .await
            .map_err(io::Error::other)?
    }

    /// Encrypts `contents` in place into a new file.
    pub(super) fn write(&self, mut contents: Vec<u8>) -> io::Result<StoredFile> {
        crate::create_private_dir(&self.dir)?;
        let mut id = [0u8; 16];
        rand::rng().fill_bytes(&mut id);
        let name: String = id.iter().map(|byte| format!("{byte:02x}")).collect();
        let key = FileKey::generate();
        let mut nonce = [0u8; NONCE_BYTES];
        rand::rng().fill_bytes(&mut nonce);
        key.cipher()
            .encrypt_in_place(&Nonce::from(nonce), name.as_bytes(), &mut contents)
            .map_err(|_| io::Error::other("encrypting a file failed"))?;
        let part = self.dir.join(format!("{name}{PART}"));
        let written = (|| {
            let mut file = create_private(&part)?;
            file.write_all(&nonce)?;
            file.write_all(&contents)?;
            file.sync_all()?;
            fs::rename(&part, self.path(&name)?)
        })();
        if let Err(error) = written {
            fs::remove_file(&part).ok();
            return Err(error);
        }
        Ok(StoredFile { name, key })
    }

    /// Reads and decrypts a file.
    pub(super) fn read(&self, name: &str, key: &FileKey) -> io::Result<Vec<u8>> {
        let mut contents = fs::read(self.path(name)?)?;
        if contents.len() < NONCE_BYTES {
            return Err(io::Error::other("file too short"));
        }
        let nonce: [u8; NONCE_BYTES] = contents[..NONCE_BYTES].try_into().unwrap_or_default();
        contents.drain(..NONCE_BYTES);
        key.cipher()
            .decrypt_in_place(&Nonce::from(nonce), name.as_bytes(), &mut contents)
            .map_err(|_| io::Error::other("file corrupt or key wrong"))?;
        Ok(contents)
    }

    /// Deletes a file, if it's still there.
    pub(super) fn remove(&self, name: &str) -> io::Result<()> {
        match fs::remove_file(self.path(name)?) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    /// Every file's name, including unfinished ones.
    pub(super) fn list(&self) -> io::Result<Vec<String>> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut names = Vec::new();
        for entry in entries {
            if let Some(name) = entry?.file_name().to_str() {
                names.push(name.to_owned());
            }
        }
        Ok(names)
    }

    /// Only names this module makes, so none leads outside the directory.
    fn path(&self, name: &str) -> io::Result<PathBuf> {
        let stem = name.strip_suffix(PART).unwrap_or(name);
        if stem.is_empty() || !stem.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(io::Error::other("invalid file name"));
        }
        Ok(self.dir.join(name))
    }
}

/// A new file only the user can read.
fn create_private(path: &Path) -> io::Result<fs::File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files() -> (tempfile::TempDir, Files) {
        let dir = tempfile::tempdir().unwrap();
        let files = Files::beside(&dir.path().join("test.db"));
        (dir, files)
    }

    #[test]
    fn files_are_encrypted_and_read_back() {
        let (dir, files) = files();
        let contents = b"a secret picture".repeat(100);

        let StoredFile { name, key } = files.write(contents.clone()).unwrap();

        let on_disk = fs::read(dir.path().join(DIR).join(&name)).unwrap();
        assert!(
            !on_disk
                .windows(16)
                .any(|window| window == b"a secret picture")
        );
        assert_eq!(files.read(&name, &key).unwrap(), contents);
        assert_eq!(files.list().unwrap(), [name]);
    }

    #[test]
    fn another_key_or_name_doesnt_read() {
        let (dir, files) = files();
        let StoredFile { name, key } = files.write(b"hello".to_vec()).unwrap();
        let other = files.write(b"other".to_vec()).unwrap();
        let (other, other_key) = (other.name, other.key);

        assert!(files.read(&name, &other_key).is_err());
        // Swapped files fail too: the name is authenticated.
        let folder = dir.path().join(DIR);
        fs::copy(folder.join(&name), folder.join(&other)).unwrap();
        assert!(files.read(&other, &key).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn files_are_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let (dir, files) = files();
        let name = files.write(b"hello".to_vec()).unwrap().name;

        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir.path().join(DIR)), 0o700);
        assert_eq!(mode(&dir.path().join(DIR).join(name)), 0o600);
    }

    #[test]
    fn removing_twice_is_fine_and_names_stay_inside() {
        let (_dir, files) = files();
        let name = files.write(b"hello".to_vec()).unwrap().name;

        files.remove(&name).unwrap();
        files.remove(&name).unwrap();
        assert!(files.list().unwrap().is_empty());
        assert!(files.remove("../copiale.db").is_err());
        assert!(files.read("", &FileKey::generate()).is_err());
    }
}
