//! Emoji picked lately, for the emoji picker.

use super::StorageError;
use super::database::{Database, database_error};
use crate::actor::MAX_RECENT_EMOJI;
use crate::model::RecentEmoji;

const RECENT_EMOJI: &str = "recent_emoji";

/// Longest emoji kept, in bytes; tag flags take 28.
const MAX_EMOJI_LEN: usize = 64;

impl Database {
    pub(crate) async fn recent_emoji(&self) -> Result<RecentEmoji, StorageError> {
        let value: Option<String> = sqlx::query_scalar("SELECT value FROM app_meta WHERE key = ?")
            .bind(RECENT_EMOJI)
            .fetch_optional(&self.app)
            .await
            .map_err(database_error)?;
        Ok(RecentEmoji(
            value
                .iter()
                .flat_map(|value| value.lines())
                .map(str::to_owned)
                .collect(),
        ))
    }

    /// Keeps the first [`MAX_RECENT_EMOJI`], leaving out empty, long or
    /// multi-line entries.
    pub(crate) async fn set_recent_emoji(&self, recent: &RecentEmoji) -> Result<(), StorageError> {
        let value = recent
            .0
            .iter()
            .filter(|emoji| !emoji.is_empty() && emoji.len() <= MAX_EMOJI_LEN)
            .filter(|emoji| !emoji.contains(['\n', '\r']))
            .take(MAX_RECENT_EMOJI)
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n");
        sqlx::query(
            "INSERT INTO app_meta (key, value) VALUES (?, ?)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        )
        .bind(RECENT_EMOJI)
        .bind(value)
        .execute(&self.app)
        .await
        .map_err(database_error)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::testing::db;

    #[tokio::test]
    async fn recent_emoji_round_trip_bounded() {
        let (_dir, db) = db().await;
        assert_eq!(db.recent_emoji().await.unwrap(), RecentEmoji::default());

        let mut recent = vec!["👍".to_owned(), "🏳️‍🌈".into(), String::new(), "a\nb".into()];
        recent.push("x".repeat(MAX_EMOJI_LEN + 1));
        recent.extend((0..MAX_RECENT_EMOJI).map(|ix| ix.to_string()));
        db.set_recent_emoji(&RecentEmoji(recent)).await.unwrap();
        let stored = db.recent_emoji().await.unwrap().0;
        assert_eq!(stored.len(), MAX_RECENT_EMOJI);
        assert_eq!(stored[..3], ["👍", "🏳️‍🌈", "0"]);

        db.set_recent_emoji(&RecentEmoji(vec!["❤️".into()]))
            .await
            .unwrap();
        assert_eq!(db.recent_emoji().await.unwrap().0, ["❤️"]);
        db.close().await;
    }
}
