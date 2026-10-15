//! Receipts, reads on other devices, typing lookups and account settings.

use super::StorageError;
use super::database::{Database, database_error};
use super::messages::millis;
use crate::model::{
    AccountSettings, ConversationId, GroupId, Message, MessageId, ReceiptKind, ServiceId, Timestamp,
};

const READ_RECEIPTS: &str = "read_receipts";
const TYPING_INDICATORS: &str = "typing_indicators";

impl Database {
    /// Applies `sender`'s receipt for our messages sent at `timestamps`;
    /// returns the messages whose status advanced.
    pub(crate) async fn apply_receipt(
        &self,
        sender: ServiceId,
        kind: ReceiptKind,
        timestamps: &[Timestamp],
    ) -> Result<Vec<Message>, StorageError> {
        let sender_bytes = sender.uuid().as_bytes().to_vec();
        let read = kind == ReceiptKind::Read;
        let mut changed = Vec::new();
        for sent_at in timestamps {
            let mut tx = self.app.begin().await.map_err(database_error)?;
            // Sent messages to `sender`, or to a group they're in.
            let targets: Vec<(i64, i64, bool)> = sqlx::query_as(
                "SELECT m.id, m.conversation_id, c.group_id IS NOT NULL
                 FROM messages m JOIN conversations c ON c.id = m.conversation_id
                 WHERE m.outgoing = 1 AND m.status = 'sent' AND m.sent_at = ?1
                   AND (c.peer = ?2 OR EXISTS (
                       SELECT 1 FROM group_members g
                       WHERE g.conversation_id = c.id AND g.member = ?2))",
            )
            .bind(millis(*sent_at))
            .bind(&sender_bytes)
            .fetch_all(&mut *tx)
            .await
            .map_err(database_error)?;
            let mut advanced = Vec::new();
            for (id, conversation, group) in targets {
                let delivery = if group {
                    sqlx::query(
                        "INSERT INTO receipts (message_id, recipient, read) VALUES (?, ?, ?)
                         ON CONFLICT DO UPDATE SET read = max(read, excluded.read)",
                    )
                    .bind(id)
                    .bind(&sender_bytes)
                    .bind(read)
                    .execute(&mut *tx)
                    .await
                    .map_err(database_error)?;
                    // Delivered or read once every member has.
                    let (members, delivered, read): (i64, i64, i64) = sqlx::query_as(
                        "SELECT
                             (SELECT count(*) FROM group_members WHERE conversation_id = ?1),
                             (SELECT count(*) FROM receipts WHERE message_id = ?2),
                             (SELECT count(*) FROM receipts WHERE message_id = ?2 AND read = 1)",
                    )
                    .bind(conversation)
                    .bind(id)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(database_error)?;
                    if members > 0 && read >= members {
                        Some("read")
                    } else if members > 0 && delivered >= members {
                        Some("delivered")
                    } else {
                        None
                    }
                } else if read {
                    Some("read")
                } else {
                    Some("delivered")
                };
                let Some(delivery) = delivery else {
                    continue;
                };
                // Only ever forwards: delivered, then read.
                let updated = sqlx::query(
                    "UPDATE messages SET delivery = ?1 WHERE id = ?2
                     AND (delivery IS NULL OR (delivery = 'delivered' AND ?1 = 'read'))",
                )
                .bind(delivery)
                .bind(id)
                .execute(&mut *tx)
                .await
                .map_err(database_error)?;
                if updated.rows_affected() > 0 {
                    advanced.push(MessageId::new(id));
                }
            }
            tx.commit().await.map_err(database_error)?;
            for id in advanced {
                changed.extend(self.message(id).await?);
            }
        }
        Ok(changed)
    }

    /// Marks messages read on another device read here, with everything
    /// before them in their conversation; returns the conversations that
    /// changed.
    pub(crate) async fn mark_read_elsewhere(
        &self,
        read: &[(ServiceId, Timestamp)],
    ) -> Result<Vec<ConversationId>, StorageError> {
        let mut changed = Vec::new();
        for (author, sent_at) in read {
            let found: Option<(i64, i64)> = sqlx::query_as(
                "SELECT conversation_id, sent_at FROM messages WHERE author = ? AND sent_at = ?",
            )
            .bind(author.uuid().as_bytes().as_slice())
            .bind(millis(*sent_at))
            .fetch_optional(&self.app)
            .await
            .map_err(database_error)?;
            let Some((conversation, sent_at)) = found else {
                continue;
            };
            let result = sqlx::query(
                "UPDATE messages SET read = 1
                 WHERE conversation_id = ? AND read = 0 AND sent_at <= ?",
            )
            .bind(conversation)
            .bind(sent_at)
            .execute(&self.app)
            .await
            .map_err(database_error)?;
            let conversation = ConversationId::new(conversation);
            if result.rows_affected() > 0 && !changed.contains(&conversation) {
                changed.push(conversation);
            }
        }
        Ok(changed)
    }

