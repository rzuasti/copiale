//! Plain data shared between the core and the UI.

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use presage::libsignal_service::zkgroup::groups;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::logging::Tag;

/// Signal account identifier. `Debug` shows only its [`Tag`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
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
        write!(f, "ServiceId({})", Tag::new(self.0.as_bytes()))
    }
}

/// E.164 phone number. `Debug` shows only its [`Tag`]; no `Display`,
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
        write!(f, "PhoneNumber({})", Tag::new(self.0.as_bytes()))
    }
}

/// Public Signal group identifier, derived from the master key. `Debug`
/// shows only its [`Tag`].
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
        write!(f, "GroupId({})", Tag::new(&self.0))
    }
}

/// Groups v2 master key: grants access to the group's state, so it is
/// secret. `Debug` shows nothing of it.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct GroupMasterKey([u8; 32]);

impl GroupMasterKey {
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The group's public identifier.
    pub fn id(&self) -> GroupId {
        let key = groups::GroupMasterKey::new(self.0);
        let params = groups::GroupSecretParams::derive_from_master_key(key);
        GroupId(params.get_group_identifier())
    }
}

impl fmt::Debug for GroupMasterKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GroupMasterKey(…)")
    }
}

/// A group as a message refers to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GroupRef {
    pub key: GroupMasterKey,
    /// The group state's revision when the message was sent.
    pub revision: u32,
}

/// Where a message goes: one account, or a group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Destination {
    Contact(ServiceId),
    Group(GroupRef),
}

/// Message text. `Debug` shows only its length.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
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

/// What a message carries. `Debug` shows only the kind.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Content {
    /// Text, possibly empty, with attachments this client can't show yet,
    /// maybe replying to an earlier message.
    Text {
        body: MessageBody,
        attachments: Vec<Attachment>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        quote: Option<Quote>,
    },
    Sticker {
        emoji: Option<String>,
    },
    Reaction {
        emoji: String,
    },
    /// An incoming call, to answer on the phone.
    Call {
        video: bool,
    },
    /// Other content to open on the phone.
    Unsupported(Unsupported),
    /// Changes to a group, made by the message's author.
    GroupChanged(Vec<GroupUpdate>),
    /// Disappearing messages set to this many seconds; `None` turns them off.
    TimerChanged(Option<u32>),
    /// Deleted by its author.
    Deleted,
    /// Couldn't be decrypted; replaced if the sender resends it.
    Undecryptable,
    /// The author's identity key, so their safety number, changed.
    SafetyNumberChanged,
}

impl Content {
    pub fn text(body: impl Into<String>) -> Self {
        Self::Text {
            body: MessageBody::new(body),
            attachments: Vec::new(),
            quote: None,
        }
    }

    /// The text, if any.
    pub fn body(&self) -> Option<&MessageBody> {
        match self {
            Self::Text { body, .. } if !body.as_str().is_empty() => Some(body),
            _ => None,
        }
    }

    /// Shown as a system line rather than a message from someone.
    pub fn is_system(&self) -> bool {
        matches!(
            self,
            Self::GroupChanged(_) | Self::TimerChanged(_) | Self::SafetyNumberChanged
        )
    }

    /// Arrives read: system lines, reactions and placeholders for what
    /// couldn't be decrypted don't need attention.
    pub fn is_quiet(&self) -> bool {
        self.is_system() || matches!(self, Self::Reaction { .. } | Self::Undecryptable)
    }
}

impl fmt::Debug for Content {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text {
                body,
                attachments,
                quote,
            } => f
                .debug_struct("Text")
                .field("body", body)
                .field("attachments", &attachments.len())
                .field("quote", quote)
                .finish(),
            Self::Sticker { .. } => f.write_str("Sticker"),
            Self::Reaction { .. } => f.write_str("Reaction"),
            Self::Call { video } => f.debug_struct("Call").field("video", video).finish(),
            Self::Unsupported(kind) => f.debug_tuple("Unsupported").field(kind).finish(),
            Self::GroupChanged(updates) => {
                f.debug_tuple("GroupChanged").field(&updates.len()).finish()
            }
            Self::TimerChanged(seconds) => f.debug_tuple("TimerChanged").field(seconds).finish(),
            Self::Deleted => f.write_str("Deleted"),
            Self::Undecryptable => f.write_str("Undecryptable"),
            Self::SafetyNumberChanged => f.write_str("SafetyNumberChanged"),
        }
    }
}

