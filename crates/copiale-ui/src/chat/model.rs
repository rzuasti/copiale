//! Chat state: chat list, open conversation and connection, driven by core
//! events, and the commands user actions produce. No GPUI; the views
//! render it.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use copiale_core::{
    BackendError, Command, Contact, Content, ConversationId, ConversationSummary, DELETE_WINDOW,
    Direction, EDIT_WINDOW, Event, Message, MessageBody, MessageId, MessageStatus, Peer, ServiceId,
    TYPING_TIMEOUT, Timestamp,
};

use super::content::{placeholders, quote_line, system_lines};
use super::new_chat::{Candidate, candidates};
use super::palette::{self, Entry, PaletteAction};
use super::time::{Clock, countdown, countdown_changes_in};

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
        retry_at: Timestamp,
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
    /// The row at this index is gone; the one now there may look different.
    Removed(usize),
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
    /// Sender and time of the message's group; follow-ups indent by its width.
    pub prefix: Prefix,
    /// Whether the message starts its group, so shows the prefix.
    pub leads: bool,
    /// The message a reply quotes, as one line above it.
    pub quote: Option<String>,
    /// Muted lines for content the log can't show, above any text.
    pub placeholders: Vec<String>,
    /// System lines (group and timer changes); the row shows only these.
    pub system: Vec<String>,
    /// For disappearing messages, time left: `◷ 4m`, or the whole timer
    /// until the countdown starts.
    pub timer: Option<String>,
    /// Day label, on the first message of a day.
    pub day: Option<String>,
    /// Why sending failed, if it failed this session.
    pub error: Option<&'a BackendError>,
    /// What can be done to the message.
    pub actions: Actions,
    /// Selected from the keyboard.
    pub selected: bool,
    /// Asking how to delete it.
    pub confirming_delete: bool,
    /// An edit or delete of it didn't reach everyone.
    pub change_failed: bool,
}

/// What can be done to a message; deleting for this account always can.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Actions {
    pub reply: bool,
    pub edit: bool,
    pub delete_for_everyone: bool,
}

/// What the composer sends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Compose {
    #[default]
    New,
    Reply(MessageId),
    Edit(MessageId),
}

/// What covers the window, taking the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overlay {
    NewChat,
    Palette,
    Settings,
}

/// A picker's query and highlighted entry.
#[derive(Debug, Default)]
struct Picker {
    query: String,
    selected: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prefix {
    pub sender: String,
    /// The sender, unless it's us.
    pub peer: Option<ServiceId>,
    pub time: String,
}

#[derive(Debug)]
pub struct Chat {
    clock: Clock,
    /// Most recently active first.
    conversations: Vec<ConversationSummary>,
    search: String,
    /// The search's highlighted match.
    search_selected: usize,
    open: Option<Log>,
    connection: Connection,
    /// Who is typing where, until when.
    typing: Vec<Typing>,
    /// A conversation has been opened since starting.
    opened: bool,
    /// Synced contacts, by name.
    contacts: Vec<Contact>,
    overlay: Option<Overlay>,
    /// The new chat picker's or palette's state.
    picker: Picker,
    /// Chosen in the picker; opens once its conversation exists.
    pending_peer: Option<ServiceId>,
    /// A conversation for the view to open.
    requested_open: Option<ConversationId>,
}

#[derive(Debug)]
struct Typing {
    conversation: ConversationId,
    member: ServiceId,
    until: Timestamp,
}

/// The open conversation's loaded messages.
#[derive(Debug)]
struct Log {
    id: ConversationId,
    /// Oldest first, by sent time.
    messages: Vec<Message>,
    /// Index of each message's group's first message.
    starts: Vec<usize>,
    /// The newest page has arrived.
    loaded: bool,
    /// Older messages may be in storage.
    has_older: bool,
    loading_older: bool,
    errors: HashMap<MessageId, BackendError>,
    compose: Compose,
    /// Selected from the keyboard.
    selected: Option<MessageId>,
    /// Asking how to delete it.
    confirming: Option<MessageId>,
    change_failed: HashSet<MessageId>,
}

impl Log {
    fn regroup(&mut self, clock: &Clock) {
        self.starts.clear();
        for (ix, message) in self.messages.iter().enumerate() {
            let start = match ix.checked_sub(1) {
                Some(previous) if continues(clock, &self.messages[previous], message) => {
                    self.starts[previous]
                }
                _ => ix,
            };
            self.starts.push(start);
        }
    }
}

/// Whether `next` joins `previous`'s group: same author, same day, close in
/// time.
fn continues(clock: &Clock, previous: &Message, next: &Message) -> bool {
    !previous.content.is_system()
        && !next.content.is_system()
        && previous.direction == next.direction
        && previous.sender == next.sender
        && clock.day(previous.sent_at) == clock.day(next.sent_at)
        && next
            .sent_at
            .as_millis()
            .saturating_sub(previous.sent_at.as_millis())
            <= GROUP_GAP_MILLIS
}

impl Chat {
    pub fn new(clock: Clock) -> Self {
        Self {
            clock,
            conversations: Vec::new(),
            search: String::new(),
            search_selected: 0,
            open: None,
            connection: Connection::Connecting,
            typing: Vec::new(),
            opened: false,
            contacts: Vec::new(),
            overlay: None,
            picker: Picker::default(),
            pending_peer: None,
            requested_open: None,
        }
    }

    pub fn clock(&self) -> &Clock {
        &self.clock
    }

    /// Shows times as "2:05 PM" instead of "14:05".
    pub fn set_twelve_hour(&mut self, twelve_hour: bool) {
        self.clock.set_twelve_hour(twelve_hour);
    }

    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    /// "online", or "offline · retrying in 5s" counting down to
    /// "offline · retrying…".
    pub fn connection_label(&self, now: Timestamp) -> String {
        match self.connection {
            Connection::Connecting => "connecting…".into(),
            Connection::Syncing => "syncing…".into(),
            Connection::Online => "online".into(),
            Connection::Offline { .. } => match self.retry_millis(now) {
                0 => "offline · retrying…".into(),
                millis => format!("offline · retrying in {}s", millis.div_ceil(1000)),
            },
        }
    }

    /// Time until [`Self::connection_label`] next changes by itself, while
    /// counting down.
    pub fn connection_label_changes_in(&self, now: Timestamp) -> Option<Duration> {
        match self.retry_millis(now) {
            0 => None,
            millis => Some(Duration::from_millis((millis - 1) % 1000 + 1)),
        }
    }

    fn retry_millis(&self, now: Timestamp) -> u64 {
        match self.connection {
            Connection::Offline { retry_at } => {
                retry_at.as_millis().saturating_sub(now.as_millis())
            }
            _ => 0,
        }
    }

    /// Whether any conversations are listed, regardless of the search.
    pub fn has_conversations(&self) -> bool {
        self.conversations
            .iter()
            .any(|summary| self.listed(summary))
    }

