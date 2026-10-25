//! Key lookup and database open flows, with an in-memory keychain.

use copiale_core::storage::{
    DatabaseKey, KdfParams, KeyLookup, Passphrase, Storage, StorageError, StoragePaths, Wiped,
};
use copiale_testkit::MemorySecretStore;
use tempfile::TempDir;

const CHEAP_KDF: KdfParams = KdfParams {
    memory_kib: 64,
    iterations: 1,
    parallelism: 1,
};

fn storage() -> (TempDir, Storage) {
    let dir = tempfile::tempdir().unwrap();
    let paths = StoragePaths::in_dir(&dir.path().join("data"));
    (dir, Storage::new(paths).with_kdf_params(CHEAP_KDF))
}

fn found(lookup: KeyLookup) -> DatabaseKey {
    match lookup {
        KeyLookup::Found(key) => key,
        other => panic!("expected a key, got {other:?}"),
    }
}

async fn create_with_keychain(storage: &Storage, secrets: &MemorySecretStore) {
    let key = found(storage.find_key(secrets).unwrap());
    storage.open(&key).await.unwrap().close().await;
}

#[tokio::test]
async fn first_run_stores_new_key_in_keychain() {
    let (_dir, storage) = storage();
    let secrets = MemorySecretStore::default();

    let key = found(storage.find_key(&secrets).unwrap());

    assert_eq!(secrets.secret().as_deref(), Some(&key.as_bytes()[..]));
    storage.open(&key).await.unwrap().close().await;
    assert!(storage.paths().database.exists());
    assert!(!storage.paths().key_file.exists());
}

#[tokio::test]
async fn reopens_with_key_from_keychain() {
    let (_dir, storage) = storage();
    let secrets = MemorySecretStore::default();
    create_with_keychain(&storage, &secrets).await;

    let key = found(storage.find_key(&secrets).unwrap());

    assert_eq!(secrets.secret().as_deref(), Some(&key.as_bytes()[..]));
    let db = storage.open(&key).await.unwrap();
    assert!(!db.created_at().await.unwrap().is_empty());
    db.close().await;
}

#[tokio::test]
async fn stale_keychain_entry_is_replaced_on_first_run() {
    let (_dir, storage) = storage();
    let secrets = MemorySecretStore::default();
    secrets.put_secret(&[1; 32]);

    let key = found(storage.find_key(&secrets).unwrap());

    assert_ne!(key.as_bytes(), &[1; 32]);
    assert_eq!(secrets.secret().as_deref(), Some(&key.as_bytes()[..]));
}

#[tokio::test]
async fn missing_keychain_entry_for_existing_database_is_an_error() {
    let (_dir, storage) = storage();
    let secrets = MemorySecretStore::default();
    create_with_keychain(&storage, &secrets).await;
    secrets.clear_secret();

    let result = storage.find_key(&secrets);

    assert!(matches!(result, Err(StorageError::KeyMissing)));
}

#[tokio::test]
async fn unavailable_keychain_for_existing_database_is_an_error() {
    let (_dir, storage) = storage();
    let secrets = MemorySecretStore::default();
    create_with_keychain(&storage, &secrets).await;
    secrets.set_available(false);

    let result = storage.find_key(&secrets);

    assert!(matches!(result, Err(StorageError::KeychainUnavailable(_))));
}

#[tokio::test]
async fn wrong_keychain_key_fails_to_open() {
    let (_dir, storage) = storage();
    let secrets = MemorySecretStore::default();
    create_with_keychain(&storage, &secrets).await;
    secrets.put_secret(&[9; 32]);

    let key = found(storage.find_key(&secrets).unwrap());

    assert!(matches!(
        storage.open(&key).await,
        Err(StorageError::WrongKey)
    ));
}

#[tokio::test]
async fn no_keychain_on_first_run_asks_for_new_passphrase() {
    let (_dir, storage) = storage();
    let secrets = MemorySecretStore::unavailable();

    let lookup = storage.find_key(&secrets).unwrap();

    assert!(matches!(lookup, KeyLookup::NeedsNewPassphrase));
    assert!(!storage.paths().database.exists());
}