    /// The existing conversation someone types in: the group, or the 1:1
    /// with them.
    pub(crate) async fn typing_conversation(
        &self,
        sender: ServiceId,
        group: Option<GroupId>,
    ) -> Result<Option<ConversationId>, StorageError> {
        let id: Option<i64> = match group {
            Some(group) => {
                sqlx::query_scalar(
                    "SELECT c.id FROM conversations c
                     WHERE c.group_id = ? AND EXISTS (
                         SELECT 1 FROM group_members g
                         WHERE g.conversation_id = c.id AND g.member = ?)",
                )
                .bind(group.as_bytes().as_slice())
                .bind(sender.uuid().as_bytes().as_slice())
                .fetch_optional(&self.app)
                .await
            }
            None => {
                sqlx::query_scalar("SELECT id FROM conversations WHERE peer = ?")
                    .bind(sender.uuid().as_bytes().as_slice())
                    .fetch_optional(&self.app)
                    .await
            }
        }
        .map_err(database_error)?;
        Ok(id.map(ConversationId::new))
    }

    /// The account's settings; `None` until the phone has sent them.
    pub(crate) async fn settings(&self) -> Result<Option<AccountSettings>, StorageError> {
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT key, value FROM app_meta WHERE key IN (?, ?)")
                .bind(READ_RECEIPTS)
                .bind(TYPING_INDICATORS)
                .fetch_all(&self.app)
                .await
                .map_err(database_error)?;
        if rows.is_empty() {
            return Ok(None);
        }
        let enabled = |key: &str| rows.iter().any(|(k, value)| k == key && value == "1");
        Ok(Some(AccountSettings {
            read_receipts: enabled(READ_RECEIPTS),
            typing_indicators: enabled(TYPING_INDICATORS),
        }))
    }

