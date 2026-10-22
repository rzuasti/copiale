//! 1:1 disappearing message timers, for presage's store: presage keeps
//! them only for contacts.

use sqlx::SqlitePool;
use uuid::Uuid;

use super::database::Database;

/// Timers by peer, versioned; clones share the pool.
#[derive(Clone)]
pub(crate) struct PeerTimers(SqlitePool);

impl PeerTimers {
    /// `peer`'s timer in seconds (0 if off) and its version.
    pub(crate) async fn get(&self, peer: Uuid) -> Result<Option<(u32, u32)>, sqlx::Error> {
        sqlx::query_as("SELECT timer, version FROM peer_timers WHERE peer = ?")
            .bind(peer.as_bytes().as_slice())
            .fetch_optional(&self.0)
            .await
    }

    /// Sets `peer`'s timer, unless the stored one's version is as new.
    pub(crate) async fn set(
        &self,
        peer: Uuid,
        timer: u32,
        version: u32,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO peer_timers (peer, timer, version) VALUES (?, ?, ?)
             ON CONFLICT (peer) DO UPDATE SET timer = excluded.timer, version = excluded.version
             WHERE excluded.version > peer_timers.version",
        )
        .bind(peer.as_bytes().as_slice())
        .bind(timer)
        .bind(version)
        .execute(&self.0)
        .await?;
        Ok(())
    }
}

impl Database {
    pub(crate) fn peer_timers(&self) -> PeerTimers {
        PeerTimers(self.app.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::testing::db;

    #[tokio::test]
    async fn only_newer_versions_replace_a_timer() {
        let (_dir, db) = db().await;
        let timers = db.peer_timers();
        let peer = Uuid::from_u128(1);
        assert_eq!(timers.get(peer).await.unwrap(), None);

        timers.set(peer, 60, 2).await.unwrap();
        timers.set(peer, 30, 2).await.unwrap();
        timers.set(peer, 10, 1).await.unwrap();
        assert_eq!(timers.get(peer).await.unwrap(), Some((60, 2)));

        timers.set(peer, 0, 3).await.unwrap();
        assert_eq!(timers.get(peer).await.unwrap(), Some((0, 3)));
        assert_eq!(timers.get(Uuid::from_u128(2)).await.unwrap(), None);
        db.close().await;
    }
}