#[tokio::test]
async fn passphrase_database_round_trip() {
    let (_dir, storage) = storage();
    let secrets = MemorySecretStore::unavailable();
    let passphrase = Passphrase::new("correct horse battery staple");

    let key = storage.new_passphrase_key(&passphrase).unwrap();
    storage.open(&key).await.unwrap().close().await;

    // The key file marks passphrase mode, even once a keychain appears.
    secrets.set_available(true);
    assert!(matches!(
        storage.find_key(&secrets).unwrap(),
        KeyLookup::NeedsPassphrase
    ));
    let key = storage.key_from_passphrase(&passphrase).unwrap();
    storage.open(&key).await.unwrap().close().await;
    assert_eq!(secrets.secret(), None);
}

#[tokio::test]
async fn wrong_passphrase_fails_to_open() {
    let (_dir, storage) = storage();
    let key = storage
        .new_passphrase_key(&Passphrase::new("right"))
        .unwrap();
    storage.open(&key).await.unwrap().close().await;

    let key = storage
        .key_from_passphrase(&Passphrase::new("wrong"))
        .unwrap();

    assert!(matches!(
        storage.open(&key).await,
        Err(StorageError::WrongKey)
    ));
}

#[tokio::test]
async fn new_passphrase_refuses_existing_database() {
    let (_dir, storage) = storage();
    create_with_keychain(&storage, &MemorySecretStore::default()).await;

    let result = storage.new_passphrase_key(&Passphrase::new("late"));

    assert!(matches!(result, Err(StorageError::DatabaseExists)));
    assert!(!storage.paths().key_file.exists());
}

#[tokio::test]
async fn corrupt_key_file_is_an_error() {
    let (_dir, storage) = storage();
    storage.new_passphrase_key(&Passphrase::new("x")).unwrap();
    std::fs::write(&storage.paths().key_file, "garbage").unwrap();

    let result = storage.key_from_passphrase(&Passphrase::new("x"));

    assert!(matches!(result, Err(StorageError::KeyFile(_))));
}

#[tokio::test]
async fn wipe_deletes_the_database_its_key_and_attachments() {
    let (_dir, storage) = storage();
    let secrets = MemorySecretStore::default();
    create_with_keychain(&storage, &secrets).await;
    let attachments = storage.paths().attachments();
    std::fs::create_dir_all(&attachments).unwrap();
    std::fs::write(attachments.join("0123abcd"), "encrypted").unwrap();

    assert_eq!(storage.wipe(&secrets).unwrap(), Wiped::All);
    assert!(!storage.paths().database.exists());
    assert!(!attachments.exists());
    assert_eq!(secrets.secret(), None);
}

#[tokio::test]
async fn wipe_reports_a_key_left_in_an_unreachable_keychain() {
    let (_dir, storage) = storage();
    let secrets = MemorySecretStore::default();
    create_with_keychain(&storage, &secrets).await;
    secrets.set_available(false);

    assert_eq!(
        storage.wipe(&secrets).unwrap(),
        Wiped::KeyLeft("no keychain".into())
    );
    assert!(!storage.paths().database.exists());
    assert!(secrets.secret().is_some());
}

#[tokio::test]
async fn wiping_a_passphrase_database_needs_no_keychain() {
    let (_dir, storage) = storage();
    let secrets = MemorySecretStore::unavailable();
    let key = storage.new_passphrase_key(&Passphrase::new("x")).unwrap();
    storage.open(&key).await.unwrap().close().await;

    assert_eq!(storage.wipe(&secrets).unwrap(), Wiped::All);
    assert!(!storage.paths().database.exists());
    assert!(!storage.paths().key_file.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn data_dir_and_key_file_are_private() {
    use std::os::unix::fs::PermissionsExt as _;

    let (_dir, storage) = storage();
    storage.new_passphrase_key(&Passphrase::new("x")).unwrap();

    let mode =
        |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    let data_dir = storage.paths().database.parent().unwrap();
    assert_eq!(mode(data_dir), 0o700);
    assert_eq!(mode(&storage.paths().key_file), 0o600);
}
