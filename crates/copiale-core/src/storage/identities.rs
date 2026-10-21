//! Identity keys verified on the phone or in Storage Service, and lines
//! noting changed safety numbers.

use sqlx::{SqliteConnection, SqlitePool};
use uuid::Uuid;

use super::StorageError;
use super::database::{Database, database_error};
use super::messages::{insert_message, service_id};
use crate::model::{Content, IdentityKey, Message, ServiceId, StoredIdentity, Timestamp};

/// Reads verified keys, for the protocol store; clones share the pool.
#[derive(Clone)]
pub(crate) struct VerifiedKeys(SqlitePool);

impl VerifiedKeys {
    /// The key verified for the account with this ACI.
    pub(crate) async fn get(&self, peer: Uuid) -> Result<Option<IdentityKey>, StorageError> {
        let key: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT identity_key FROM verified_identities WHERE peer = ?")
                .bind(peer.as_bytes().as_slice())
                .fetch_optional(&self.0)
                .await
                .map_err(database_error)?;
        Ok(key.map(IdentityKey::new))
    }
}

impl Database {
    pub(crate) fn verified_keys(&self) -> VerifiedKeys {
        VerifiedKeys(self.app.clone())
    }

    /// The key verified for `peer`.
    pub(crate) async fn verified_key(
        &self,
        peer: ServiceId,
    ) -> Result<Option<IdentityKey>, StorageError> {
        self.verified_keys().get(peer.uuid()).await
    }

    /// Verified keys, by account.
    pub(crate) async fn verified(&self) -> Result<Vec<(ServiceId, IdentityKey)>, StorageError> {
        let rows: Vec<(Vec<u8>, Vec<u8>)> =
            sqlx::query_as("SELECT peer, identity_key FROM verified_identities ORDER BY peer")
                .fetch_all(&self.app)
                .await
                .map_err(database_error)?;
        rows.into_iter()
            .map(|(peer, key)| Ok((service_id(&peer)?, IdentityKey::new(key))))
            .collect()
    }

    /// Marks `key` verified for `peer`, or `peer` unverified (`None`);
    /// `false` if that's how it was.
    pub(crate) async fn set_verified(
        &self,
        peer: ServiceId,
        key: Option<&IdentityKey>,
    ) -> Result<bool, StorageError> {
        let mut conn = self.app.acquire().await.map_err(database_error)?;
        set_verified(&mut conn, peer, key).await
    }

    /// Notes that `peer`'s safety number changed, at `now`, in their 1:1
    /// conversation, if any, and each group they're in; returns the lines.
    pub(crate) async fn add_safety_number_changed(
        &self,
        peer: ServiceId,
        now: Timestamp,
    ) -> Result<Vec<Message>, StorageError> {
        let mut tx = self.begin_write().await?;
        let conversations: Vec<i64> = sqlx::query_scalar(
            "SELECT id FROM conversations WHERE peer = ?1
             UNION SELECT conversation_id FROM group_members WHERE member = ?1",
        )
        .bind(peer.uuid().as_bytes().as_slice())
        .fetch_all(&mut *tx)
        .await
        .map_err(database_error)?;
        let mut added = Vec::new();
        for conversation in conversations {
            let line = insert_message(
                &mut tx,
                conversation,
                Some(peer),
                None,
                now,
                &Content::SafetyNumberChanged,
                None,
                None,
                now,
            )
            .await?;
            added.extend(line);
        }
        tx.commit().await.map_err(database_error)?;
        Ok(added)
    }
}

/// [`Database::set_verified`] on `conn`.
async fn set_verified(
    conn: &mut SqliteConnection,
    peer: ServiceId,
    key: Option<&IdentityKey>,
) -> Result<bool, StorageError> {
    let peer = peer.uuid().as_bytes().to_vec();
    let result = match key {
        Some(key) => {
            sqlx::query(
                "INSERT INTO verified_identities (peer, identity_key) VALUES (?1, ?2)
                 ON CONFLICT (peer) DO UPDATE SET identity_key = ?2
                 WHERE identity_key != ?2",
            )
            .bind(peer)
            .bind(key.as_bytes())
            .execute(&mut *conn)
            .await
        }
        None => {
            sqlx::query("DELETE FROM verified_identities WHERE peer = ?")
                .bind(peer)
                .execute(&mut *conn)
                .await
        }
    };
    Ok(result.map_err(database_error)?.rows_affected() > 0)
}