    /// Stores the settings that are `Some`; returns them all, off unless
    /// known to be on.
    pub(crate) async fn update_settings(
        &self,
        read_receipts: Option<bool>,
        typing_indicators: Option<bool>,
    ) -> Result<AccountSettings, StorageError> {
        for (key, value) in [
            (READ_RECEIPTS, read_receipts),
            (TYPING_INDICATORS, typing_indicators),
        ] {
            let Some(value) = value else {
                continue;
            };
            sqlx::query(
                "INSERT INTO app_meta (key, value) VALUES (?, ?)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            )
            .bind(key)
            .bind(if value { "1" } else { "0" })
            .execute(&self.app)
            .await
            .map_err(database_error)?;
        }
        Ok(self.settings().await?.unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;
    use uuid::Uuid;

    use super::*;
    use crate::model::{
        Content, Destination, GroupInfo, GroupMasterKey, GroupRef, Member, MessageBody,
        MessageStatus,
    };
    use crate::storage::DatabaseKey;

    async fn db() -> (TempDir, Database) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("test.db"), &DatabaseKey::generate())
            .await
            .unwrap();
        (dir, db)
    }

    fn peer(n: u128) -> ServiceId {
        ServiceId::new(Uuid::from_u128(n))
    }

    fn at(millis: u64) -> Timestamp {
        Timestamp::from_millis(millis)
    }

    /// A sent message of ours, at `sent_at`.
    async fn sent(db: &Database, destination: Destination, sent_at: u64) -> Message {
        db.add_message(&destination, None, at(sent_at), &Content::text("out"))
            .await
            .unwrap()
            .unwrap()
    }

    fn statuses(messages: &[Message]) -> Vec<Option<MessageStatus>> {
        messages.iter().map(|message| message.status).collect()
    }

    #[tokio::test]
    async fn one_to_one_receipts_advance_the_status() {
        let (_dir, db) = db().await;
        let message = sent(&db, Destination::Contact(peer(1)), 100).await;
        assert_eq!(message.status, Some(MessageStatus::Sent));

        let delivered = db
            .apply_receipt(peer(1), ReceiptKind::Delivered, &[at(100)])
            .await
            .unwrap();
        assert_eq!(statuses(&delivered), [Some(MessageStatus::Delivered)]);
        let read = db
            .apply_receipt(peer(1), ReceiptKind::Read, &[at(100)])
            .await
            .unwrap();
        assert_eq!(statuses(&read), [Some(MessageStatus::Read)]);
        // Never back, and only from the recipient.
        let late = db
            .apply_receipt(peer(1), ReceiptKind::Delivered, &[at(100)])
            .await
            .unwrap();
        assert!(late.is_empty());
        let stranger = sent(&db, Destination::Contact(peer(1)), 101).await;
        let wrong = db
            .apply_receipt(peer(2), ReceiptKind::Read, &[stranger.sent_at])
            .await
            .unwrap();
        assert!(wrong.is_empty());
        let loaded = db.messages(message.conversation, None, 10).await.unwrap();
        assert_eq!(loaded[0].status, Some(MessageStatus::Read));
        db.close().await;
    }

    #[tokio::test]
    async fn group_receipts_count_once_every_member_has_them() {
        let (_dir, db) = db().await;
        let group = GroupRef {
            key: GroupMasterKey::new([1; 32]),
            revision: 1,
        };
        let message = sent(&db, Destination::Group(group), 100).await;
        let members = vec![
            Member {
                id: peer(1),
                name: None,
            },
            Member {
                id: peer(2),
                name: None,
            },
        ];
        db.set_group(
            group.key,
            &GroupInfo {
                title: "Dev".into(),
                revision: 1,
                members,
            },
        )
        .await
        .unwrap();
        let sent_at = [at(100)];
        let receipt = |from, kind| db.apply_receipt(peer(from), kind, &sent_at);

        assert!(receipt(1, ReceiptKind::Delivered).await.unwrap().is_empty());
        assert!(
            receipt(3, ReceiptKind::Delivered).await.unwrap().is_empty(),
            "not a member"
        );
        let delivered = receipt(2, ReceiptKind::Read).await.unwrap();
        assert_eq!(statuses(&delivered), [Some(MessageStatus::Delivered)]);
        let read = receipt(1, ReceiptKind::Read).await.unwrap();
        assert_eq!(statuses(&read), [Some(MessageStatus::Read)]);
        assert_eq!(read[0].id, message.id);
        db.close().await;
    }

    #[tokio::test]
    async fn pending_messages_ignore_receipts() {
        let (_dir, db) = db().await;
        let conversation = db
            .open_conversation(&Destination::Contact(peer(1)))
            .await
            .unwrap();
        db.add_pending(conversation, at(100), &MessageBody::new("x"))
            .await
            .unwrap();

        let changed = db
            .apply_receipt(peer(1), ReceiptKind::Read, &[at(100)])
            .await
            .unwrap();

        assert!(changed.is_empty());
        db.close().await;
    }

    async fn incoming(db: &Database, from: u128, sent_at: u64) -> Message {
        db.add_message(
            &Destination::Contact(peer(from)),
            Some(peer(from)),
            at(sent_at),
            &Content::text("in"),
        )
        .await
        .unwrap()
        .unwrap()
    }

    #[tokio::test]
    async fn mark_read_returns_what_it_marked() {
        let (_dir, db) = db().await;
        let first = incoming(&db, 1, 100).await;
        incoming(&db, 1, 101).await;

        let read = db.mark_read(first.conversation).await.unwrap();

        assert_eq!(read, [(peer(1), at(100)), (peer(1), at(101))]);
        assert!(db.mark_read(first.conversation).await.unwrap().is_empty());
        db.close().await;
    }

    #[tokio::test]
    async fn reads_elsewhere_mark_earlier_messages_too() {
        let (_dir, db) = db().await;
        let first = incoming(&db, 1, 100).await;
        incoming(&db, 1, 101).await;
        incoming(&db, 1, 102).await;

        let changed = db
            .mark_read_elsewhere(&[(peer(1), at(101)), (peer(9), at(5))])
            .await
            .unwrap();

        assert_eq!(changed, [first.conversation]);
        let summary = db.conversation(first.conversation).await.unwrap().unwrap();
        assert_eq!(summary.unread, 1);
        db.close().await;
    }

    #[tokio::test]
    async fn typing_finds_only_existing_conversations() {
        let (_dir, db) = db().await;
        let chat = incoming(&db, 1, 100).await.conversation;
        let key = GroupMasterKey::new([1; 32]);
        let group = sent(&db, Destination::Group(GroupRef { key, revision: 1 }), 100)
            .await
            .conversation;
        db.set_group(
            key,
            &GroupInfo {
                title: "Dev".into(),
                revision: 1,
                members: vec![Member {
                    id: peer(2),
                    name: None,
                }],
            },
        )
        .await
        .unwrap();

        let find = |sender, group| db.typing_conversation(peer(sender), group);
        assert_eq!(find(1, None).await.unwrap(), Some(chat));
        assert_eq!(find(3, None).await.unwrap(), None);
        assert_eq!(find(2, Some(key.id())).await.unwrap(), Some(group));
        assert_eq!(find(3, Some(key.id())).await.unwrap(), None, "not a member");
        db.close().await;
    }

    #[tokio::test]
    async fn settings_are_unknown_until_synced_then_merge() {
        let (_dir, db) = db().await;
        assert_eq!(db.settings().await.unwrap(), None);

        let settings = db.update_settings(Some(true), None).await.unwrap();
        assert_eq!(
            settings,
            AccountSettings {
                read_receipts: true,
                typing_indicators: false,
            }
        );
        let settings = db.update_settings(None, Some(true)).await.unwrap();
        assert!(settings.read_receipts && settings.typing_indicators);
        db.update_settings(Some(false), None).await.unwrap();
        assert_eq!(
            db.settings().await.unwrap(),
            Some(AccountSettings {
                read_receipts: false,
                typing_indicators: true,
            })
        );
        db.close().await;
    }
}