/// The message a reply quotes. Only which one is stored; what it says
/// comes from storage, never from the reply.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quote {
    /// The quoted message's author; `None` for this account.
    pub author: Option<ServiceId>,
    pub sent_at: Timestamp,
    /// The quoted message as stored now; `None` if it isn't, e.g. never
    /// received, deleted or disappeared.
    #[serde(skip)]
    pub original: Option<Quoted>,
}

/// What a quoted text message says. `Debug` hides the text.
#[derive(Clone, PartialEq, Eq)]
pub struct Quoted {
    /// Its text, possibly empty.
    pub text: String,
    /// Its first attachment, for quotes of attachments without text.
    pub attachment: Option<Attachment>,
}

impl fmt::Debug for Quoted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Quoted")
            .field("text_bytes", &self.text.len())
            .field("attachment", &self.attachment)
            .finish()
    }
}

/// A conversation as another device names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChatRef {
    Contact(ServiceId),
    Group(GroupId),
}

/// A message as another device names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MessageRef {
    pub chat: ChatRef,
    /// `None` for this account.
    pub author: Option<ServiceId>,
    pub sent_at: Timestamp,
}

/// What the backend sends: a message, or a change to one of ours.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outgoing {
    Text {
        body: MessageBody,
        quote: Option<Quote>,
    },
    /// New text for our message sent at `target`.
    Edit {
        target: Timestamp,
        body: MessageBody,
        quote: Option<Quote>,
    },
    /// Deletes our message sent at `target` for everyone.
    Delete { target: Timestamp },
}

/// A change to an earlier message by its author.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageChange {
    /// The conversation it's in.
    pub destination: Destination,
    /// The message's author; `None` for this account.
    pub author: Option<ServiceId>,
    /// The message's sent time.
    pub target: Timestamp,
    pub change: Change,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// New content, sent at `at`.
    Edit {
        content: Content,
        at: Timestamp,
    },
    Delete,
}

/// An attachment, by what the placeholder calls it. `Debug` hides file
/// names.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Attachment {
    Image,
    Video,
    Gif,
    Voice,
    Audio,
    File { name: Option<String> },
}

impl fmt::Debug for Attachment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Image => "Image",
            Self::Video => "Video",
            Self::Gif => "Gif",
            Self::Voice => "Voice",
            Self::Audio => "Audio",
            Self::File { .. } => "File",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unsupported {
    Contact,
    Payment,
    Poll,
    ViewOnce,
    GiftBadge,
}

/// One change to a group. The author of the message is who made it.
/// `Debug` hides titles.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupUpdate {
    Title(String),
    Description,
    Avatar,
    /// Disappearing messages, in seconds; `None` turns them off.
    Timer(Option<u32>),
    Added(ServiceId),
    Removed(ServiceId),
    /// The author joined, e.g. by link or accepting an invite.
    Joined,
    /// The author left.
    Left,
    Invited(ServiceId),
    /// The author asked to join.
    Requested,
    Admin {
        member: ServiceId,
        admin: bool,
    },
    /// Permissions, invite link or other settings.
    Settings,
}

impl fmt::Debug for GroupUpdate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Title(_) => f.write_str("Title"),
            Self::Description => f.write_str("Description"),
            Self::Avatar => f.write_str("Avatar"),
            Self::Timer(seconds) => f.debug_tuple("Timer").field(seconds).finish(),
            Self::Added(id) => f.debug_tuple("Added").field(id).finish(),
            Self::Removed(id) => f.debug_tuple("Removed").field(id).finish(),
            Self::Joined => f.write_str("Joined"),
            Self::Left => f.write_str("Left"),
            Self::Invited(id) => f.debug_tuple("Invited").field(id).finish(),
            Self::Requested => f.write_str("Requested"),
            Self::Admin { member, admin } => f
                .debug_struct("Admin")
                .field("member", member)
                .field("admin", admin)
                .finish(),
            Self::Settings => f.write_str("Settings"),
        }
    }
}

/// Milliseconds since the Unix epoch; Signal's message timestamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
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

/// A message received from another account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IncomingMessage {
    pub sender: ServiceId,
    /// The group it was sent in; `None` for 1:1.
    pub group: Option<GroupRef>,
    pub timestamp: Timestamp,
    pub content: Content,
    /// Disappears this many seconds after being read; `None` if it doesn't.
    pub expire_timer: Option<u32>,
}