/// Applies a Storage Service verification unless it's as last read;
/// `true` if `peer`'s verified key changed.
pub(super) async fn apply_stored_identity(
    conn: &mut SqliteConnection,
    stored: &StoredIdentity,
) -> Result<bool, StorageError> {
    let peer = stored.peer.uuid().as_bytes().to_vec();
    let result = sqlx::query(
        "INSERT INTO stored_identities (peer, identity_key, verified) VALUES (?1, ?2, ?3)
         ON CONFLICT (peer) DO UPDATE SET identity_key = ?2, verified = ?3
         WHERE identity_key != ?2 OR verified != ?3",
    )
    .bind(peer)
    .bind(stored.key.as_bytes())
    .bind(stored.verified)
    .execute(&mut *conn)
    .await
    .map_err(database_error)?;
    if result.rows_affected() == 0 {
        return Ok(false);
    }
    set_verified(conn, stored.peer, stored.verified.then_some(&stored.key)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Destination, GroupInfo, GroupMasterKey, GroupRef, Member};
    use crate::storage::testing::{at, db, peer};

    fn key(n: u8) -> IdentityKey {
        IdentityKey::new(vec![5, n])
    }

    #[tokio::test]
    async fn verified_keys_are_set_replaced_and_cleared() {
        let (_dir, db) = db().await;

        assert!(db.set_verified(peer(2), Some(&key(1))).await.unwrap());
        assert!(db.set_verified(peer(1), Some(&key(1))).await.unwrap());
        assert!(!db.set_verified(peer(1), Some(&key(1))).await.unwrap());
        assert!(db.set_verified(peer(1), Some(&key(2))).await.unwrap());
        assert_eq!(db.verified_key(peer(1)).await.unwrap(), Some(key(2)));
        assert_eq!(
            db.verified().await.unwrap(),
            [(peer(1), key(2)), (peer(2), key(1))]
        );

        assert!(db.set_verified(peer(1), None).await.unwrap());
        assert!(!db.set_verified(peer(1), None).await.unwrap());
        assert_eq!(db.verified_key(peer(1)).await.unwrap(), None);
        assert_eq!(
            db.verified_keys().get(peer(2).uuid()).await.unwrap(),
            Some(key(1))
        );
        db.close().await;
    }

    #[tokio::test]
    async fn stored_verifications_apply_when_they_change() {
        let (_dir, db) = db().await;
        let stored = |n, k, verified| StoredIdentity {
            peer: peer(n),
            key: key(k),
            verified,
        };
        let apply = async |identities: &[StoredIdentity]| {
            db.apply_stored(&[], identities, 1).await.unwrap().1
        };

        assert_eq!(
            apply(&[stored(1, 1, true), stored(2, 1, false)]).await,
            [peer(1)]
        );
        assert_eq!(db.verified_key(peer(1)).await.unwrap(), Some(key(1)));

        db.set_verified(peer(1), None).await.unwrap();
        assert!(
            apply(&[stored(1, 1, true)]).await.is_empty(),
            "as last read"
        );
        assert_eq!(db.verified_key(peer(1)).await.unwrap(), None);

        assert_eq!(apply(&[stored(1, 2, true)]).await, [peer(1)]);
        assert_eq!(db.verified_key(peer(1)).await.unwrap(), Some(key(2)));
        assert_eq!(apply(&[stored(1, 2, false)]).await, [peer(1)]);
        assert_eq!(db.verified_key(peer(1)).await.unwrap(), None);

        db.set_verified(peer(2), Some(&key(3))).await.unwrap();
        assert!(
            apply(&[stored(2, 1, false)]).await.is_empty(),
            "as last read"
        );
        assert_eq!(db.verified_key(peer(2)).await.unwrap(), Some(key(3)));
        db.close().await;
    }

    #[tokio::test]
    async fn changed_safety_numbers_are_noted_in_every_shared_chat() {
        let (_dir, db) = db().await;
        let with = |n: u8| GroupRef {
            key: GroupMasterKey::new([n; 32]),
            revision: 1,
        };
        let one_to_one = db
            .open_conversation(&Destination::Contact(peer(1)))
            .await
            .unwrap();
        db.open_conversation(&Destination::Contact(peer(2)))
            .await
            .unwrap();
        for (group, members) in [(1, vec![1, 2]), (2, vec![2])] {
            let info = GroupInfo {
                title: format!("group {group}"),
                revision: 1,
                members: members
                    .into_iter()
                    .map(|n| Member {
                        id: peer(n),
                        name: None,
                    })
                    .collect(),
            };
            db.open_conversation(&Destination::Group(with(group)))
                .await
                .unwrap();
            db.set_group(with(group).key, &info).await.unwrap();
        }
        let shared_group = db
            .open_conversation(&Destination::Group(with(1)))
            .await
            .unwrap();

        let lines = db
            .add_safety_number_changed(peer(1), at(500))
            .await
            .unwrap();

        let mut conversations: Vec<_> = lines.iter().map(|line| line.conversation).collect();
        conversations.sort();
        assert_eq!(conversations, [one_to_one, shared_group]);
        for line in &lines {
            assert_eq!(line.content, Content::SafetyNumberChanged);
            assert_eq!(line.sender, Some(peer(1)));
            assert_eq!(line.sent_at, at(500));
            assert!(line.read, "system lines arrive read");
        }
        assert!(
            db.add_safety_number_changed(peer(3), at(500))
                .await
                .unwrap()
                .is_empty()
        );
        db.close().await;
    }
}
