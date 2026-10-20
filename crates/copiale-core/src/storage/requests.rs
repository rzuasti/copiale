//! Which accounts and groups the user accepted or blocked.

use std::collections::HashSet;

use super::StorageError;
use super::database::{Database, database_error};
use super::messages::service_id;
use crate::model::{BlockedList, ChatRef, GroupId, PhoneNumber, StoredChat};

const STORAGE_VERSION: &str = "storage_version";

impl Database {
    /// Whether `chat` is blocked.
    pub(crate) async fn is_blocked(&self, chat: ChatRef) -> Result<bool, StorageError> {
        let blocked: Option<bool> =
            sqlx::query_scalar("SELECT blocked FROM chat_states WHERE chat = ?")
                .bind(chat_key(chat))
                .fetch_optional(&self.app)
                .await
                .map_err(database_error)?;
        Ok(blocked.unwrap_or(false))
    }

    /// Whether `chat` is accepted and not blocked.
    pub(crate) async fn is_accepted(&self, chat: ChatRef) -> Result<bool, StorageError> {
        let accepted: Option<bool> =
            sqlx::query_scalar("SELECT accepted AND NOT blocked FROM chat_states WHERE chat = ?")
                .bind(chat_key(chat))
                .fetch_optional(&self.app)
                .await
                .map_err(database_error)?;
        Ok(accepted.unwrap_or(false))
    }

    /// Accepts `chat`, unblocking it; `false` if it was already accepted
    /// and not blocked.
    pub(crate) async fn accept(&self, chat: ChatRef) -> Result<bool, StorageError> {
        let result = sqlx::query(
            "INSERT INTO chat_states (chat, accepted) VALUES (?, 1)
             ON CONFLICT (chat) DO UPDATE SET accepted = 1, blocked = 0
             WHERE accepted = 0 OR blocked = 1",
        )
        .bind(chat_key(chat))
        .execute(&self.app)
        .await
        .map_err(database_error)?;
        Ok(result.rows_affected() > 0)
    }

    /// Accepts `chat`, leaving it blocked if it is; `false` if it was
    /// already accepted.
    pub(crate) async fn mark_accepted(&self, chat: ChatRef) -> Result<bool, StorageError> {
        let mut conn = self.app.acquire().await.map_err(database_error)?;
        mark_accepted(&mut conn, chat).await
    }

    /// Blocks or unblocks `chat`; `false` if that's how it was.
    pub(crate) async fn set_blocked(
        &self,
        chat: ChatRef,
        blocked: bool,
    ) -> Result<bool, StorageError> {
        let mut tx = self.begin_write().await?;
        let changed = set_blocked(&mut tx, chat, blocked).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(changed)
    }

    /// Blocked accounts, groups and numbers.
    pub(crate) async fn blocked(&self) -> Result<BlockedList, StorageError> {
        let chats: Vec<Vec<u8>> =
            sqlx::query_scalar("SELECT chat FROM chat_states WHERE blocked = 1 ORDER BY chat")
                .fetch_all(&self.app)
                .await
                .map_err(database_error)?;
        let mut list = BlockedList::default();
        for chat in chats {
            match <[u8; 32]>::try_from(chat.as_slice()) {
                Ok(group) => list.groups.push(GroupId::new(group)),
                Err(_) => list.accounts.push(service_id(&chat)?),
            }
        }
        let numbers: Vec<String> =
            sqlx::query_scalar("SELECT number FROM blocked_numbers ORDER BY number")
                .fetch_all(&self.app)
                .await
                .map_err(database_error)?;
        list.numbers = numbers.into_iter().map(PhoneNumber::new).collect();
        Ok(list)
    }

    /// Replaces the blocked list with the phone's; returns the chats whose
    /// state changed.
    pub(crate) async fn set_blocked_list(
        &self,
        list: &BlockedList,
    ) -> Result<Vec<ChatRef>, StorageError> {
        let current = self.blocked().await?;
        let chats = |list: &BlockedList| -> Vec<ChatRef> {
            let accounts = list.accounts.iter().copied().map(ChatRef::Contact);
            let groups = list.groups.iter().copied().map(ChatRef::Group);
            accounts.chain(groups).collect()
        };
        let wanted: HashSet<_> = chats(list).into_iter().collect();
        let mut tx = self.begin_write().await?;
        let mut changed = Vec::new();
        for chat in chats(&current) {
            if !wanted.contains(&chat) && set_blocked(&mut tx, chat, false).await? {
                changed.push(chat);
            }
        }
        for chat in chats(list) {
            if set_blocked(&mut tx, chat, true).await? {
                changed.push(chat);
            }
        }
        sqlx::query("DELETE FROM blocked_numbers")
            .execute(&mut *tx)
            .await
            .map_err(database_error)?;
        for number in &list.numbers {
            sqlx::query("INSERT OR IGNORE INTO blocked_numbers (number) VALUES (?)")
                .bind(number.as_str())
                .execute(&mut *tx)
                .await
                .map_err(database_error)?;
        }
        tx.commit().await.map_err(database_error)?;
        Ok(changed)
    }