/// A message this account sent from another device (the phone).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SentMessage {
    pub destination: Destination,
    pub timestamp: Timestamp,
    pub content: Content,
    /// Disappears this many seconds after being sent; `None` if it doesn't.
    pub expire_timer: Option<u32>,
}

/// A group's state as last fetched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupInfo {
    pub title: String,
    pub revision: u32,
    /// Other members, in the group's order; this account is left out.
    pub members: Vec<Member>,
}

/// Who is in a group, as last fetched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupMembers {
    pub revision: u32,
    /// Full members, this account included if it is one; invited ones and
    /// those asking to join are left out.
    pub members: Vec<ServiceId>,
}

/// A group member. `Debug` hides the name.
#[derive(Clone, PartialEq, Eq)]
pub struct Member {
    pub id: ServiceId,
    /// Contact or profile name, if known.
    pub name: Option<String>,
}

impl fmt::Debug for Member {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Member")
            .field("id", &self.id)
            .field("named", &self.name.is_some())
            .finish()
    }
}

/// A contact synced from the phone. `Debug` hides the name and number.
#[derive(Clone, PartialEq, Eq)]
pub struct Contact {
    pub id: ServiceId,
    /// Contact or profile name, if known.
    pub name: Option<String>,
    pub phone: Option<PhoneNumber>,
}

impl fmt::Debug for Contact {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Contact")
            .field("id", &self.id)
            .field("named", &self.name.is_some())
            .field("phone", &self.phone)
            .finish()
    }
}

/// An account's public identity key, as Signal encodes it. `Debug` hides
/// it.
#[derive(Clone, PartialEq, Eq)]
pub struct IdentityKey(Vec<u8>);

impl IdentityKey {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for IdentityKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("IdentityKey(…)")
    }
}

/// The 60 digits two accounts compare to check that nobody sits between
/// them. `Debug` hides them.
#[derive(Clone, PartialEq, Eq)]
pub struct SafetyNumber(String);

impl SafetyNumber {
    pub fn new(digits: impl Into<String>) -> Self {
        Self(digits.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Blocks of five digits, as Signal shows them.
    pub fn blocks(&self) -> Vec<&str> {
        let mut blocks = Vec::new();
        let mut rest = self.0.as_str();
        while !rest.is_empty() {
            let (block, tail) = rest.split_at(rest.len().min(5));
            blocks.push(block);
            rest = tail;
        }
        blocks
    }
}

impl fmt::Debug for SafetyNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SafetyNumber(…)")
    }
}

/// Whether the user checked an account's safety number, on the phone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Verification {
    #[default]
    Unverified,
    /// Verified, for the key sessions with them use.
    Verified,
    /// Verified, but their key changed since: sending to them waits until
    /// the user confirms the new one.
    Changed,
}

/// An account's safety number with this one, and whether it's verified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub peer: ServiceId,
    /// `None` until a session with them exists.
    pub safety_number: Option<SafetyNumber>,
    pub verification: Verification,
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

/// Who a conversation is with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Peer {
    Contact(ServiceId),
    Group(GroupId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Incoming,
    /// Sent by this account, from any of its devices.
    Outgoing,
}

/// Progress of an outgoing message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageStatus {
    /// Waiting to be sent, or sending.
    Pending,
    /// Accepted by the server.
    Sent,
    /// On the recipient's device; in a group, on every member's.
    Delivered,
    /// Read by the recipient; in a group, by every member.
    Read,
    /// Sending failed; can be retried.
    Failed,
}

/// Privacy settings of the account, synced from the phone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AccountSettings {
    /// Send read receipts, and show others'.
    pub read_receipts: bool,
    /// Send typing indicators, and show others'.
    pub typing_indicators: bool,
}

/// This device and its account, as the settings show them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Account {
    /// This device's name, as the phone lists it.
    pub device_name: Option<String>,
    pub settings: AccountSettings,
}

/// Which receipt an account sent for messages of ours.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiptKind {
    Delivered,
    Read,
}

/// A stored message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub id: MessageId,
    pub conversation: ConversationId,
    pub direction: Direction,
    /// Author of an incoming message; `None` for outgoing ones.
    pub sender: Option<ServiceId>,
    /// The sender's timestamp; with the author, Signal's message identity.
    pub sent_at: Timestamp,
    /// When this device stored it, by the local clock.
    pub received_at: Timestamp,
    pub content: Content,
    /// Edited by its author since it was sent.
    pub edited: bool,
    pub read: bool,
    /// Outgoing messages only.
    pub status: Option<MessageStatus>,
    /// Set if the message disappears.
    pub expiry: Option<Expiry>,
}

