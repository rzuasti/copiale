//! Chat state: chat list, open conversation and connection, driven by core
//! events, and the commands user actions produce. No GPUI; the views
//! render it.

use std::collections::HashMap;
use std::time::Duration;

use copiale_core::{
    BackendError, Command, ConversationId, ConversationSummary, Direction, Event, Message,
    MessageBody, MessageId, MessageStatus, Peer, ServiceId,
};

use super::time::Clock;

/// Messages loaded per page.
pub const PAGE: u32 = 100;

/// Consecutive messages from one side within this gap share a header.
const GROUP_GAP_MILLIS: u64 = 5 * 60_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Connection {
    Connecting,
    /// Connected; receiving queued messages.
    Syncing,
    Online,
    Offline {
        retry_in: Duration,
    },
}

/// How the open conversation's rows changed, for the virtual list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogChange {
    /// All rows replaced by this many.
    Reset(usize),
    /// Rows inserted at `at`; the row after them may look different now.
    Inserted { at: usize, count: usize },
    /// The row at this index changed.
    Changed(usize),
}

/// What the view does after a change.
#[derive(Debug, Default)]
pub struct Effects {
    pub commands: Vec<Command>,
    pub log: Vec<LogChange>,
}

impl Effects {
    fn log(change: LogChange) -> Self {
        Self {
            commands: Vec::new(),
            log: vec![change],
        }
    }
}

/// One message as the log shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row<'a> {
    pub message: &'a Message,
    /// Time and sender, on the first message of a group.
    pub header: Option<(String, String)>,
    /// Day label, on the first message of a day.
    pub day: Option<String>,
    /// Why sending failed, if it failed this session.
    pub error: Option<&'a BackendError>,
}

#[derive(Debug)]
pub struct Chat {
    clock: Clock,
    /// Most recently active first.
    conversations: Vec<ConversationSummary>,
    search: String,
    open: Option<Log>,
    connection: Connection,
}

/// The open conversation's loaded messages.
#[derive(Debug)]
struct Log {
    id: ConversationId,
    peer: ServiceId,
    /// Oldest first, by sent time.
    messages: Vec<Message>,
    /// The newest page has arrived.
    loaded: bool,
    /// Older messages may be in storage.
    has_older: bool,
    loading_older: bool,
    errors: HashMap<MessageId, BackendError>,
}

impl Chat {
    pub fn new(clock: Clock) -> Self {
        Self {
            clock,
            conversations: Vec::new(),
            search: String::new(),
            open: None,
            connection: Connection::Connecting,
        }
    }

    pub fn clock(&self) -> &Clock {
        &self.clock
    }

    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    /// Whether any conversations exist, regardless of the search.
    pub fn has_conversations(&self) -> bool {
        !self.conversations.is_empty()
    }

    /// Conversations matching the search, most recently active first.
    pub fn conversations(&self) -> impl Iterator<Item = &ConversationSummary> {
        let search = self.search.trim().to_lowercase();
        self.conversations.iter().filter(move |summary| {
            search.is_empty() || title(summary).to_lowercase().contains(&search)
        })
    }

    pub fn set_search(&mut self, search: &str) {
        self.search = search.into();
    }

    pub fn open_id(&self) -> Option<ConversationId> {
        self.open.as_ref().map(|log| log.id)
    }

    pub fn open_conversation(&self) -> Option<&ConversationSummary> {
        let id = self.open_id()?;
        self.conversations.iter().find(|summary| summary.id == id)
    }

    pub fn row_count(&self) -> usize {
        self.open.as_ref().map_or(0, |log| log.messages.len())
    }

