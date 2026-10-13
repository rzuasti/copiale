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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_id_debug_shows_only_last_digits() {
        let uuid = Uuid::parse_str("0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0").unwrap();
        assert_eq!(format!("{:?}", ServiceId::new(uuid)), "ServiceId(…e1f0)");
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