    /// Applies Storage Service's chats, at manifest `version`: accepted ones
    /// are accepted here too, and blocking follows it. Returns the chats
    /// whose state changed.
    pub(crate) async fn apply_stored_chats(
        &self,
        chats: &[StoredChat],
        version: u64,
    ) -> Result<Vec<ChatRef>, StorageError> {
        let mut tx = self.begin_write().await?;
        let mut changed = Vec::new();
        for stored in chats {
            let mut chat_changed = set_blocked(&mut tx, stored.chat, stored.blocked).await?;
            if stored.accepted {
                chat_changed |= mark_accepted(&mut tx, stored.chat).await?;
            }
            if chat_changed {
                changed.push(stored.chat);
            }
        }
        sqlx::query(
            "INSERT INTO app_meta (key, value) VALUES (?, ?)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        )
        .bind(STORAGE_VERSION)
        .bind(version.to_string())
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
        tx.commit().await.map_err(database_error)?;
        Ok(changed)
    }

    /// The Storage Service manifest version last applied.
    pub(crate) async fn storage_version(&self) -> Result<Option<u64>, StorageError> {
        let value: Option<String> = sqlx::query_scalar("SELECT value FROM app_meta WHERE key = ?")
            .bind(STORAGE_VERSION)
            .fetch_optional(&self.app)
            .await
            .map_err(database_error)?;
        Ok(value.and_then(|value| value.parse().ok()))
    }
}

/// Accepts `chat`, leaving blocking as it is; `false` if it was already
/// accepted.
async fn mark_accepted(
    conn: &mut sqlx::SqliteConnection,
    chat: ChatRef,
) -> Result<bool, StorageError> {
    let result = sqlx::query(
        "INSERT INTO chat_states (chat, accepted) VALUES (?, 1)
         ON CONFLICT (chat) DO UPDATE SET accepted = 1 WHERE accepted = 0",
    )
    .bind(chat_key(chat))
    .execute(&mut *conn)
    .await
    .map_err(database_error)?;
    Ok(result.rows_affected() > 0)
}

/// Blocks or unblocks `chat`; `false` if that's how it was.
async fn set_blocked(
    tx: &mut sqlx::SqliteConnection,
    chat: ChatRef,
    blocked: bool,
) -> Result<bool, StorageError> {
    let sql = if blocked {
        "INSERT INTO chat_states (chat, blocked) VALUES (?, 1)
         ON CONFLICT (chat) DO UPDATE SET blocked = 1 WHERE blocked = 0"
    } else {
        "UPDATE chat_states SET blocked = 0 WHERE chat = ? AND blocked = 1"
    };
    let result = sqlx::query(sql)
        .bind(chat_key(chat))
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
    Ok(result.rows_affected() > 0)
}

/// `chat_states` key of `chat`: an ACI or a group ID, told apart by length.
fn chat_key(chat: ChatRef) -> Vec<u8> {
    match chat {
        ChatRef::Contact(peer) => peer.uuid().as_bytes().to_vec(),
        ChatRef::Group(group) => group.as_bytes().to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ChatState, Content, Destination, GroupMasterKey, GroupRef};
    use crate::storage::testing::{at, db, peer};

    fn group(n: u8) -> GroupRef {
        GroupRef {
            key: GroupMasterKey::new([n; 32]),
            revision: 1,
        }
    }

    async fn state(db: &Database, destination: Destination) -> ChatState {
        let id = db.open_conversation(&destination).await.unwrap();
        db.conversation(id).await.unwrap().unwrap().state
    }

    async fn add(
        db: &Database,
        destination: Destination,
        author: u128,
        sent_at: u64,
        content: Content,
    ) {
        db.add_message(
            &destination,
            Some(peer(author)),
            at(sent_at),
            &content,
            None,
            Some(at(sent_at)),
            at(0),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn chats_are_requests_once_the_other_side_writes_until_accepted() {
        let (_dir, db) = db().await;
        let alice = Destination::Contact(peer(1));
        let dev = Destination::Group(group(7));
        assert_eq!(state(&db, alice).await, ChatState::Accepted, "empty");
        add(&db, alice, 1, 100, Content::SafetyNumberChanged).await;
        assert_eq!(state(&db, alice).await, ChatState::Accepted);

        add(&db, alice, 1, 101, Content::text("hi")).await;
        add(&db, dev, 2, 102, Content::text("hello all")).await;
        assert_eq!(state(&db, alice).await, ChatState::Request);
        assert_eq!(state(&db, dev).await, ChatState::Request);
        assert!(!db.is_accepted(ChatRef::Contact(peer(1))).await.unwrap());

        assert!(db.accept(ChatRef::Contact(peer(1))).await.unwrap());
        assert!(!db.accept(ChatRef::Contact(peer(1))).await.unwrap());
        assert_eq!(state(&db, alice).await, ChatState::Accepted);
        assert!(db.is_accepted(ChatRef::Contact(peer(1))).await.unwrap());
        assert_eq!(state(&db, dev).await, ChatState::Request);
        db.close().await;
    }

    #[tokio::test]
    async fn blocking_overrides_accepting_and_accepting_unblocks() {
        let (_dir, db) = db().await;
        let chat = ChatRef::Contact(peer(1));
        let alice = Destination::Contact(peer(1));
        assert!(!db.set_blocked(chat, false).await.unwrap(), "not known");
        db.accept(chat).await.unwrap();

        assert!(db.set_blocked(chat, true).await.unwrap());
        assert!(!db.set_blocked(chat, true).await.unwrap());
        assert!(db.is_blocked(chat).await.unwrap());
        assert!(!db.is_accepted(chat).await.unwrap());
        assert_eq!(state(&db, alice).await, ChatState::Blocked);

        assert!(db.accept(chat).await.unwrap());
        assert!(!db.is_blocked(chat).await.unwrap());
        assert_eq!(state(&db, alice).await, ChatState::Accepted);
        db.close().await;
    }

    #[tokio::test]
    async fn the_blocked_list_is_replaced_whole() {
        let (_dir, db) = db().await;
        let dev = GroupMasterKey::new([7; 32]).id();
        db.set_blocked(ChatRef::Contact(peer(1)), true)
            .await
            .unwrap();
        db.set_blocked(ChatRef::Contact(peer(2)), true)
            .await
            .unwrap();

        let list = BlockedList {
            accounts: vec![peer(2), peer(3)],
            groups: vec![dev],
            numbers: vec![PhoneNumber::new("+14155550123")],
        };
        let changed = db.set_blocked_list(&list).await.unwrap();

        assert_eq!(
            changed,
            [
                ChatRef::Contact(peer(1)),
                ChatRef::Contact(peer(3)),
                ChatRef::Group(dev)
            ]
        );
        assert_eq!(db.blocked().await.unwrap(), list);
        assert!(db.set_blocked_list(&list).await.unwrap().is_empty());
        db.close().await;
    }

    #[tokio::test]
    async fn storage_service_accepts_and_sets_blocking() {
        let (_dir, db) = db().await;
        let alice = ChatRef::Contact(peer(1));
        let bob = ChatRef::Contact(peer(2));
        let dev = ChatRef::Group(GroupMasterKey::new([7; 32]).id());
        db.accept(bob).await.unwrap();
        db.set_blocked(dev, true).await.unwrap();
        assert_eq!(db.storage_version().await.unwrap(), None);

        let stored = |chat, accepted, blocked| StoredChat {
            chat,
            accepted,
            blocked,
        };
        let changed = db
            .apply_stored_chats(
                &[
                    stored(alice, true, false),
                    stored(bob, false, false),
                    stored(dev, false, false),
                ],
                5,
            )
            .await
            .unwrap();

        assert_eq!(changed, [alice, dev]);
        assert!(db.is_accepted(alice).await.unwrap());
        assert!(db.is_accepted(bob).await.unwrap(), "not taken back");
        assert!(!db.is_blocked(dev).await.unwrap());
        assert_eq!(db.storage_version().await.unwrap(), Some(5));
        db.close().await;
    }

    #[tokio::test]
    async fn conversations_from_before_requests_stay_accepted() {
        let (_dir, db) = db().await;
        add(
            &db,
            Destination::Contact(peer(1)),
            1,
            100,
            Content::text("hi"),
        )
        .await;
        add(
            &db,
            Destination::Group(group(7)),
            2,
            101,
            Content::text("yo"),
        )
        .await;
        let migration = include_str!("../../migrations/20261025120000_requests.sql");
        sqlx::raw_sql("DROP TABLE blocked_numbers; DROP TABLE chat_states;")
            .execute(db.pool())
            .await
            .unwrap();
        sqlx::raw_sql(migration).execute(db.pool()).await.unwrap();

        let conversations = db.conversations().await.unwrap();
        assert_eq!(conversations.len(), 2);
        for conversation in conversations {
            assert_eq!(conversation.state, ChatState::Accepted);
        }
        db.close().await;
    }
}