    pub fn row(&self, ix: usize) -> Option<Row<'_>> {
        let log = self.open.as_ref()?;
        let message = log.messages.get(ix)?;
        let previous = ix.checked_sub(1).map(|ix| &log.messages[ix]);
        let day = self.clock.day(message.sent_at);
        let new_day = previous.is_none_or(|previous| self.clock.day(previous.sent_at) != day);
        let grouped = !new_day
            && previous.is_some_and(|previous| {
                previous.direction == message.direction
                    && message
                        .sent_at
                        .as_millis()
                        .saturating_sub(previous.sent_at.as_millis())
                        <= GROUP_GAP_MILLIS
            });
        let header = (!grouped).then(|| {
            let sender = match message.direction {
                Direction::Outgoing => "you".into(),
                Direction::Incoming => self
                    .open_conversation()
                    .map_or_else(|| unknown(log.peer), title),
            };
            (self.clock.time(message.sent_at), sender)
        });
        Some(Row {
            message,
            header,
            day: new_day.then(|| self.clock.day_label(day)),
            error: log.errors.get(&message.id),
        })
    }

    /// Whether older messages are being loaded.
    pub fn loading_older(&self) -> bool {
        self.open.as_ref().is_some_and(|log| log.loading_older)
    }

    /// `active`: whether the window has focus, so new messages count as read.
    pub fn apply(&mut self, event: &Event, active: bool) -> Effects {
        match event {
            Event::Conversations(conversations) => {
                self.conversations = conversations.clone();
                self.sort();
            }
            Event::ConversationUpdated(summary) => {
                match self.conversations.iter_mut().find(|c| c.id == summary.id) {
                    Some(existing) => *existing = summary.clone(),
                    None => self.conversations.push(summary.clone()),
                }
                self.sort();
            }
            Event::Messages {
                conversation,
                before,
                messages,
            } => return self.page(*conversation, *before, messages),
            Event::MessageAdded(message) => return self.added(message, active),
            Event::MessageUpdated(message) => return self.updated(message, None),
            Event::SendFailed { message, error } => {
                return self.updated(message, Some(error.clone()));
            }
            Event::Connected => self.connection = Connection::Syncing,
            Event::Synced => self.connection = Connection::Online,
            Event::Disconnected { retry_in, .. } => {
                self.connection = Connection::Offline {
                    retry_in: *retry_in,
                };
            }
            _ => {}
        }
        Effects::default()
    }

    /// Shows a conversation, loads its newest messages and marks it read.
    pub fn open(&mut self, id: ConversationId) -> Effects {
        if self.open_id() == Some(id) {
            return Effects::default();
        }
        let Some(summary) = self.conversations.iter().find(|c| c.id == id) else {
            return Effects::default();
        };
        let Peer::Contact(peer) = summary.peer;
        let unread = summary.unread > 0;
        self.open = Some(Log {
            id,
            peer,
            messages: Vec::new(),
            loaded: false,
            has_older: false,
            loading_older: false,
            errors: HashMap::new(),
        });
        let mut commands = vec![Command::LoadMessages {
            conversation: id,
            before: None,
            limit: PAGE,
        }];
        if unread {
            commands.push(Command::MarkRead(id));
        }
        Effects {
            commands,
            log: vec![LogChange::Reset(0)],
        }
    }

    /// Asks for the page before the oldest loaded message, if there may be
    /// one and none is on its way.
    pub fn load_older(&mut self) -> Option<Command> {
        let log = self.open.as_mut()?;
        if !log.loaded || !log.has_older || log.loading_older {
            return None;
        }
        let oldest = log.messages.first()?.id;
        log.loading_older = true;
        Some(Command::LoadMessages {
            conversation: log.id,
            before: Some(oldest),
            limit: PAGE,
        })
    }

    /// The command sending `text` to the open conversation; `None` if
    /// there's nothing to send.
    pub fn submit(&self, text: &str) -> Option<Command> {
        let log = self.open.as_ref()?;
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        Some(Command::SendText {
            recipient: log.peer,
            body: MessageBody::new(text),
        })
    }

    pub fn retry(&self, id: MessageId) -> Option<Command> {
        let log = self.open.as_ref()?;
        let failed = log
            .messages
            .iter()
            .any(|m| m.id == id && m.status == Some(MessageStatus::Failed));
        failed.then_some(Command::RetrySend(id))
    }

    /// The window got focus: the open conversation's new messages are read.
    pub fn activated(&self) -> Option<Command> {
        let summary = self.open_conversation()?;
        (summary.unread > 0).then_some(Command::MarkRead(summary.id))
    }

    fn sort(&mut self) {
        self.conversations
            .sort_by_key(|c| std::cmp::Reverse((c.last_activity, c.id)));
    }

    fn page(
        &mut self,
        conversation: ConversationId,
        before: Option<MessageId>,
        messages: &[Message],
    ) -> Effects {
        let Some(log) = self.open.as_mut().filter(|log| log.id == conversation) else {
            return Effects::default();
        };
        let full = messages.len() >= PAGE as usize;
        match before {
            None if !log.loaded => {
                log.messages = messages.to_vec();
                log.loaded = true;
                log.has_older = full;
                Effects::log(LogChange::Reset(log.messages.len()))
            }
            Some(before)
                if log.loading_older && log.messages.first().map(|m| m.id) == Some(before) =>
            {
                log.loading_older = false;
                log.has_older = full;
                let older: Vec<_> = messages
                    .iter()
                    .filter(|m| !log.messages.iter().any(|known| known.id == m.id))
                    .cloned()
                    .collect();
                let count = older.len();
                log.messages.splice(0..0, older);
                if count == 0 {
                    return Effects::default();
                }
                Effects::log(LogChange::Inserted { at: 0, count })
            }
            _ => Effects::default(),
        }
    }

    fn added(&mut self, message: &Message, active: bool) -> Effects {
        let Some(log) = self
            .open
            .as_mut()
            .filter(|log| log.id == message.conversation && log.loaded)
        else {
            return Effects::default();
        };
        if log.messages.iter().any(|m| m.id == message.id) {
            return Effects::default();
        }
        let at = log
            .messages
            .partition_point(|m| (m.sent_at, m.id) < (message.sent_at, message.id));
        // Older than everything loaded: it comes with that page.
        if at == 0 && log.has_older {
            return Effects::default();
        }
        log.messages.insert(at, message.clone());
        let mut effects = Effects::log(LogChange::Inserted { at, count: 1 });
        if active && message.direction == Direction::Incoming {
            effects.commands.push(Command::MarkRead(log.id));
        }
        effects
    }

    fn updated(&mut self, message: &Message, error: Option<BackendError>) -> Effects {
        let Some(log) = self
            .open
            .as_mut()
            .filter(|log| log.id == message.conversation)
        else {
            return Effects::default();
        };
        let Some(ix) = log.messages.iter().position(|m| m.id == message.id) else {
            return Effects::default();
        };
        log.messages[ix] = message.clone();
        match error {
            Some(error) => log.errors.insert(message.id, error),
            None => log.errors.remove(&message.id),
        };
        Effects::log(LogChange::Changed(ix))
    }
}

