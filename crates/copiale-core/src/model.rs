//! Plain data shared between the core and the UI.

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use uuid::Uuid;

/// Signal account identifier. `Debug` shows only the last 4 hex digits.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ServiceId(Uuid);

impl ServiceId {
    pub fn new(uuid: Uuid) -> Self {
        Self(uuid)
    }

    pub fn uuid(&self) -> Uuid {
        self.0
    }
}

impl fmt::Debug for ServiceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hex = self.0.simple().to_string();
        write!(f, "ServiceId(…{})", &hex[hex.len() - 4..])
    }
}

/// E.164 phone number. `Debug` shows only the last 2 digits; no `Display`,
/// so it can't reach a log by accident.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PhoneNumber(String);

impl PhoneNumber {
    pub fn new(e164: impl Into<String>) -> Self {
        Self(e164.into())
    }

    /// The full number, for display to the user.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for PhoneNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tail = self.0.len().saturating_sub(2);
        write!(f, "PhoneNumber(…{})", self.0.get(tail..).unwrap_or(""))
    }
}

/// Signal group identifier. `Debug` shows only the last 2 bytes.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct GroupId([u8; 32]);

impl GroupId {
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for GroupId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "GroupId(…{:02x}{:02x})", self.0[30], self.0[31])
    }
}

/// Message text. `Debug` shows only its length.
#[derive(Clone, PartialEq, Eq)]
pub struct MessageBody(String);

impl MessageBody {
    pub fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for MessageBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MessageBody({} bytes)", self.0.len())
    }
}

/// Milliseconds since the Unix epoch; Signal's message timestamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(u64);

impl Timestamp {
    pub fn from_millis(millis: u64) -> Self {
        Self(millis)
    }

    pub fn as_millis(&self) -> u64 {
        self.0
    }

    pub fn now() -> Self {
        let elapsed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Self(elapsed.as_millis() as u64)
    }
}

/// A text message received from another account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IncomingMessage {
    pub sender: ServiceId,
    pub timestamp: Timestamp,
    pub body: MessageBody,
}

/// A text message this account sent from another device (the phone).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SentMessage {
    pub recipient: ServiceId,
    pub timestamp: Timestamp,
    pub body: MessageBody,
}

/// Local ID of a conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConversationId(i64);

impl ConversationId {
    pub fn new(id: i64) -> Self {
        Self(id)
    }

    pub fn get(self) -> i64 {
        self.0
    }
}

/// Local ID of a stored message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MessageId(i64);

impl MessageId {
    pub fn new(id: i64) -> Self {
        Self(id)
    }

    pub fn get(self) -> i64 {
        self.0
    }
}

/// Who a conversation is with. Groups come with #12.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Peer {
    Contact(ServiceId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Incoming,
    /// Sent by this account, from any of its devices.
    Outgoing,
}

/// A stored message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub id: MessageId,
    pub conversation: ConversationId,
    pub direction: Direction,
    /// The sender's timestamp; with the author, Signal's message identity.
    pub sent_at: Timestamp,
    /// When this device stored it, by the local clock.
    pub received_at: Timestamp,
    pub body: MessageBody,
    pub read: bool,
}

/// A conversation as the chat list shows it. `Debug` hides the title.
#[derive(Clone, PartialEq, Eq)]
pub struct ConversationSummary {
    pub id: ConversationId,
    pub peer: Peer,
    /// Synced contact name, if known.
    pub title: Option<String>,
    /// Local time of the newest message.
    pub last_activity: Option<Timestamp>,
    pub unread: u32,
    pub last_message: Option<MessagePreview>,
}

impl fmt::Debug for ConversationSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConversationSummary")
            .field("id", &self.id)
            .field("peer", &self.peer)
            .field("titled", &self.title.is_some())
            .field("last_activity", &self.last_activity)
            .field("unread", &self.unread)
            .field("last_message", &self.last_message)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessagePreview {
    pub direction: Direction,
    pub body: MessageBody,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_id_debug_shows_only_last_digits() {
        let uuid = Uuid::parse_str("0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0").unwrap();
        assert_eq!(format!("{:?}", ServiceId::new(uuid)), "ServiceId(…e1f0)");
    }

    #[test]
    fn phone_number_debug_shows_only_last_digits() {
        assert_eq!(
            format!("{:?}", PhoneNumber::new("+14155550123")),
            "PhoneNumber(…23)"
        );
        assert_eq!(format!("{:?}", PhoneNumber::new("")), "PhoneNumber(…)");
    }

    #[test]
    fn group_id_debug_shows_only_last_bytes() {
        let mut bytes = [0x11; 32];
        bytes[30] = 0xab;
        bytes[31] = 0xcd;
        assert_eq!(format!("{:?}", GroupId::new(bytes)), "GroupId(…abcd)");
    }

    #[test]
    fn message_body_debug_hides_text() {
        let debug = format!("{:?}", MessageBody::new("secret plans"));
        assert_eq!(debug, "MessageBody(12 bytes)");
    }

    #[test]
    fn timestamp_now_is_after_2020() {
        assert!(Timestamp::now().as_millis() > 1_577_836_800_000);
    }
}
