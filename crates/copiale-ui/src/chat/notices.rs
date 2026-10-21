//! Which desktop notifications to show, and what they reveal. No platform
//! code; [`crate::notifier`] shows them.

use std::collections::{HashMap, HashSet};

use copiale_core::config::NotificationContent;
use copiale_core::{
    ChatState, Content, ConversationId, ConversationSummary, Direction, Event, Message, Peer,
};

use super::content::placeholders;
use super::model::{sender_name, title};
use super::text::isolate;

/// Longest message text a notification shows, in characters.
const MAX_PREVIEW: usize = 200;

/// What a notification is about; a new one replaces the shown one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NoticeKey {
    /// New messages in a chat.
    Chat(ConversationId),
    /// Messages that arrived while offline.
    Backlog,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub key: NoticeKey,
    pub title: String,
    pub body: String,
    /// No sound, low urgency: for message requests.
    pub quiet: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoticeAction {
    /// Shows the notice, replacing any with its key.
    Show(Notice),
    Close(NoticeKey),
}

/// Notification state: unseen messages per chat, and the backlog while
/// queued messages arrive.
#[derive(Debug)]
pub struct Notices {
    content: NotificationContent,
    /// Messages notified per chat since it was last read.
    counts: HashMap<ConversationId, u32>,
    /// New messages to notify once their conversation's update arrives,
    /// which names it.
    waiting: Vec<Message>,
    /// Set while queued messages arrive.
    backlog: Option<Backlog>,
}

#[derive(Debug, Default)]
struct Backlog {
    messages: u32,
    chats: HashSet<ConversationId>,
}

impl Notices {
    pub fn new(content: NotificationContent) -> Self {
        Self {
            content,
            counts: HashMap::new(),
            waiting: Vec::new(),
            backlog: None,
        }
    }

    /// Switching off closes what's shown.
    pub fn set_content(&mut self, content: NotificationContent) -> Vec<NoticeAction> {
        self.content = content;
        if content != NotificationContent::Off {
            return Vec::new();
        }
        self.close_all()
    }

    /// Closes everything shown and forgets what's pending.
    pub fn close_all(&mut self) -> Vec<NoticeAction> {
        self.waiting.clear();
        self.backlog = None;
        let mut actions: Vec<_> = self
            .counts
            .drain()
            .map(|(id, _)| NoticeAction::Close(NoticeKey::Chat(id)))
            .collect();
        actions.push(NoticeAction::Close(NoticeKey::Backlog));
        actions
    }

    /// Reacts to `event`. `focused`: the chat open in the focused window,
    /// which never notifies.
    pub fn apply(&mut self, event: &Event, focused: Option<ConversationId>) -> Vec<NoticeAction> {
        match event {
            Event::Connected => {
                self.backlog.get_or_insert_with(Backlog::default);
            }
            Event::Synced => {
                if let Some(backlog) = self.backlog.take()
                    && backlog.messages > 0
                    && self.content != NotificationContent::Off
                {
                    return vec![NoticeAction::Show(backlog_notice(&backlog))];
                }
            }
            Event::MessageAdded(message) if notable(message) => {
                if Some(message.conversation) != focused {
                    self.waiting.push(message.clone());
                }
            }
            Event::ConversationUpdated(summary) => return self.updated(summary),
            Event::MessageUpdated(Message {
                conversation,
                content: Content::Deleted,
                ..
            })
            | Event::MessageRemoved { conversation, .. }
            | Event::ConversationRemoved(conversation) => {
                // Don't leave what's gone on screen.
                self.waiting.retain(|m| m.conversation != *conversation);
                return self.close(*conversation);
            }
            _ => {}
        }
        Vec::new()
    }

    fn updated(&mut self, summary: &ConversationSummary) -> Vec<NoticeAction> {
        let (new, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.waiting)
            .into_iter()
            .partition(|m| m.conversation == summary.id);
        self.waiting = rest;
        if summary.unread == 0 {
            return self.close(summary.id);
        }
        let Some(latest) = new.last() else {
            return Vec::new();
        };
        if self.content == NotificationContent::Off {
            return Vec::new();
        }
        if let Some(backlog) = &mut self.backlog {
            backlog.messages += new.len() as u32;
            backlog.chats.insert(summary.id);
            return Vec::new();
        }
        let count = self.counts.entry(summary.id).or_default();
        *count += new.len() as u32;
        vec![NoticeAction::Show(chat_notice(
            summary,
            latest,
            *count,
            self.content,
        ))]
    }

    fn close(&mut self, conversation: ConversationId) -> Vec<NoticeAction> {
        match self.counts.remove(&conversation) {
            Some(_) => vec![NoticeAction::Close(NoticeKey::Chat(conversation))],
            None => Vec::new(),
        }
    }
}