    /// Conversations matching the search, most recently active first.
    pub fn conversations(&self) -> impl Iterator<Item = &ConversationSummary> {
        let search = self.search.trim().to_lowercase();
        self.conversations.iter().filter(move |summary| {
            self.listed(summary)
                && (search.is_empty() || title(summary).to_lowercase().contains(&search))
        })
    }

    /// Whether any listed chat has unread messages.
    pub fn has_unread(&self) -> bool {
        self.conversations
            .iter()
            .any(|summary| summary.unread > 0 && self.listed(summary))
    }

    /// Listed chats with unread messages, other than the open one.
    pub fn unread_elsewhere(&self) -> usize {
        self.conversations
            .iter()
            .filter(|summary| {
                summary.unread > 0 && self.listed(summary) && Some(summary.id) != self.open_id()
            })
            .count()
    }

    /// Chats without messages are listed only while open.
    fn listed(&self, summary: &ConversationSummary) -> bool {
        summary.last_activity.is_some() || self.open_id() == Some(summary.id)
    }

    pub fn set_search(&mut self, search: &str) {
        self.search = search.into();
        self.search_selected = 0;
    }

    /// The search's highlighted match: the best one until moved.
    pub fn search_selected(&self) -> Option<ConversationId> {
        let count = self.conversations().count();
        let ix = self.search_selected.min(count.checked_sub(1)?);
        self.conversations().nth(ix).map(|summary| summary.id)
    }

    /// Moves the search's highlight, stopping at the ends.
    pub fn search_step(&mut self, step: isize) {
        let last = self.conversations().count().saturating_sub(1);
        self.search_selected = self
            .search_selected
            .min(last)
            .saturating_add_signed(step)
            .min(last);
    }

    /// The chat `step` places from the open one in the list, stopping at
    /// the ends; the first or last one if none is open or listed.
    pub fn step_chat(&self, step: isize) -> Option<ConversationId> {
        let ids: Vec<_> = self.conversations().map(|summary| summary.id).collect();
        let last = ids.len().checked_sub(1)?;
        let ix = match ids.iter().position(|id| Some(*id) == self.open_id()) {
            Some(ix) => ix.saturating_add_signed(step).min(last),
            None if step < 0 => last,
            None => 0,
        };
        Some(ids[ix])
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
        let start = log.starts[ix];
        let first = &log.messages[start];
        let (sender, peer) = match (first.direction, first.sender) {
            (Direction::Incoming, Some(author)) => (self.author_name(author), Some(author)),
            _ => ("you".into(), None),
        };
        let actor = match (message.direction, message.sender) {
            (Direction::Incoming, Some(author)) => self.author_name(author),
            _ => "you".into(),
        };
        let system = system_lines(&message.content, &actor, |id| self.member_name(id));
        Some(Row {
            message,
            prefix: Prefix {
                sender,
                peer,
                time: self.clock.time(first.sent_at),
            },
            leads: start == ix,
            quote: quote_line(&message.content, |author| match author {
                Some(author) => self.author_name(author),
                None => "you".into(),
            }),
            placeholders: placeholders(&message.content),
            system,
            timer: message.expiry.map(|expiry| {
                let left = match expiry.at {
                    Some(at) => at.as_millis().saturating_sub(self.clock.now().as_millis()),
                    None => u64::from(expiry.timer) * 1000,
                };
                format!("◷ {}", countdown(left))
            }),
            day: new_day.then(|| self.clock.day_label(day)),
            error: log.errors.get(&message.id),
            actions: self.actions(message),
            selected: log.selected == Some(message.id),
            confirming_delete: log.confirming == Some(message.id),
            change_failed: log.change_failed.contains(&message.id),
        })
    }

    /// An incoming message's author as the open conversation knows them.
    fn author_name(&self, author: ServiceId) -> String {
        match self.open_conversation() {
            Some(summary) => sender_name(summary, author),
            None => unknown(author),
        }
    }

