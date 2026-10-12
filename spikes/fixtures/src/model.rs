use crate::color::Rgb;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContactId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConversationId(pub u32);

/// Globally unique and increasing with `sent_at_ms`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MessageId(pub u64);

#[derive(Debug, Clone, PartialEq)]
pub struct Contact {
    pub id: ContactId,
    pub name: String,
    /// Avatar color.
    pub color: Rgb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationKind {
    Direct,
    Group,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Conversation {
    pub id: ConversationId,
    pub kind: ConversationKind,
    pub title: String,
    /// Excludes the local user.
    pub members: Vec<ContactId>,
    /// Oldest first.
    pub messages: Vec<Message>,
}

impl Conversation {
    pub fn last_message(&self) -> Option<&Message> {
        self.messages.last()
    }

    /// Index of `id` in `messages`.
    pub fn position(&self, id: MessageId) -> Option<usize> {
        self.messages.binary_search_by_key(&id, |m| m.id).ok()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sender {
    Me,
    Contact(ContactId),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub id: MessageId,
    pub conversation: ConversationId,
    pub sender: Sender,
    /// Unix epoch, milliseconds.
    pub sent_at_ms: i64,
    /// May be empty for image-only messages.
    pub body: String,
    pub image: Option<ImageAttachment>,
    /// Always an earlier message in the same conversation.
    pub reply_to: Option<MessageId>,
    pub reactions: Vec<Reaction>,
}

impl Message {
    /// `HH:MM` (UTC).
    pub fn time_label(&self) -> String {
        let seconds_of_day = (self.sent_at_ms / 1000).rem_euclid(86_400);
        format!(
            "{:02}:{:02}",
            seconds_of_day / 3600,
            seconds_of_day % 3600 / 60
        )
    }

    /// `YYYY-MM-DD` (UTC), for day separators.
    pub fn date_label(&self) -> String {
        let (year, month, day) = civil_from_days((self.sent_at_ms / 1000).div_euclid(86_400));
        format!("{year:04}-{month:02}-{day:02}")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Reaction {
    pub emoji: &'static str,
    pub count: u32,
}

/// Synthetic image; pixels are generated on demand from `seed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageAttachment {
    pub width: u32,
    pub height: u32,
    pub seed: u64,
}

/// Days since the Unix epoch to a proleptic Gregorian date (Howard Hinnant's
/// algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message_at(sent_at_ms: i64) -> Message {
        Message {
            id: MessageId(1),
            conversation: ConversationId(0),
            sender: Sender::Me,
            sent_at_ms,
            body: String::new(),
            image: None,
            reply_to: None,
            reactions: Vec::new(),
        }
    }

    #[test]
    fn labels() {
        // 2026-10-01T12:34:56Z
        let message = message_at(1_790_858_096_000);
        assert_eq!(message.time_label(), "12:34");
        assert_eq!(message.date_label(), "2026-10-01");
        assert_eq!(message_at(0).date_label(), "1970-01-01");
        // 2024-02-29T00:00:00Z
        assert_eq!(message_at(1_709_164_800_000).date_label(), "2024-02-29");
    }
}