/// Incoming and waiting to be read; reactions and system lines don't count.
fn notable(message: &Message) -> bool {
    message.direction == Direction::Incoming && !message.read && !message.content.is_quiet()
}

/// The chat's notice after `count` unseen messages, `latest` the newest. A
/// request's shows neither its name, which its sender chose, nor its text;
/// a disappearing message's never shows its text, which the OS may keep.
fn chat_notice(
    summary: &ConversationSummary,
    latest: &Message,
    count: u32,
    content: NotificationContent,
) -> Notice {
    let group = matches!(summary.peer, Peer::Group(_));
    if summary.state == ChatState::Request {
        let from = if group {
            "a group you haven't accepted"
        } else {
            "someone not in your contacts"
        };
        let body = match count {
            1 => format!("From {from}"),
            count => format!("{count} messages from {from}"),
        };
        return Notice {
            key: NoticeKey::Chat(summary.id),
            title: "Message request".into(),
            body,
            quiet: true,
        };
    }
    let content = match content {
        NotificationContent::Message if latest.expiry.is_some() => NotificationContent::Name,
        content => content,
    };
    let sender = latest
        .sender
        .map(|author| isolate(&sender_name(summary, author)));
    let body = match (content, count, sender) {
        (NotificationContent::Message, _, Some(sender)) if group => {
            format!("{sender}: {}", preview(&latest.content))
        }
        (NotificationContent::Message, _, _) => preview(&latest.content),
        (_, 1, Some(sender)) if group => format!("New message from {sender}"),
        (_, 1, _) => "New message".into(),
        (_, count, _) => format!("{count} new messages"),
    };
    Notice {
        key: NoticeKey::Chat(summary.id),
        title: title(summary),
        body,
        quiet: false,
    }
}

fn backlog_notice(backlog: &Backlog) -> Notice {
    let messages = match backlog.messages {
        1 => "1 new message".to_string(),
        count => format!("{count} new messages"),
    };
    let body = match backlog.chats.len() {
        0 | 1 => messages,
        chats => format!("{messages} in {chats} chats"),
    };
    Notice {
        key: NoticeKey::Backlog,
        title: "Copiale".into(),
        body,
        quiet: false,
    }
}