    /// A group member's name, or a stand-in.
    fn member_name(&self, id: ServiceId) -> String {
        self.open_conversation()
            .and_then(|summary| summary.members.iter().find(|member| member.id == id))
            .and_then(|member| member.name.clone())
            .unwrap_or_else(|| unknown(id))
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
                if let Peer::Contact(peer) = summary.peer
                    && self.pending_peer == Some(peer)
                {
                    self.pending_peer = None;
                    self.requested_open = Some(summary.id);
                }
            }
            Event::Contacts(contacts) => {
                self.contacts = contacts.clone();
                if self.new_chat_open() {
                    self.picker.selected = 0;
                }
            }
            Event::Messages {
                conversation,
                before,
                messages,
            } => return self.page(*conversation, *before, messages),
            Event::MessageAdded(message) => {
                // A message ends its author's typing.
                if let Some(sender) = message.sender {
                    self.typing
                        .retain(|t| t.conversation != message.conversation || t.member != sender);
                }
                return self.added(message, active);
            }
            Event::Typing {
                conversation,
                member,
                typing,
            } => {
                let now = self.clock.now();
                self.typing.retain(|t| {
                    t.until > now && (t.conversation != *conversation || t.member != *member)
                });
                if *typing {
                    let timeout = u64::try_from(TYPING_TIMEOUT.as_millis()).unwrap_or(u64::MAX);
                    self.typing.push(Typing {
                        conversation: *conversation,
                        member: *member,
                        until: Timestamp::from_millis(now.as_millis().saturating_add(timeout)),
                    });
                }
            }
            Event::MessageUpdated(message) => return self.updated(message, None),
            Event::MessageRemoved { conversation, id } => {
                return self.removed(*conversation, *id);
            }
            Event::ConversationRemoved(id) => {
                self.conversations.retain(|summary| summary.id != *id);
                self.typing.retain(|typing| typing.conversation != *id);
                if self.open_id() == Some(*id) {
                    self.open = None;
                    return Effects::log(LogChange::Reset(0));
                }
            }
            Event::SendFailed { message, error } => {
                return self.updated(message, Some(error.clone()));
            }
            Event::ChangeFailed { id, .. } => {
                if let Some(ix) = self.open.as_mut().and_then(|log| {
                    log.change_failed.insert(*id);
                    log.messages.iter().position(|m| m.id == *id)
                }) {
                    return Effects::log(LogChange::Changed(ix));
                }
            }
            Event::Connected => self.connection = Connection::Syncing,
            Event::Synced => self.connection = Connection::Online,
            Event::Disconnected { retry_in, .. } => {
                let retry_in = u64::try_from(retry_in.as_millis()).unwrap_or(u64::MAX);
                self.connection = Connection::Offline {
                    retry_at: Timestamp::from_millis(
                        self.clock.now().as_millis().saturating_add(retry_in),
                    ),
                };
            }
            _ => {}
        }
        Effects::default()
    }

    /// Opens the most recently active conversation, once, if none has been
    /// opened yet; marks it read only if `active`.
    pub fn open_latest(&mut self, active: bool) -> Option<Effects> {
        if self.opened {
            return None;
        }
        let latest = self
            .conversations
            .iter()
            .find(|summary| summary.last_activity.is_some())?
            .id;
        let mut effects = self.open(latest);
        if !active {
            effects
                .commands
                .retain(|command| !matches!(command, Command::MarkRead(_)));
        }
        Some(effects)
    }

    pub fn overlay(&self) -> Option<Overlay> {
        self.overlay
    }

    /// Shows `overlay` in place of any other, with an empty query.
    pub fn open_overlay(&mut self, overlay: Overlay) {
        self.overlay = Some(overlay);
        self.picker = Picker::default();
    }

    pub fn close_overlay(&mut self) {
        self.overlay = None;
    }

    /// Opens the new chat picker; refreshes contacts.
    pub fn start_new_chat(&mut self) -> Command {
        self.open_overlay(Overlay::NewChat);
        Command::LoadContacts
    }

    /// Whether any contacts have synced.
    pub fn has_contacts(&self) -> bool {
        !self.contacts.is_empty()
    }

    pub fn new_chat_open(&self) -> bool {
        self.overlay == Some(Overlay::NewChat)
    }

    /// Sets the open picker's query, highlighting the best match.
    pub fn set_picker_query(&mut self, query: &str) {
        self.picker.query = query.into();
        self.picker.selected = 0;
    }

    /// Contacts matching the picker's query, best first.
    pub fn new_chat_candidates(&self) -> Vec<Candidate<'_>> {
        if !self.new_chat_open() {
            return Vec::new();
        }
        candidates(&self.contacts, &self.picker.query, |contact| {
            self.chat_with(contact.id).is_some()
        })
    }

    /// The open picker's highlighted entry.
    pub fn picker_selected(&self) -> Option<usize> {
        matches!(self.overlay, Some(Overlay::NewChat | Overlay::Palette))
            .then_some(self.picker.selected)
    }

    /// Moves the open picker's highlight, stopping at the ends.
    pub fn picker_step(&mut self, step: isize) {
        let count = match self.overlay {
            Some(Overlay::NewChat) => self.new_chat_candidates().len(),
            Some(Overlay::Palette) => self.palette_entries().len(),
            _ => return,
        };
        let last = count.saturating_sub(1);
        self.picker.selected = self.picker.selected.saturating_add_signed(step).min(last);
    }

    /// Picks candidate `ix`, or the highlighted one, and closes the
    /// picker: its chat opens through [`Self::take_requested_open`], once
    /// the core has created it if new.
    pub fn choose_new_chat(&mut self, ix: Option<usize>) -> Option<Command> {
        if !self.new_chat_open() {
            return None;
        }
        let ix = ix.or(self.picker_selected())?;
        let peer = self.new_chat_candidates().get(ix)?.contact.id;
        self.overlay = None;
        match self.chat_with(peer) {
            Some(id) => {
                self.requested_open = Some(id);
                None
            }
            None => {
                self.pending_peer = Some(peer);
                Some(Command::OpenConversation(peer))
            }
        }
    }

    /// Opens the palette, or closes it if open.
    pub fn toggle_palette(&mut self) {
        if self.overlay == Some(Overlay::Palette) {
            self.overlay = None;
        } else {
            self.open_overlay(Overlay::Palette);
        }
    }

    /// Chats other than the open one, and commands, matching the palette's
    /// query, best first.
    pub fn palette_entries(&self) -> Vec<Entry> {
        if self.overlay != Some(Overlay::Palette) {
            return Vec::new();
        }
        let chats = self
            .conversations
            .iter()
            .filter(|summary| self.listed(summary) && Some(summary.id) != self.open_id())
            .map(|summary| (summary.id, title(summary)));
        palette::entries(chats, &self.picker.query)
    }

    /// Picks entry `ix`, or the highlighted one, and closes the palette.
    pub fn choose_palette(&mut self, ix: Option<usize>) -> Option<PaletteAction> {
        let ix = ix.or(self.picker_selected())?;
        let action = self.palette_entries().get(ix)?.action;
        self.overlay = None;
        Some(action)
    }

    /// A conversation the view should open, e.g. one chosen in the picker.
    pub fn take_requested_open(&mut self) -> Option<ConversationId> {
        self.requested_open.take()
    }

    fn chat_with(&self, peer: ServiceId) -> Option<ConversationId> {
        self.conversations
            .iter()
            .find(|summary| summary.peer == Peer::Contact(peer))
            .map(|summary| summary.id)
    }

    /// Shows a conversation, loads its newest messages and marks it read.
    pub fn open(&mut self, id: ConversationId) -> Effects {
        if self.open_id() == Some(id) {
            return Effects::default();
        }
        let Some(summary) = self.conversations.iter().find(|c| c.id == id) else {
            return Effects::default();
        };
        let unread = summary.unread > 0;
        self.opened = true;
        self.open = Some(Log {
            id,
            messages: Vec::new(),
            starts: Vec::new(),
            loaded: false,
            has_older: false,
            loading_older: false,
            errors: HashMap::new(),
            compose: Compose::New,
            selected: None,
            confirming: None,
            change_failed: HashSet::new(),
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
    /// Who is typing in the open conversation, as a line to show.
    pub fn typing_label(&self) -> Option<String> {
        let open = self.open_id()?;
        let now = self.clock.now();
        let names: Vec<_> = self
            .typing
            .iter()
            .filter(|t| t.conversation == open && t.until > now)
            .map(|t| self.author_name(t.member))
            .collect();
        match names.as_slice() {
            [] => None,
            [one] => Some(format!("{one} is typing…")),
            [one, two] => Some(format!("{one} and {two} are typing…")),
            more => Some(format!("{} people are typing…", more.len())),
        }
    }

    /// Whether someone is typing in a conversation.
    pub fn is_typing(&self, conversation: ConversationId) -> bool {
        let now = self.clock.now();
        self.typing
            .iter()
            .any(|t| t.conversation == conversation && t.until > now)
    }

    /// Time until the next typing indicator expires.
    pub fn typing_changes_in(&self) -> Option<Duration> {
        let now = self.clock.now();
        self.typing
            .iter()
            .filter(|t| t.until > now)
            .map(|t| Duration::from_millis(t.until.as_millis() - now.as_millis()))
            .min()
    }

    /// Time until a countdown in the open conversation next changes.
    pub fn timers_change_in(&self) -> Option<Duration> {
        let now = self.clock.now().as_millis();
        self.open
            .as_ref()?
            .messages
            .iter()
            .filter_map(|message| message.expiry?.at)
            .filter(|at| at.as_millis() > now)
            .map(|at| countdown_changes_in(at.as_millis() - now))
            .min()
    }

    /// Tells the core whether we're typing, given the composer's text.
    pub fn typing_command(&self, text: &str) -> Option<Command> {
        Some(Command::Typing {
            conversation: self.open_id()?,
            typing: !text.trim().is_empty(),
        })
    }

    /// The command for the composer's text: a message, reply or edit.
    /// Ends a reply or edit.
    pub fn submit(&mut self, text: &str) -> Option<Command> {
        let log = self.open.as_mut()?;
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        let body = MessageBody::new(text);
        Some(match std::mem::take(&mut log.compose) {
            Compose::New => Command::SendText {
                conversation: log.id,
                body,
                reply_to: None,
            },
            Compose::Reply(id) => Command::SendText {
                conversation: log.id,
                body,
                reply_to: Some(id),
            },
            Compose::Edit(id) => Command::Edit { id, body },
        })
    }

    /// What can be done to `message` now.
    pub fn actions(&self, message: &Message) -> Actions {
        let now = self.clock.now().as_millis();
        let within = |window: Duration| {
            u128::from(now.saturating_sub(message.sent_at.as_millis())) < window.as_millis()
        };
        let own_sent = message.sender.is_none()
            && message.direction == Direction::Outgoing
            && matches!(
                message.status,
                Some(MessageStatus::Sent | MessageStatus::Delivered | MessageStatus::Read)
            );
        let shown = !message.content.is_system() && message.content != Content::Deleted;
        let plain_text = matches!(
            &message.content,
            Content::Text { attachments, .. } if attachments.is_empty()
        );
        Actions {
            reply: shown,
            edit: own_sent && plain_text && within(EDIT_WINDOW),
            delete_for_everyone: own_sent && shown && within(DELETE_WINDOW),
        }
    }

    pub fn compose(&self) -> Compose {
        self.open
            .as_ref()
            .map(|log| log.compose)
            .unwrap_or_default()
    }

    /// What the composer is doing, as a line above it.
    pub fn compose_banner(&self) -> Option<String> {
        let log = self.open.as_ref()?;
        match log.compose {
            Compose::New => None,
            Compose::Edit(_) => Some("editing your message".into()),
            Compose::Reply(id) => {
                let message = log.messages.iter().find(|m| m.id == id)?;
                let name = match message.sender {
                    Some(author) => self.author_name(author),
                    None => "yourself".into(),
                };
                let text = match &message.content {
                    Content::Text { body, .. } => body
                        .as_str()
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" "),
                    _ => String::new(),
                };
                Some(format!("↳ replying to {name}: {text}"))
            }
        }
    }

    /// Replies to a message; `false` if it can't be replied to.
    pub fn start_reply(&mut self, id: ConversationId, message: MessageId) -> bool {
        let can = self
            .message(id, message)
            .is_some_and(|m| self.actions(m).reply);
        if let Some(log) = self.open.as_mut().filter(|_| can) {
            log.compose = Compose::Reply(message);
            log.selected = None;
        }
        can
    }

    /// Edits a message; returns its text for the composer, or `None` if it
    /// can't be edited.
    pub fn start_edit(&mut self, id: ConversationId, message: MessageId) -> Option<String> {
        let found = self.message(id, message)?;
        if !self.actions(found).edit {
            return None;
        }
        let text = found.content.body().map(|body| body.as_str().to_string())?;
        let log = self.open.as_mut()?;
        log.compose = Compose::Edit(message);
        log.selected = None;
        Some(text)
    }

    /// Edits our last editable message, for `↑` in an empty composer.
    pub fn edit_last(&mut self) -> Option<String> {
        let log = self.open.as_ref()?;
        let (id, last) = log
            .messages
            .iter()
            .rev()
            .find(|m| m.sender.is_none() && m.direction == Direction::Outgoing)
            .map(|m| (log.id, m.id))?;
        self.start_edit(id, last)
    }

    /// Ends a reply, edit, selection or delete question; `false` if there
    /// was none.
    pub fn cancel(&mut self) -> bool {
        let Some(log) = self.open.as_mut() else {
            return false;
        };
        if log.confirming.take().is_some() || log.selected.take().is_some() {
            return true;
        }
        std::mem::take(&mut log.compose) != Compose::New
    }

    /// Selects the last message that can be acted on; returns its row.
    pub fn select_last(&mut self) -> Option<usize> {
        let log = self.open.as_ref()?;
        let ix = (0..log.messages.len())
            .rev()
            .find(|&ix| !log.messages[ix].content.is_system())?;
        self.select_row(ix)
    }

    /// Moves the selection by `step` rows, skipping system lines; returns
    /// the selected row.
    pub fn select_step(&mut self, step: isize) -> Option<usize> {
        let log = self.open.as_ref()?;
        let current = log
            .selected
            .and_then(|id| log.messages.iter().position(|m| m.id == id))?;
        let mut ix = current;
        loop {
            ix = ix
                .checked_add_signed(step)
                .filter(|ix| *ix < log.messages.len())?;
            if !log.messages[ix].content.is_system() {
                return self.select_row(ix);
            }
        }
    }

    fn select_row(&mut self, ix: usize) -> Option<usize> {
        let log = self.open.as_mut()?;
        log.selected = Some(log.messages.get(ix)?.id);
        log.confirming = None;
        Some(ix)
    }

    pub fn selected(&self) -> Option<MessageId> {
        self.open.as_ref()?.selected
    }

    /// Whether a delete question is open.
    pub fn confirming_delete(&self) -> Option<MessageId> {
        self.open.as_ref()?.confirming
    }

    /// Asks how to delete a message.
    pub fn confirm_delete(&mut self, message: MessageId) {
        if let Some(log) = self.open.as_mut() {
            log.confirming = Some(message);
        }
    }

    /// Deletes the message being asked about.
    pub fn delete(&mut self, everyone: bool) -> Option<Command> {
        let open = self.open_id()?;
        let id = self.open.as_mut()?.confirming.take()?;
        if everyone
            && !self
                .message(open, id)
                .is_some_and(|m| self.actions(m).delete_for_everyone)
        {
            return None;
        }
        if let Some(log) = self.open.as_mut() {
            log.selected = None;
        }
        Some(Command::Delete { id, everyone })
    }

    /// The row of a message in the open conversation.
    pub fn row_of(&self, message: MessageId) -> Option<usize> {
        let log = self.open.as_ref()?;
        log.messages.iter().position(|m| m.id == message)
    }

    fn message(&self, conversation: ConversationId, id: MessageId) -> Option<&Message> {
        let log = self.open.as_ref().filter(|log| log.id == conversation)?;
        log.messages.iter().find(|m| m.id == id)
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
                log.regroup(&self.clock);
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
                log.regroup(&self.clock);
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
        log.regroup(&self.clock);
        let mut effects = Effects::log(LogChange::Inserted { at, count: 1 });
        if active && message.direction == Direction::Incoming {
            effects.commands.push(Command::MarkRead(log.id));
        }
        effects
    }

    fn removed(&mut self, conversation: ConversationId, id: MessageId) -> Effects {
        let Some(log) = self.open.as_mut().filter(|log| log.id == conversation) else {
            return Effects::default();
        };
        let Some(ix) = log.messages.iter().position(|m| m.id == id) else {
            return Effects::default();
        };
        log.messages.remove(ix);
        log.errors.remove(&id);
        log.regroup(&self.clock);
        Effects::log(LogChange::Removed(ix))
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

/// The contact name or group title, or a stand-in.
pub fn title(summary: &ConversationSummary) -> String {
    summary.title.clone().unwrap_or_else(|| match summary.peer {
        Peer::Contact(peer) => unknown(peer),
        Peer::Group(_) => "Unknown group".into(),
    })
}

/// An incoming message's author as `summary` knows them: the contact, or
/// the group member.
pub fn sender_name(summary: &ConversationSummary, author: ServiceId) -> String {
    match summary.peer {
        Peer::Contact(_) => title(summary),
        Peer::Group(_) => summary
            .members
            .iter()
            .find(|member| member.id == author)
            .and_then(|member| member.name.clone())
            .unwrap_or_else(|| unknown(author)),
    }
}

/// A group's members as the header lists them: the first few names and a
/// count of the rest; `None` for 1:1 or before the members are known.
pub fn member_list(summary: &ConversationSummary) -> Option<String> {
    if summary.members.is_empty() {
        return None;
    }
    let names: Vec<_> = summary
        .members
        .iter()
        .take(LISTED_MEMBERS)
        .map(|member| member.name.clone().unwrap_or_else(|| unknown(member.id)))
        .collect();
    let rest = summary.members.len().saturating_sub(LISTED_MEMBERS);
    let mut list = names.join(", ");
    if rest > 0 {
        list.push_str(&format!(" +{rest}"));
    }
    Some(list)
}

/// Members named in [`member_list`].
const LISTED_MEMBERS: usize = 5;

fn unknown(peer: ServiceId) -> String {
    let hex = peer.uuid().simple().to_string();
    format!("Unknown ({})", &hex[hex.len() - 4..])
}

#[cfg(test)]
mod tests {
    use chrono::FixedOffset;
    use copiale_core::{Attachment, Content, Expiry, GroupMasterKey, GroupUpdate, Member};
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

    fn later(millis: u64) -> Timestamp {
        Timestamp::from_millis(NOW + millis)
    }

    fn peer(n: u128) -> ServiceId {
        ServiceId::new(Uuid::from_u128(n))
    }

    fn summary(id: i64, activity: u64, unread: u32, title: Option<&str>) -> ConversationSummary {
        ConversationSummary {
            id: ConversationId::new(id),
            peer: Peer::Contact(peer(id as u128)),
            title: title.map(Into::into),
            members: Vec::new(),
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
            sender: (!outgoing).then(|| peer(1)),
            sent_at: Timestamp::from_millis(sent_at),
            received_at: Timestamp::from_millis(sent_at),
            content: Content::text(format!("message {id}")),
            edited: false,
            read: outgoing,
            status: outgoing.then_some(MessageStatus::Sent),
            expiry: None,
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
        let prefix = |ix| {
            let row = row(ix);
            (row.leads, row.prefix.sender, row.prefix.time)
        };
        let lead = |sender: &str, time: &str| (true, sender.to_string(), time.to_string());
        let follow = |sender: &str, time: &str| (false, sender.to_string(), time.to_string());

        assert_eq!(row(0).day.as_deref(), Some("Yesterday"));
        assert_eq!(prefix(0), lead("Alice", "23:59"));
        assert_eq!(row(1).day.as_deref(), Some("Today"));
        assert_eq!(prefix(1), lead("Alice", "14:00"));
        assert_eq!(row(2).day, None);
        assert_eq!(prefix(2), follow("Alice", "14:00"));
        assert_eq!(prefix(3), lead("Alice", "14:20"));
        assert_eq!(prefix(4), lead("you", "14:21"));
        assert_eq!(row(3).prefix.peer, Some(peer(1)));
        assert_eq!(row(4).prefix.peer, None);
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
        let Some(Command::SendText {
            conversation,
            body,
            reply_to: None,
        }) = chat.submit("  hi\nthere \n")
        else {
            panic!("expected SendText");
        };
        assert_eq!(conversation, ConversationId::new(1));
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
        assert_eq!(chat.connection_label(later(0)), "online");
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
                retry_at: later(4000)
            }
        );
    }

    #[test]
    fn retry_counts_down() {
        let mut chat = chat();
        chat.apply(
            &Event::Disconnected {
                error: None,
                retry_in: Duration::from_secs(16),
            },
            true,
        );

        let label = |millis| chat.connection_label(later(millis));
        assert_eq!(label(0), "offline · retrying in 16s");
        assert_eq!(label(500), "offline · retrying in 16s");
        assert_eq!(label(1000), "offline · retrying in 15s");
        assert_eq!(label(15_999), "offline · retrying in 1s");
        assert_eq!(label(16_000), "offline · retrying…");
        assert_eq!(label(20_000), "offline · retrying…");

        let changes_in = |millis| chat.connection_label_changes_in(later(millis));
        assert_eq!(changes_in(0), Some(Duration::from_secs(1)));
        assert_eq!(changes_in(500), Some(Duration::from_millis(500)));
        assert_eq!(changes_in(15_999), Some(Duration::from_millis(1)));
        assert_eq!(changes_in(16_000), None);

        chat.apply(&Event::Connected, true);
        assert_eq!(chat.connection_label_changes_in(later(0)), None);
    }

    fn group_summary(members: Vec<Member>, title: Option<&str>) -> ConversationSummary {
        ConversationSummary {
            peer: Peer::Group(GroupMasterKey::new([1; 32]).id()),
            members,
            ..summary(1, NOW, 0, title)
        }
    }

    fn member(n: u128, name: Option<&str>) -> Member {
        Member {
            id: peer(n),
            name: name.map(Into::into),
        }
    }

    fn from(id: i64, author: u128, sent_at: u64) -> Message {
        Message {
            sender: Some(peer(author)),
            ..incoming(id, sent_at)
        }
    }

    #[test]
    fn groups_without_a_title_are_unknown_groups() {
        assert_eq!(title(&group_summary(vec![], None)), "Unknown group");
        assert_eq!(title(&group_summary(vec![], Some("Dev"))), "Dev");
    }

    #[test]
    fn member_list_names_the_first_few() {
        assert_eq!(member_list(&summary(1, NOW, 0, Some("Alice"))), None);
        let few = group_summary(vec![member(1, Some("Alice")), member(0xab12, None)], None);
        assert_eq!(member_list(&few).as_deref(), Some("Alice, Unknown (ab12)"));
        let many = group_summary((1..=8).map(|n| member(n, Some("M"))).collect(), None);
        assert_eq!(member_list(&many).as_deref(), Some("M, M, M, M, M +3"));
    }

    #[test]
    fn group_rows_name_and_group_by_author() {
        let mut chat = chat();
        chat.apply(
            &Event::Conversations(vec![group_summary(
                vec![member(1, Some("Alice")), member(2, None)],
                Some("Dev"),
            )]),
            true,
        );
        chat.open(ConversationId::new(1));
        let today = NOW - 5 * MINUTE;
        chat.apply(
            &Event::Messages {
                conversation: ConversationId::new(1),
                before: None,
                messages: vec![
                    from(1, 1, today),
                    from(2, 1, today + MINUTE),
                    from(3, 2, today + 2 * MINUTE),
                    from(4, 3, today + 3 * MINUTE),
                ],
            },
            true,
        );
        let row = |ix| chat.row(ix).unwrap();

        assert_eq!(row(0).prefix.sender, "Alice");
        assert_eq!(row(0).prefix.peer, Some(peer(1)));
        assert!(!row(1).leads);
        assert!(row(2).leads, "another author starts a group");
        assert_eq!(row(2).prefix.sender, "Unknown (0002)");
        assert_eq!(row(3).prefix.sender, "Unknown (0003)", "no longer a member");
    }

    #[test]
    fn system_rows_stand_alone_and_name_the_actor() {
        let mut chat = chat();
        chat.apply(
            &Event::Conversations(vec![group_summary(
                vec![member(1, Some("Alice")), member(2, Some("Bob"))],
                Some("Dev"),
            )]),
            true,
        );
        chat.open(ConversationId::new(1));
        let today = NOW - 5 * MINUTE;
        let update = Message {
            content: Content::GroupChanged(vec![GroupUpdate::Added(peer(2))]),
            ..from(2, 1, today + MINUTE)
        };
        let mine = Message {
            direction: Direction::Outgoing,
            sender: None,
            status: None,
            content: Content::TimerChanged(None),
            ..from(4, 1, today + 3 * MINUTE)
        };
        let image = Message {
            content: Content::Text {
                body: MessageBody::new("look"),
                attachments: vec![Attachment::Image],
                quote: None,
            },
            ..from(3, 1, today + 2 * MINUTE)
        };
        chat.apply(
            &Event::Messages {
                conversation: ConversationId::new(1),
                before: None,
                messages: vec![from(1, 1, today), update, image, mine],
            },
            true,
        );
        let row = |ix| chat.row(ix).unwrap();

        assert_eq!(row(1).system, ["Alice added Bob"]);
        assert!(row(1).leads);
        assert!(row(2).leads, "a system line breaks the group");
        assert_eq!(row(2).placeholders, ["[image] open on your phone"]);
        assert!(row(2).system.is_empty());
        assert_eq!(row(3).system, ["you turned off disappearing messages"]);
    }

    fn typing(chat: &mut Chat, member: u128, typing: bool) {
        chat.apply(
            &Event::Typing {
                conversation: ConversationId::new(1),
                member: peer(member),
                typing,
            },
            true,
        );
    }

    #[test]
    fn typing_shows_names_and_expires() {
        let mut chat = chat();
        chat.apply(
            &Event::Conversations(vec![group_summary(
                vec![
                    member(1, Some("Alice")),
                    member(2, Some("Bob")),
                    member(3, None),
                ],
                Some("Dev"),
            )]),
            true,
        );
        chat.open(ConversationId::new(1));
        assert_eq!(chat.typing_label(), None);

        typing(&mut chat, 1, true);
        assert_eq!(chat.typing_label().as_deref(), Some("Alice is typing…"));
        assert!(chat.is_typing(ConversationId::new(1)));
        assert!(!chat.is_typing(ConversationId::new(2)));
        typing(&mut chat, 2, true);
        assert_eq!(
            chat.typing_label().as_deref(),
            Some("Alice and Bob are typing…")
        );
        typing(&mut chat, 3, true);
        assert_eq!(chat.typing_label().as_deref(), Some("3 people are typing…"));
        typing(&mut chat, 3, false);
        // A message from Bob ends his typing.
        chat.apply(&Event::MessageAdded(from(9, 2, NOW)), true);
        assert_eq!(chat.typing_label().as_deref(), Some("Alice is typing…"));
        assert_eq!(chat.typing_changes_in(), Some(TYPING_TIMEOUT));

        chat.clock = Clock::fixed(
            FixedOffset::east_opt(2 * 3600).unwrap(),
            Timestamp::from_millis(NOW + TYPING_TIMEOUT.as_millis() as u64),
        );
        assert_eq!(chat.typing_label(), None);
        assert_eq!(chat.typing_changes_in(), None);
    }

    #[test]
    fn composer_text_sets_typing() {
        let mut chat = with_conversations();
        assert!(chat.typing_command("hi").is_none(), "no open conversation");
        chat.open(ConversationId::new(1));
        let typing = |text| match chat.typing_command(text) {
            Some(Command::Typing {
                conversation,
                typing,
            }) => {
                assert_eq!(conversation, ConversationId::new(1));
                typing
            }
            other => panic!("expected Typing, got {other:?}"),
        };
        assert!(typing("h"));
        assert!(!typing("  "));
    }

    #[test]
    fn removed_messages_leave_the_log() {
        let today = NOW - 5 * MINUTE;
        let mut chat = opened(vec![
            incoming(1, today),
            incoming(2, today + MINUTE),
            incoming(3, today + 2 * MINUTE),
        ]);
        let removed = chat.apply(
            &Event::MessageRemoved {
                conversation: ConversationId::new(1),
                id: MessageId::new(1),
            },
            true,
        );
        assert_eq!(removed.log, [LogChange::Removed(0)]);
        assert_eq!(chat.row_count(), 2);
        assert!(chat.row(0).unwrap().leads, "the next message leads now");
        let elsewhere = chat.apply(
            &Event::MessageRemoved {
                conversation: ConversationId::new(2),
                id: MessageId::new(2),
            },
            true,
        );
        assert!(elsewhere.log.is_empty());
    }

    #[test]
    fn removed_conversations_close() {
        let mut chat = opened(vec![incoming(1, NOW)]);
        let removed = chat.apply(&Event::ConversationRemoved(ConversationId::new(1)), true);
        assert_eq!(removed.log, [LogChange::Reset(0)]);
        assert_eq!(chat.open_id(), None);
        assert_eq!(titles(&chat), ["Unknown (0002)"]);
    }

    fn mine(id: i64, sent_at: u64) -> Message {
        Message {
            sender: None,
            ..message(id, Direction::Outgoing, sent_at)
        }
    }

    #[test]
    fn actions_follow_ownership_status_and_age() {
        let chat = chat();
        let recent = NOW - MINUTE;
        let actions = |message: &Message| chat.actions(message);
        assert_eq!(
            actions(&mine(1, recent)),
            Actions {
                reply: true,
                edit: true,
                delete_for_everyone: true,
            }
        );
        assert_eq!(
            actions(&incoming(2, recent)),
            Actions {
                reply: true,
                edit: false,
                delete_for_everyone: false,
            }
        );
        let pending = Message {
            status: Some(MessageStatus::Pending),
            ..mine(3, recent)
        };
        assert!(!actions(&pending).edit && !actions(&pending).delete_for_everyone);
        let old = mine(4, NOW - 25 * 60 * MINUTE);
        assert!(!actions(&old).edit && !actions(&old).delete_for_everyone);
        let deleted = Message {
            content: Content::Deleted,
            ..mine(5, recent)
        };
        assert_eq!(actions(&deleted), Actions::default());
        let image = Message {
            content: Content::Text {
                body: MessageBody::new("look"),
                attachments: vec![Attachment::Image],
                quote: None,
            },
            ..mine(6, recent)
        };
        assert!(!actions(&image).edit && actions(&image).delete_for_everyone);
    }

    #[test]
    fn replies_and_edits_change_what_submit_sends() {
        let recent = NOW - 2 * MINUTE;
        let mut chat = opened(vec![incoming(1, recent), mine(2, recent + MINUTE)]);
        let open = ConversationId::new(1);

        assert!(chat.start_reply(open, MessageId::new(1)));
        assert_eq!(chat.compose(), Compose::Reply(MessageId::new(1)));
        assert_eq!(
            chat.compose_banner().as_deref(),
            Some("↳ replying to Alice: message 1")
        );
        let Some(Command::SendText {
            reply_to: Some(id), ..
        }) = chat.submit("yes")
        else {
            panic!("expected a reply");
        };
        assert_eq!(id, MessageId::new(1));
        assert_eq!(chat.compose(), Compose::New);

        assert_eq!(chat.edit_last().as_deref(), Some("message 2"));
        assert_eq!(
            chat.compose_banner().as_deref(),
            Some("editing your message")
        );
        let Some(Command::Edit { id, body }) = chat.submit("fixed") else {
            panic!("expected an edit");
        };
        assert_eq!((id, body), (MessageId::new(2), MessageBody::new("fixed")));
        assert_eq!(chat.start_edit(open, MessageId::new(1)), None, "not ours");

        chat.start_reply(open, MessageId::new(2));
        assert!(chat.cancel());
        assert_eq!(chat.compose(), Compose::New);
        assert!(!chat.cancel());
    }

    #[test]
    fn selection_skips_system_lines_and_deletes_ask_first() {
        let recent = NOW - 4 * MINUTE;
        let update = Message {
            content: Content::TimerChanged(None),
            ..incoming(2, recent + MINUTE)
        };
        let mut chat = opened(vec![
            incoming(1, recent),
            update,
            mine(3, recent + 2 * MINUTE),
        ]);

        assert_eq!(chat.select_last(), Some(2));
        assert_eq!(chat.select_step(-1), Some(0), "skips the system line");
        assert_eq!(chat.select_step(-1), None);
        assert!(chat.row(0).unwrap().selected);
        assert_eq!(chat.select_step(1), Some(2));

        chat.confirm_delete(MessageId::new(3));
        assert!(chat.row(2).unwrap().confirming_delete);
        let Some(Command::Delete { id, everyone: true }) = chat.delete(true) else {
            panic!("expected a delete for everyone");
        };
        assert_eq!(id, MessageId::new(3));
        assert_eq!(chat.selected(), None);

        chat.confirm_delete(MessageId::new(1));
        assert!(chat.delete(true).is_none(), "not ours");
        chat.confirm_delete(MessageId::new(1));
        assert!(matches!(
            chat.delete(false),
            Some(Command::Delete {
                everyone: false,
                ..
            })
        ));
    }

    #[test]
    fn failed_changes_show_on_their_row() {
        let mut chat = opened(vec![mine(1, NOW - MINUTE)]);
        let effects = chat.apply(
            &Event::ChangeFailed {
                id: MessageId::new(1),
                error: BackendError::new("rejected"),
            },
            true,
        );
        assert_eq!(effects.log, [LogChange::Changed(0)]);
        assert!(chat.row(0).unwrap().change_failed);
    }

    #[test]
    fn disappearing_messages_show_their_time_left() {
        let now = chat().clock().now().as_millis();
        let timed = |id, at: Option<u64>| Message {
            expiry: Some(Expiry {
                timer: 604_800,
                at: at.map(Timestamp::from_millis),
            }),
            ..incoming(id, 100 + id as u64)
        };
        let mut chat = opened(vec![
            incoming(1, 100),
            timed(2, None),
            timed(3, Some(now + 4 * 60_000 + 500)),
            timed(4, Some(now + 90_000)),
        ]);
        let timers: Vec<_> = (0..4).map(|ix| chat.row(ix).unwrap().timer).collect();
        assert_eq!(
            timers,
            [
                None,
                Some("◷ 1w".into()),
                Some("◷ 4m".into()),
                Some("◷ 1m".into())
            ]
        );
        assert_eq!(chat.timers_change_in(), Some(Duration::from_millis(501)));

        chat.apply(
            &Event::MessageRemoved {
                conversation: ConversationId::new(1),
                id: MessageId::new(3),
            },
            true,
        );
        assert_eq!(chat.timers_change_in(), Some(Duration::from_millis(30_001)));
    }

    fn contact(n: u128, name: Option<&str>, phone: Option<&str>) -> Contact {
        Contact {
            id: peer(n),
            name: name.map(Into::into),
            phone: phone.map(copiale_core::PhoneNumber::new),
        }
    }

    /// Conversations 1 (Alice) and 2, with contacts Alice (1), Bob (3)
    /// and a number-only one (4).
    fn picking() -> Chat {
        let mut chat = with_conversations();
        chat.apply(
            &Event::Contacts(vec![
                contact(1, Some("Alice"), Some("+14155550123")),
                contact(3, Some("Bob Builder"), None),
                contact(4, None, Some("+14155550100")),
            ]),
            true,
        );
        assert!(matches!(chat.start_new_chat(), Command::LoadContacts));
        chat
    }

    fn labels(chat: &Chat) -> Vec<String> {
        chat.new_chat_candidates()
            .into_iter()
            .map(|candidate| candidate.label)
            .collect()
    }

    #[test]
    fn the_picker_filters_contacts_fuzzily() {
        let mut chat = picking();
        assert_eq!(labels(&chat), ["Alice", "Bob Builder", "+14155550100"]);
        let alice = &chat.new_chat_candidates()[0];
        assert!(alice.existing);
        assert_eq!(alice.detail, Some("+14155550123"));

        chat.set_picker_query("bb");
        let candidates = chat.new_chat_candidates();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].highlights, [0..1, 4..5]);
        assert!(!candidates[0].existing);

        // Numbers match, unhighlighted under a name.
        chat.set_picker_query("0123");
        let candidates = chat.new_chat_candidates();
        assert_eq!(candidates[0].label, "Alice");
        assert!(candidates[0].highlights.is_empty());

        chat.set_picker_query("zz");
        assert!(chat.new_chat_candidates().is_empty());
        assert!(chat.choose_new_chat(None).is_none());
    }

    #[test]
    fn the_picker_highlight_moves_within_the_list() {
        let mut chat = picking();
        assert_eq!(chat.picker_selected(), Some(0));
        chat.picker_step(-1);
        assert_eq!(chat.picker_selected(), Some(0));
        chat.picker_step(5);
        assert_eq!(chat.picker_selected(), Some(2));
        chat.set_picker_query("b");
        assert_eq!(chat.picker_selected(), Some(0), "a new query starts over");
        chat.close_overlay();
        assert!(!chat.new_chat_open());
        assert_eq!(chat.picker_selected(), None);
    }

    #[test]
    fn choosing_an_existing_chat_opens_it() {
        let mut chat = picking();
        assert!(chat.choose_new_chat(Some(0)).is_none());
        assert!(!chat.new_chat_open());
        assert_eq!(chat.take_requested_open(), Some(ConversationId::new(1)));
        assert_eq!(chat.take_requested_open(), None);
    }

    #[test]
    fn choosing_a_new_contact_opens_the_chat_once_created() {
        let mut chat = picking();
        chat.picker_step(1);
        assert!(matches!(
            chat.choose_new_chat(None),
            Some(Command::OpenConversation(id)) if id == peer(3)
        ));
        assert_eq!(chat.take_requested_open(), None);

        let empty = ConversationSummary {
            last_activity: None,
            ..summary(3, 0, 0, Some("Bob Builder"))
        };
        chat.apply(&Event::ConversationUpdated(empty), true);
        let id = chat.take_requested_open().unwrap();
        assert_eq!(id, ConversationId::new(3));
        assert!(!chat.conversations().any(|c| c.id == id), "empty: unlisted");

        chat.open(id);
        assert!(
            chat.conversations().any(|c| c.id == id),
            "listed while open"
        );
        chat.open(ConversationId::new(1));
        assert!(!chat.conversations().any(|c| c.id == id));
        assert_eq!(chat.conversations().count(), 2);
    }

    #[test]
    fn empty_chats_dont_open_at_start() {
        let mut chat = chat();
        let empty = ConversationSummary {
            last_activity: None,
            ..summary(3, 0, 0, None)
        };
        chat.apply(&Event::Conversations(vec![empty.clone()]), true);
        assert!(!chat.has_conversations());
        assert!(chat.open_latest(true).is_none());
    }

    #[test]
    fn the_latest_conversation_opens_once_at_start() {
        let mut chat = with_conversations();
        let effects = chat.open_latest(false).unwrap();
        assert_eq!(chat.open_id(), Some(ConversationId::new(1)));
        assert!(
            !effects
                .commands
                .iter()
                .any(|command| matches!(command, Command::MarkRead(_))),
            "not read until the window is active"
        );
        assert!(chat.open_latest(true).is_none(), "only once");

        let mut chat = with_conversations();
        let effects = chat.open_latest(true).unwrap();
        assert!(
            effects
                .commands
                .iter()
                .any(|command| matches!(command, Command::MarkRead(_)))
        );
        assert!(chat.open_latest(true).is_none());
        let mut empty = super::tests::chat();
        assert!(empty.open_latest(true).is_none(), "no conversations");
    }

    /// Alice (1), Bob (2) and Carol (3), most recent first.
    fn three_chats() -> Chat {
        let mut chat = chat();
        chat.apply(
            &Event::Conversations(vec![
                summary(1, NOW, 0, Some("Alice")),
                summary(2, NOW - MINUTE, 0, Some("Bob")),
                summary(3, NOW - 2 * MINUTE, 0, Some("Carol")),
            ]),
            true,
        );
        chat
    }

    #[test]
    fn chats_step_through_the_list_stopping_at_the_ends() {
        let mut chat = three_chats();
        let id = ConversationId::new;
        assert_eq!(chat.step_chat(1), Some(id(1)), "none open: the first");
        assert_eq!(chat.step_chat(-1), Some(id(3)), "none open: the last");

        chat.open(id(2));
        assert_eq!(chat.step_chat(1), Some(id(3)));
        assert_eq!(chat.step_chat(-1), Some(id(1)));
        chat.open(id(3));
        assert_eq!(chat.step_chat(1), Some(id(3)));

        // Within the search's matches.
        chat.set_search("a");
        assert_eq!(chat.step_chat(-1), Some(id(1)));
        chat.set_search("zz");
        assert_eq!(chat.step_chat(1), None);
    }

    #[test]
    fn search_highlight_moves_within_the_matches() {
        let mut chat = three_chats();
        let id = ConversationId::new;
        assert_eq!(chat.search_selected(), Some(id(1)));
        chat.search_step(-1);
        assert_eq!(chat.search_selected(), Some(id(1)));
        chat.search_step(5);
        assert_eq!(chat.search_selected(), Some(id(3)));

        chat.set_search("o");
        assert_eq!(
            chat.search_selected(),
            Some(id(2)),
            "a new search starts over"
        );
        chat.search_step(1);
        assert_eq!(chat.search_selected(), Some(id(3)));
        chat.set_search("zz");
        assert_eq!(chat.search_selected(), None);
    }

    fn palette_labels(chat: &Chat) -> Vec<String> {
        chat.palette_entries()
            .into_iter()
            .map(|entry| entry.label)
            .collect()
    }

    #[test]
    fn the_palette_lists_other_chats_then_commands() {
        let mut chat = three_chats();
        chat.open(ConversationId::new(1));
        assert!(chat.palette_entries().is_empty(), "closed");

        chat.toggle_palette();
        assert_eq!(chat.overlay(), Some(Overlay::Palette));
        assert_eq!(&palette_labels(&chat)[..3], ["Bob", "Carol", "New chat"]);
        assert_eq!(chat.picker_selected(), Some(0));

        chat.set_picker_query("car");
        assert_eq!(palette_labels(&chat), ["Carol"]);
        chat.toggle_palette();
        assert_eq!(chat.overlay(), None);
    }

    #[test]
    fn choosing_in_the_palette_closes_it() {
        let mut chat = three_chats();
        chat.open(ConversationId::new(1));
        chat.toggle_palette();
        chat.picker_step(1);
        assert_eq!(
            chat.choose_palette(None),
            Some(PaletteAction::Open(ConversationId::new(3)))
        );
        assert_eq!(chat.overlay(), None);
        assert_eq!(chat.choose_palette(None), None, "closed");

        chat.toggle_palette();
        chat.set_picker_query("quit");
        chat.picker_step(3);
        assert_eq!(chat.picker_selected(), Some(0), "stops at the end");
        assert_eq!(chat.choose_palette(None), Some(PaletteAction::Quit));
    }

    #[test]
    fn overlays_replace_each_other() {
        let mut chat = three_chats();
        chat.toggle_palette();
        chat.set_picker_query("x");
        chat.start_new_chat();
        assert!(chat.new_chat_open());
        assert!(chat.palette_entries().is_empty());
        assert_eq!(chat.picker_selected(), Some(0));
        chat.open_overlay(Overlay::Settings);
        assert_eq!(chat.picker_selected(), None);
        assert!(chat.choose_new_chat(None).is_none());
        chat.close_overlay();
        assert_eq!(chat.overlay(), None);
    }

    #[test]
    fn counts_unread_chats_other_than_the_open_one() {
        let mut chat = three_chats();
        assert_eq!(chat.unread_elsewhere(), 0);
        for (id, unread) in [(1, 2), (2, 1)] {
            chat.apply(
                &Event::ConversationUpdated(summary(id, NOW, unread, None)),
                true,
            );
        }
        assert_eq!(chat.unread_elsewhere(), 2);
        chat.open(ConversationId::new(1));
        assert_eq!(chat.unread_elsewhere(), 1);
        assert!(chat.has_unread());

        for id in [1, 2] {
            chat.apply(&Event::ConversationUpdated(summary(id, NOW, 0, None)), true);
        }
        assert!(!chat.has_unread());
    }
}