/// When a disappearing message goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Expiry {
    /// Seconds from the countdown's start: reading an incoming message,
    /// sending an outgoing one.
    pub timer: u32,
    /// When it's deleted; `None` until the countdown starts.
    pub at: Option<Timestamp>,
}

/// Whether the user let a conversation in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChatState {
    /// Accepted, or nothing from the other side yet.
    #[default]
    Accepted,
    /// A message request: an account or group the user hasn't accepted
    /// wrote. No read receipts or typing go out until accepted.
    Request,
    /// Their messages are dropped.
    Blocked,
}

/// How the user answered a message request, here or on the phone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestResponse {
    Accept,
    Delete,
    Block,
    BlockAndDelete,
}

/// Blocked accounts and groups, as the phone syncs the list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockedList {
    pub accounts: Vec<ServiceId>,
    pub groups: Vec<GroupId>,
    /// Numbers blocked without a known account.
    pub numbers: Vec<PhoneNumber>,
}

/// A chat as Storage Service holds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoredChat {
    pub chat: ChatRef,
    /// Accepted ("whitelisted"): shares this account's profile.
    pub accepted: bool,
    pub blocked: bool,
}

/// A conversation as the chat list shows it. `Debug` hides the title.
#[derive(Clone, PartialEq, Eq)]
pub struct ConversationSummary {
    pub id: ConversationId,
    pub peer: Peer,
    pub state: ChatState,
    /// Synced contact name or group title, if known.
    pub title: Option<String>,
    /// A group's other members; empty for 1:1.
    pub members: Vec<Member>,
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
            .field("state", &self.state)
            .field("titled", &self.title.is_some())
            .field("members", &self.members)
            .field("last_activity", &self.last_activity)
            .field("unread", &self.unread)
            .field("last_message", &self.last_message)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessagePreview {
    pub direction: Direction,
    pub content: Content,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_id_debug_shows_only_its_tag() {
        let uuid = Uuid::parse_str("0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0").unwrap();
        let tag = Tag::new(uuid.as_bytes());
        assert_eq!(
            format!("{:?}", ServiceId::new(uuid)),
            format!("ServiceId({tag})")
        );
    }

    #[test]
    fn phone_number_debug_shows_only_its_tag() {
        let tag = Tag::new(b"+14155550123");
        assert_eq!(
            format!("{:?}", PhoneNumber::new("+14155550123")),
            format!("PhoneNumber({tag})")
        );
    }

    #[test]
    fn group_id_debug_shows_only_its_tag() {
        let bytes = [0x11; 32];
        let tag = Tag::new(&bytes);
        assert_eq!(
            format!("{:?}", GroupId::new(bytes)),
            format!("GroupId({tag})")
        );
    }

    #[test]
    fn group_master_key_debug_shows_nothing() {
        let debug = format!("{:?}", GroupMasterKey::new([0xab; 32]));
        assert_eq!(debug, "GroupMasterKey(…)");
    }

    #[test]
    fn group_ids_derive_from_master_keys() {
        let key = GroupMasterKey::new([7; 32]);
        assert_eq!(key.id(), key.id());
        assert_ne!(key.id(), GroupMasterKey::new([8; 32]).id());
        assert_ne!(key.id().as_bytes(), key.as_bytes());
    }

    #[test]
    fn member_debug_hides_the_name() {
        let member = Member {
            id: ServiceId::new(Uuid::from_u128(1)),
            name: Some("Alice Secret".into()),
        };
        assert!(!format!("{member:?}").contains("Alice"));
    }

    #[test]
    fn identity_key_and_safety_number_debug_show_nothing() {
        assert_eq!(
            format!("{:?}", IdentityKey::new(vec![5, 1, 2])),
            "IdentityKey(…)"
        );
        assert_eq!(
            format!("{:?}", SafetyNumber::new("123456789012")),
            "SafetyNumber(…)"
        );
    }

    #[test]
    fn safety_numbers_show_in_blocks_of_five() {
        let digits: String = (0..60).map(|n| char::from(b'0' + n % 10)).collect();
        let blocks = SafetyNumber::new(digits).blocks().len();
        assert_eq!(blocks, 12);
        assert_eq!(
            SafetyNumber::new("012345678901").blocks(),
            ["01234", "56789", "01"]
        );
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