/// The text, shortened, else what the log shows in its place.
fn preview(content: &Content) -> String {
    match content.body() {
        Some(body) => {
            let text = body
                .as_str()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            match text.char_indices().nth(MAX_PREVIEW) {
                Some((end, _)) => format!("{}…", &text[..end]),
                None => text,
            }
        }
        None => placeholders(content).into_iter().next().unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use copiale_core::{
        Attachment, ChatState, ConversationSummary, Expiry, GroupId, Member, MessageBody,
        MessageId, ServiceId, Timestamp,
    };
    use uuid::Uuid;

    use super::*;

    fn peer(n: u128) -> ServiceId {
        ServiceId::new(Uuid::from_u128(n))
    }

    fn chat_id(n: i64) -> ConversationId {
        ConversationId::new(n)
    }

    fn summary(n: i64, unread: u32) -> ConversationSummary {
        ConversationSummary {
            id: chat_id(n),
            peer: Peer::Contact(peer(n as u128)),
            state: ChatState::Accepted,
            title: Some(format!("Contact {n}")),
            members: Vec::new(),
            last_activity: Some(Timestamp::from_millis(1)),
            unread,
            last_message: None,
        }
    }

    fn group(unread: u32) -> ConversationSummary {
        ConversationSummary {
            peer: Peer::Group(GroupId::new([7; 32])),
            state: ChatState::Accepted,
            title: Some("Dev".into()),
            members: vec![Member {
                id: peer(1),
                name: Some("Alice".into()),
            }],
            ..summary(9, unread)
        }
    }

    fn message(id: i64, chat: i64, body: &str) -> Message {
        Message {
            id: MessageId::new(id),
            conversation: chat_id(chat),
            direction: Direction::Incoming,
            sender: Some(peer(chat as u128)),
            sent_at: Timestamp::from_millis(id as u64),
            received_at: Timestamp::from_millis(id as u64),
            content: Content::text(body),
            edits: 0,
            read: false,
            status: None,
            expiry: None,
        }
    }

    /// Delivers a message and its conversation update, as the core does.
    fn receive(
        notices: &mut Notices,
        message: Message,
        summary: ConversationSummary,
        focused: Option<ConversationId>,
    ) -> Vec<NoticeAction> {
        let mut actions = notices.apply(&Event::MessageAdded(message), focused);
        actions.extend(notices.apply(&Event::ConversationUpdated(summary), focused));
        actions
    }

    fn shown(actions: &[NoticeAction]) -> Vec<(&str, &str)> {
        actions
            .iter()
            .filter_map(|action| match action {
                NoticeAction::Show(notice) => Some((notice.title.as_str(), notice.body.as_str())),
                NoticeAction::Close(_) => None,
            })
            .collect()
    }

    #[test]
    fn modes_choose_what_notifications_reveal() {
        let mut off = Notices::new(NotificationContent::Off);
        assert!(receive(&mut off, message(1, 1, "secret"), summary(1, 1), None).is_empty());

        let mut name = Notices::new(NotificationContent::Name);
        let actions = receive(&mut name, message(1, 1, "secret"), summary(1, 1), None);
        assert_eq!(shown(&actions), [("Contact 1", "New message")]);
        let actions = receive(&mut name, message(2, 9, "secret"), group(1), None);
        let body = format!("New message from {}", isolate("Unknown (0009)"));
        assert_eq!(shown(&actions), [("Dev", body.as_str())]);

        let mut text = Notices::new(NotificationContent::Message);
        let actions = receive(&mut text, message(1, 1, "hi  there"), summary(1, 1), None);
        assert_eq!(shown(&actions), [("Contact 1", "hi there")]);
        let mut from_alice = message(2, 9, "ship it");
        from_alice.sender = Some(peer(1));
        let actions = receive(&mut text, from_alice, group(1), None);
        let body = format!("{}: ship it", isolate("Alice"));
        assert_eq!(shown(&actions), [("Dev", body.as_str())]);
    }

    #[test]
    fn disappearing_messages_never_show_their_text() {
        let mut notices = Notices::new(NotificationContent::Message);
        let disappearing = |id, chat, body| Message {
            expiry: Some(Expiry {
                timer: 60,
                at: None,
            }),
            ..message(id, chat, body)
        };
        let actions = receive(
            &mut notices,
            disappearing(1, 1, "secret"),
            summary(1, 1),
            None,
        );
        assert_eq!(shown(&actions), [("Contact 1", "New message")]);
        let actions = receive(
            &mut notices,
            disappearing(2, 1, "secret"),
            summary(1, 2),
            None,
        );
        assert_eq!(shown(&actions), [("Contact 1", "2 new messages")]);

        let mut from_alice = disappearing(3, 9, "secret");
        from_alice.sender = Some(peer(1));
        let actions = receive(&mut notices, from_alice, group(1), None);
        let body = format!("New message from {}", isolate("Alice"));
        assert_eq!(shown(&actions), [("Dev", body.as_str())]);
    }

    #[test]
    fn requests_notify_quietly_without_name_or_text() {
        let mut notices = Notices::new(NotificationContent::Message);
        let request = |n: i64, unread| ConversationSummary {
            state: ChatState::Request,
            title: Some("Mom".into()),
            ..summary(n, unread)
        };

        let actions = receive(&mut notices, message(1, 1, "it's me"), request(1, 1), None);
        let actions = actions.into_iter().chain(receive(
            &mut notices,
            message(2, 1, "new number"),
            request(1, 2),
            None,
        ));
        let notices: Vec<_> = actions
            .map(|action| match action {
                NoticeAction::Show(notice) => (notice.title, notice.body, notice.quiet),
                NoticeAction::Close(_) => panic!("nothing to close"),
            })
            .collect();
        assert_eq!(
            notices,
            [
                (
                    "Message request".into(),
                    "From someone not in your contacts".into(),
                    true
                ),
                (
                    "Message request".into(),
                    "2 messages from someone not in your contacts".into(),
                    true
                ),
            ]
        );

        let mut groups = Notices::new(NotificationContent::Message);
        let group_request = ConversationSummary {
            state: ChatState::Request,
            ..group(1)
        };
        let actions = receive(&mut groups, message(3, 9, "hi"), group_request, None);
        assert_eq!(
            shown(&actions),
            [("Message request", "From a group you haven't accepted")]
        );
    }

    #[test]
    fn non_text_content_shows_its_placeholder() {
        let mut notices = Notices::new(NotificationContent::Message);
        let image = Message {
            content: Content::Text {
                body: MessageBody::new(""),
                attachments: vec![Attachment::Image],
                quote: None,
            },
            ..message(1, 1, "")
        };
        let actions = receive(&mut notices, image, summary(1, 1), None);
        assert_eq!(
            shown(&actions),
            [("Contact 1", "[image] open on your phone")]
        );

        let long = "word ".repeat(100);
        let actions = receive(&mut notices, message(2, 1, &long), summary(1, 2), None);
        let body = shown(&actions)[0].1.to_string();
        assert_eq!(body.chars().count(), MAX_PREVIEW + 1);
        assert!(body.ends_with('…'));
    }

    #[test]
    fn the_focused_chat_and_quiet_or_read_messages_dont_notify() {
        let mut notices = Notices::new(NotificationContent::Message);
        let focused = Some(chat_id(1));
        assert!(receive(&mut notices, message(1, 1, "hi"), summary(1, 1), focused).is_empty());

        let reaction = Message {
            content: Content::Reaction {
                emoji: "👍".into()
            },
            read: true,
            ..message(2, 2, "")
        };
        assert!(receive(&mut notices, reaction, summary(2, 0), None).is_empty());
        let outgoing = Message {
            direction: Direction::Outgoing,
            sender: None,
            read: true,
            ..message(3, 2, "mine")
        };
        assert!(receive(&mut notices, outgoing, summary(2, 0), None).is_empty());

        // Open but the window isn't focused: notifies.
        let actions = receive(&mut notices, message(4, 1, "hi"), summary(1, 1), None);
        assert_eq!(shown(&actions).len(), 1);
    }

    #[test]
    fn a_chats_notification_is_updated_then_closed_once_read() {
        let mut notices = Notices::new(NotificationContent::Name);
        receive(&mut notices, message(1, 1, "a"), summary(1, 1), None);
        let actions = receive(&mut notices, message(2, 1, "b"), summary(1, 2), None);
        assert_eq!(shown(&actions), [("Contact 1", "2 new messages")]);
        let NoticeAction::Show(notice) = &actions[0] else {
            unreachable!()
        };
        assert_eq!(notice.key, NoticeKey::Chat(chat_id(1)));

        let read = notices.apply(&Event::ConversationUpdated(summary(1, 0)), None);
        assert_eq!(read, [NoticeAction::Close(NoticeKey::Chat(chat_id(1)))]);
        assert!(
            notices
                .apply(&Event::ConversationUpdated(summary(1, 0)), None)
                .is_empty(),
            "closed once"
        );
        let actions = receive(&mut notices, message(3, 1, "c"), summary(1, 1), None);
        assert_eq!(
            shown(&actions),
            [("Contact 1", "New message")],
            "counts again"
        );
    }

    #[test]
    fn deleted_and_removed_messages_close_their_notification() {
        let mut notices = Notices::new(NotificationContent::Message);
        receive(&mut notices, message(1, 1, "oops"), summary(1, 1), None);
        let deleted = Message {
            content: Content::Deleted,
            ..message(1, 1, "")
        };
        let close = vec![NoticeAction::Close(NoticeKey::Chat(chat_id(1)))];
        assert_eq!(notices.apply(&Event::MessageUpdated(deleted), None), close);

        receive(
            &mut notices,
            message(2, 1, "gone soon"),
            summary(1, 1),
            None,
        );
        let removed = Event::MessageRemoved {
            conversation: chat_id(1),
            id: MessageId::new(2),
        };
        assert_eq!(notices.apply(&removed, None), close);
    }

    #[test]
    fn queued_messages_notify_once_as_a_summary() {
        let mut notices = Notices::new(NotificationContent::Message);
        assert!(notices.apply(&Event::Connected, None).is_empty());
        assert!(receive(&mut notices, message(1, 1, "a"), summary(1, 1), None).is_empty());
        assert!(receive(&mut notices, message(2, 1, "b"), summary(1, 2), None).is_empty());
        assert!(receive(&mut notices, message(3, 2, "c"), summary(2, 1), None).is_empty());

        let actions = notices.apply(&Event::Synced, None);
        assert_eq!(shown(&actions), [("Copiale", "3 new messages in 2 chats")]);
        assert!(notices.apply(&Event::Synced, None).is_empty(), "once");

        // Live again.
        let actions = receive(&mut notices, message(4, 1, "d"), summary(1, 3), None);
        assert_eq!(shown(&actions), [("Contact 1", "d")]);

        notices.apply(&Event::Connected, None);
        assert!(
            notices.apply(&Event::Synced, None).is_empty(),
            "nothing queued"
        );
    }

    #[test]
    fn switching_off_closes_everything() {
        let mut notices = Notices::new(NotificationContent::Name);
        receive(&mut notices, message(1, 1, "a"), summary(1, 1), None);
        let actions = notices.set_content(NotificationContent::Off);
        assert_eq!(
            actions,
            [
                NoticeAction::Close(NoticeKey::Chat(chat_id(1))),
                NoticeAction::Close(NoticeKey::Backlog)
            ]
        );
        assert!(receive(&mut notices, message(2, 1, "b"), summary(1, 2), None).is_empty());
        assert!(notices.set_content(NotificationContent::Message).is_empty());
    }
}
