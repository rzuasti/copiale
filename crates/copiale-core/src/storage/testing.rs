//! Helpers shared by the storage tests.

use tempfile::TempDir;
use uuid::Uuid;

use super::messages::Removed;
use super::{Database, DatabaseKey};
use crate::model::{Message, ServiceId, Timestamp};

/// A fresh database in a temporary directory, kept while the dir lives.
pub async fn db() -> (TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&dir.path().join("test.db"), &DatabaseKey::generate())
        .await
        .unwrap();
    (dir, db)
}

pub fn peer(n: u128) -> ServiceId {
    ServiceId::new(Uuid::from_u128(n))
}

pub fn at(millis: u64) -> Timestamp {
    Timestamp::from_millis(millis)
}

/// How storage reports `message` deleted.
pub fn removed_of(message: &Message) -> Removed {
    Removed {
        conversation: message.conversation,
        id: message.id,
        author: message.sender,
        sent_at: message.sent_at,
    }
}