/// The contact name, or a stand-in naming the end of the account ID.
pub fn title(summary: &ConversationSummary) -> String {
    let Peer::Contact(peer) = summary.peer;
    summary.title.clone().unwrap_or_else(|| unknown(peer))
}

fn unknown(peer: ServiceId) -> String {
    let hex = peer.uuid().simple().to_string();
    format!("Unknown ({})", &hex[hex.len() - 4..])
}

#[cfg(test)]
mod tests {
    use chrono::FixedOffset;
    use copiale_core::Timestamp;
    use uuid::Uuid;

    use super::*;

    /// 2026-10-13 14:05 at UTC+2.
    const NOW: u64 = 1_791_893_100_000;
    const MINUTE: u64 = 60_000;

    fn chat() -> Chat {
        Chat::new(Clock::fixed(
            FixedOffset::east_opt(2 * 3600).unwrap(),
            Timestamp::from_millis(NOW),
        ))
    }

    fn peer(n: u128) -> ServiceId {
        ServiceId::new(Uuid::from_u128(n))
    }

    fn summary(id: i64, activity: u64, unread: u32, title: Option<&str>) -> ConversationSummary {
        ConversationSummary {
            id: ConversationId::new(id),
            peer: Peer::Contact(peer(id as u128)),
            title: title.map(Into::into),
            last_activity: Some(Timestamp::from_millis(activity)),
            unread,
            last_message: None,
        }
    }

