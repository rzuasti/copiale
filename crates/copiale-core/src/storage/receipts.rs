//! Receipts, reads on other devices, typing lookups and account settings.

use super::StorageError;
use super::database::{Database, database_error};
use super::messages::{MessageRow, message, message_columns, millis};
use crate::model::{
    AccountSettings, ConversationId, GroupId, Message, MessageId, ReceiptKind, ServiceId, Timestamp,
};

const READ_RECEIPTS: &str = "read_receipts";
const TYPING_INDICATORS: &str = "typing_indicators";

/// Our sent messages at `?1` to `?2`, or to a group they're in; ours have
/// no author.
const RECEIPT_TARGETS: &str = "SELECT m.id, m.conversation_id, c.group_id IS NOT NULL
     FROM messages m JOIN conversations c ON c.id = m.conversation_id
     WHERE m.author = x'' AND m.sent_at = ?1 AND m.status = 'sent'
       AND (c.peer = ?2 OR EXISTS (
           SELECT 1 FROM group_members g
           WHERE g.conversation_id = c.id AND g.member = ?2))";

/// The message by `?1` at `?2`.
const READ_ELSEWHERE_TARGET: &str =
    "SELECT conversation_id, sent_at FROM messages WHERE author = ?1 AND sent_at = ?2";

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
            let mut tx = self.begin_write().await?;
            let targets: Vec<(i64, i64, bool)> = sqlx::query_as(RECEIPT_TARGETS)
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

    /// Marks messages read on another device at `at` read here, with
    /// everything before them in their conversation, starting the countdown
    /// of disappearing ones, not before each was sent; returns the newly
    /// read ones.
    pub(crate) async fn mark_read_elsewhere(
        &self,
        read: &[(ServiceId, Timestamp)],
        at: Timestamp,
    ) -> Result<Vec<Message>, StorageError> {
        let mut changed = Vec::new();
        for (author, sent_at) in read {
            let found: Option<(i64, i64)> = sqlx::query_as(READ_ELSEWHERE_TARGET)
                .bind(author.uuid().as_bytes().as_slice())
                .bind(millis(*sent_at))
                .fetch_optional(&self.app)
                .await
                .map_err(database_error)?;
            let Some((conversation, sent_at)) = found else {
                continue;
            };
            let rows: Vec<MessageRow> = sqlx::query_as(concat!(
                "UPDATE messages SET read = 1, expires_at = MAX(?3, sent_at) + expires_in * 1000
                 WHERE conversation_id = ?1 AND read = 0 AND sent_at <= ?2
                 RETURNING ",
                message_columns!()
            ))
            .bind(conversation)
            .bind(sent_at)
            .bind(millis(at))
            .fetch_all(&self.app)
            .await
            .map_err(database_error)?;
            for row in rows {
                changed.push(message(row)?);
            }
        }
        self.resolve_quotes(&mut changed).await?;
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
    use super::*;
    use crate::model::{
        Content, Destination, GroupInfo, GroupMasterKey, GroupRef, Member, MessageBody,
        MessageStatus, Quote,
    };
    use crate::storage::testing::{at, db, peer};

    /// A sent message of ours, at `sent_at`.
    async fn sent(db: &Database, destination: Destination, sent_at: u64) -> Message {
        db.add_message(
            &destination,
            None,
            at(sent_at),
            &Content::text("out"),
            None,
            None,
            Timestamp::from_millis(0),
        )
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
        db.add_pending(conversation, at(100), &Content::text("x"), None, at(100))
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
            None,
            Some(at(sent_at)),
            Timestamp::from_millis(0),
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

        let read = db
            .mark_read(first.conversation, Timestamp::from_millis(0))
            .await
            .unwrap();

        let read: Vec<_> = read.iter().map(|m| (m.sender, m.sent_at)).collect();
        assert_eq!(read, [(Some(peer(1)), at(100)), (Some(peer(1)), at(101))]);
        assert!(
            db.mark_read(first.conversation, Timestamp::from_millis(0))
                .await
                .unwrap()
                .is_empty()
        );
        db.close().await;
    }

    #[tokio::test]
    async fn reads_elsewhere_mark_earlier_messages_too() {
        let (_dir, db) = db().await;
        let first = incoming(&db, 1, 100).await;
        incoming(&db, 1, 101).await;
        incoming(&db, 1, 102).await;

        let changed = db
            .mark_read_elsewhere(
                &[(peer(1), at(101)), (peer(9), at(5))],
                Timestamp::from_millis(0),
            )
            .await
            .unwrap();

        let changed: Vec<_> = changed.iter().map(|m| m.sent_at).collect();
        assert_eq!(changed, [at(100), at(101)]);
        let summary = db.conversation(first.conversation).await.unwrap().unwrap();
        assert_eq!(summary.unread, 1);
        db.close().await;
    }

    #[tokio::test]
    async fn reads_elsewhere_fill_in_quotes() {
        let (_dir, db) = db().await;
        incoming(&db, 1, 100).await;
        let reply = Content::Text {
            body: MessageBody::new("yes"),
            attachments: Vec::new(),
            quote: Some(Quote {
                author: Some(peer(1)),
                sent_at: at(100),
                original: None,
            }),
        };
        db.add_message(
            &Destination::Contact(peer(1)),
            Some(peer(1)),
            at(101),
            &reply,
            None,
            Some(at(101)),
            at(0),
        )
        .await
        .unwrap();

        let changed = db
            .mark_read_elsewhere(&[(peer(1), at(101))], at(0))
            .await
            .unwrap();

        let Content::Text {
            quote: Some(quote), ..
        } = &changed[1].content
        else {
            panic!("expected the reply");
        };
        assert_eq!(quote.original.as_ref().unwrap().text, "in");
        db.close().await;
    }

    #[tokio::test]
    async fn reads_elsewhere_start_countdowns_when_read_there() {
        let (_dir, db) = db().await;
        let message = db
            .add_message(
                &Destination::Contact(peer(1)),
                Some(peer(1)),
                at(100),
                &Content::text("in"),
                Some(60),
                Some(at(100)),
                at(0),
            )
            .await
            .unwrap()
            .unwrap();

        let read = db
            .mark_read_elsewhere(&[(peer(1), at(100))], at(5_000))
            .await
            .unwrap();

        assert_eq!(read.len(), 1);
        assert_eq!(read[0].id, message.id);
        assert_eq!(
            read[0].expiry.and_then(|expiry| expiry.at),
            Some(at(65_000))
        );
        db.close().await;
    }

    #[tokio::test]
    async fn reads_elsewhere_dont_start_countdowns_before_sending() {
        let (_dir, db) = db().await;
        db.add_message(
            &Destination::Contact(peer(1)),
            Some(peer(1)),
            at(10_000),
            &Content::text("in"),
            Some(60),
            Some(at(10_000)),
            at(20_000),
        )
        .await
        .unwrap()
        .unwrap();

        let read = db
            .mark_read_elsewhere(&[(peer(1), at(10_000))], at(0))
            .await
            .unwrap();

        assert_eq!(
            read[0].expiry.and_then(|expiry| expiry.at),
            Some(at(70_000))
        );
        db.close().await;
    }

    #[tokio::test]
    async fn message_lookups_use_an_index() {
        let (_dir, db) = db().await;
        for query in [RECEIPT_TARGETS, READ_ELSEWHERE_TARGET] {
            let plan: Vec<(i64, i64, i64, String)> =
                sqlx::query_as(sqlx::AssertSqlSafe(format!("EXPLAIN QUERY PLAN {query}")))
                    .fetch_all(db.pool())
                    .await
                    .unwrap();
            let steps: Vec<_> = plan.into_iter().map(|(.., step)| step).collect();
            assert!(
                steps.iter().any(|step| step.contains("messages_by_author")),
                "{steps:?}"
            );
            assert!(
                !steps.iter().any(|step| step.starts_with("SCAN")),
                "{steps:?}"
            );
        }
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