    fn message(id: i64, direction: Direction, sent_at: u64) -> Message {
        let outgoing = direction == Direction::Outgoing;
        Message {
            id: MessageId::new(id),
            conversation: ConversationId::new(1),
            direction,
            sent_at: Timestamp::from_millis(sent_at),
            received_at: Timestamp::from_millis(sent_at),
            body: MessageBody::new(format!("message {id}")),
            read: outgoing,
            status: outgoing.then_some(MessageStatus::Sent),
        }
    }

    fn incoming(id: i64, sent_at: u64) -> Message {
        message(id, Direction::Incoming, sent_at)
    }

    /// Conversation 1 ("Alice", 2 unread) and 2 (untitled), loaded.
    fn with_conversations() -> Chat {
        let mut chat = chat();
        chat.apply(
            &Event::Conversations(vec![
                summary(1, NOW, 2, Some("Alice")),
                summary(2, NOW - MINUTE, 0, None),
            ]),
            true,
        );
        chat
    }

    /// Conversation 1 open with `messages` as its newest page.
    fn opened(messages: Vec<Message>) -> Chat {
        let mut chat = with_conversations();
        chat.open(ConversationId::new(1));
        chat.apply(
            &Event::Messages {
                conversation: ConversationId::new(1),
                before: None,
                messages,
            },
            true,
        );
        chat
    }

    fn ids(chat: &Chat) -> Vec<i64> {
        (0..chat.row_count())
            .map(|ix| chat.row(ix).unwrap().message.id.get())
            .collect()
    }

    fn titles(chat: &Chat) -> Vec<String> {
        chat.conversations().map(title).collect()
    }

    #[test]
    fn untitled_conversations_name_the_account() {
        let untitled = summary(0xbeef, NOW, 0, None);
        assert_eq!(title(&untitled), "Unknown (beef)");
    }

    #[test]
    fn updates_keep_the_list_sorted_by_activity() {
        let mut chat = with_conversations();

        chat.apply(
            &Event::ConversationUpdated(summary(2, NOW + MINUTE, 1, None)),
            true,
        );
        chat.apply(
            &Event::ConversationUpdated(summary(3, NOW - 5 * MINUTE, 1, Some("Carol"))),
            true,
        );

        assert_eq!(titles(&chat), ["Unknown (0002)", "Alice", "Carol"]);
    }

    #[test]
    fn search_filters_by_title_ignoring_case() {
        let mut chat = with_conversations();

        chat.set_search(" ali ");
        assert_eq!(titles(&chat), ["Alice"]);
        chat.set_search("unknown");
        assert_eq!(titles(&chat), ["Unknown (0002)"]);
        chat.set_search("");
        assert_eq!(titles(&chat).len(), 2);
        assert!(chat.has_conversations());
    }

    #[test]
    fn opening_loads_messages_and_marks_read() {
        let mut chat = with_conversations();

        let effects = chat.open(ConversationId::new(1));

        assert!(matches!(
            effects.commands.as_slice(),
            [
                Command::LoadMessages { conversation, before: None, limit: PAGE },
                Command::MarkRead(read),
            ] if conversation.get() == 1 && read.get() == 1
        ));
        assert_eq!(effects.log, [LogChange::Reset(0)]);
        assert_eq!(chat.open_conversation().unwrap().id.get(), 1);
        // Already open: nothing to do.
        assert!(chat.open(ConversationId::new(1)).commands.is_empty());
    }

    #[test]
    fn opening_a_read_conversation_does_not_mark_it() {
        let mut chat = with_conversations();

        let effects = chat.open(ConversationId::new(2));

        assert!(matches!(
            effects.commands.as_slice(),
            [Command::LoadMessages { .. }]
        ));
    }

    #[test]
    fn first_page_resets_the_log() {
        let mut chat = with_conversations();
        chat.open(ConversationId::new(1));

        let effects = chat.apply(
            &Event::Messages {
                conversation: ConversationId::new(1),
                before: None,
                messages: vec![incoming(1, NOW - 2 * MINUTE), incoming(2, NOW - MINUTE)],
            },
            true,
        );

        assert_eq!(effects.log, [LogChange::Reset(2)]);
        assert_eq!(ids(&chat), [1, 2]);
        // A short page means there's nothing older.
        assert!(chat.load_older().is_none());
    }

    #[test]
    fn pages_for_other_conversations_are_ignored() {
        let mut chat = with_conversations();
        chat.open(ConversationId::new(1));

        let effects = chat.apply(
            &Event::Messages {
                conversation: ConversationId::new(2),
                before: None,
                messages: vec![incoming(1, NOW)],
            },
            true,
        );

        assert!(effects.log.is_empty());
        assert_eq!(chat.row_count(), 0);
    }

    fn full_page(first_id: i64, first_sent: u64) -> Vec<Message> {
        (0..PAGE as i64)
            .map(|n| incoming(first_id + n, first_sent + n as u64))
            .collect()
    }

    #[test]
    fn older_pages_are_prepended_once() {
        let mut chat = opened(full_page(1000, NOW - 1000));

        let Some(Command::LoadMessages {
            before: Some(before),
            ..
        }) = chat.load_older()
        else {
            panic!("expected a load");
        };
        assert_eq!(before.get(), 1000);
        assert!(chat.loading_older());
        assert!(chat.load_older().is_none());

        let effects = chat.apply(
            &Event::Messages {
                conversation: ConversationId::new(1),
                before: Some(before),
                messages: vec![incoming(1, NOW - 5000), incoming(2, NOW - 4000)],
            },
            true,
        );

        assert_eq!(effects.log, [LogChange::Inserted { at: 0, count: 2 }]);
        assert_eq!(&ids(&chat)[..3], [1, 2, 1000]);
        assert!(!chat.loading_older());
        assert!(chat.load_older().is_none());
    }

    #[test]
    fn new_messages_are_inserted_in_sent_order() {
        let mut chat = opened(vec![
            incoming(1, NOW - 3 * MINUTE),
            incoming(2, NOW - MINUTE),
        ]);

        let appended = chat.apply(&Event::MessageAdded(incoming(3, NOW)), true);
        let between = chat.apply(&Event::MessageAdded(incoming(4, NOW - 2 * MINUTE)), true);
        let again = chat.apply(&Event::MessageAdded(incoming(4, NOW - 2 * MINUTE)), true);

        assert_eq!(appended.log, [LogChange::Inserted { at: 2, count: 1 }]);
        assert_eq!(between.log, [LogChange::Inserted { at: 1, count: 1 }]);
        assert!(again.log.is_empty());
        assert_eq!(ids(&chat), [1, 4, 2, 3]);
    }

    #[test]
    fn messages_older_than_the_loaded_page_wait_for_it() {
        let mut chat = opened(full_page(1000, NOW - 1000));

        let effects = chat.apply(&Event::MessageAdded(incoming(1, NOW - 5000)), true);

        assert!(effects.log.is_empty());
        assert_eq!(chat.row_count(), PAGE as usize);
    }

    #[test]
    fn messages_before_the_first_page_come_with_it() {
        let mut chat = with_conversations();
        chat.open(ConversationId::new(1));

        let effects = chat.apply(&Event::MessageAdded(incoming(1, NOW)), true);

        assert!(effects.log.is_empty());
        assert_eq!(chat.row_count(), 0);
    }

    #[test]
    fn incoming_messages_are_read_only_while_focused() {
        let mut chat = opened(vec![]);

        let focused = chat.apply(&Event::MessageAdded(incoming(1, NOW)), true);
        let unfocused = chat.apply(&Event::MessageAdded(incoming(2, NOW + 1)), false);
        let own = chat.apply(
            &Event::MessageAdded(message(3, Direction::Outgoing, NOW + 2)),
            true,
        );

        assert!(matches!(
            focused.commands.as_slice(),
            [Command::MarkRead(_)]
        ));
        assert!(unfocused.commands.is_empty());
        assert!(own.commands.is_empty());
    }

    #[test]
    fn focusing_marks_the_open_conversation_read() {
        let mut chat = with_conversations();
        assert!(chat.activated().is_none());
        chat.open(ConversationId::new(1));

        assert!(matches!(chat.activated(), Some(Command::MarkRead(id)) if id.get() == 1));

        chat.apply(
            &Event::ConversationUpdated(summary(1, NOW, 0, Some("Alice"))),
            true,
        );
        assert!(chat.activated().is_none());
    }

    #[test]
    fn rows_group_by_side_time_and_day() {
        // 23:59 yesterday, then today: 14:00, 14:02, 14:20, mine at 14:21.
        let today = NOW - 5 * MINUTE;
        let chat = opened(vec![
            incoming(1, NOW - (14 * 60 + 6) * MINUTE),
            incoming(2, today),
            incoming(3, today + 2 * MINUTE),
            incoming(4, today + 20 * MINUTE),
            message(5, Direction::Outgoing, today + 21 * MINUTE),
        ]);
        let row = |ix| chat.row(ix).unwrap();
        let header = |time: &str, sender: &str| Some((time.to_string(), sender.to_string()));

        assert_eq!(row(0).day.as_deref(), Some("Yesterday"));
        assert_eq!(row(0).header, header("23:59", "Alice"));
        assert_eq!(row(1).day.as_deref(), Some("Today"));
        assert_eq!(row(1).header, header("14:00", "Alice"));
        assert_eq!(row(2).day, None);
        assert_eq!(row(2).header, None);
        assert_eq!(row(3).header, header("14:20", "Alice"));
        assert_eq!(row(4).header, header("14:21", "you"));
        assert!(chat.row(5).is_none());
    }

    #[test]
    fn send_results_update_rows_and_keep_the_error() {
        let pending = Message {
            status: Some(MessageStatus::Pending),
            ..message(1, Direction::Outgoing, NOW)
        };
        let mut chat = opened(vec![pending.clone()]);
        let failed = Message {
            status: Some(MessageStatus::Failed),
            ..pending.clone()
        };
        assert!(chat.retry(pending.id).is_none());

        let effects = chat.apply(
            &Event::SendFailed {
                message: failed.clone(),
                error: BackendError::new("offline"),
            },
            true,
        );

        assert_eq!(effects.log, [LogChange::Changed(0)]);
        let row = chat.row(0).unwrap();
        assert_eq!(row.message, &failed);
        assert_eq!(row.error, Some(&BackendError::new("offline")));
        assert!(matches!(chat.retry(pending.id), Some(Command::RetrySend(id)) if id == pending.id));

        chat.apply(&Event::MessageUpdated(pending.clone()), true);
        assert_eq!(chat.row(0).unwrap().error, None);
        assert!(chat.retry(pending.id).is_none());
    }

    #[test]
    fn submit_sends_trimmed_text_to_the_open_peer() {
        let mut chat = with_conversations();
        assert!(chat.submit("hi").is_none());
        chat.open(ConversationId::new(1));

        assert!(chat.submit(" \n ").is_none());
        let Some(Command::SendText { recipient, body }) = chat.submit("  hi\nthere \n") else {
            panic!("expected SendText");
        };
        assert_eq!(recipient, peer(1));
        assert_eq!(body, MessageBody::new("hi\nthere"));
    }

    #[test]
    fn connection_follows_the_core() {
        let mut chat = chat();
        assert_eq!(chat.connection(), &Connection::Connecting);

        chat.apply(&Event::Connected, true);
        assert_eq!(chat.connection(), &Connection::Syncing);
        chat.apply(&Event::Synced, true);
        assert_eq!(chat.connection(), &Connection::Online);
        chat.apply(
            &Event::Disconnected {
                error: None,
                retry_in: Duration::from_secs(4),
            },
            true,
        );
        assert_eq!(
            chat.connection(),
            &Connection::Offline {
                retry_in: Duration::from_secs(4)
            }
        );
    }
}
