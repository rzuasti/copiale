//! Signal layer, models, storage and async runtime for Copiale.
//!
//! Has no UI dependencies; the UI talks to it through commands and events.

mod actor;
mod backend;
mod clock;
pub mod config;
mod dirs;
pub mod hardening;
pub mod logging;
mod model;
mod outgoing;
pub mod signal;
pub mod storage;
pub mod text;
pub mod video;

/// Reverse-DNS application ID: Wayland app ID, macOS bundle ID, keychain
/// service, Flatpak ID.
pub const APP_ID: &str = "page.codeberg.korba.Copiale";

pub use actor::{
    Command, CoreHandle, CoreOptions, CoreStopped, DELETE_WINDOW, EDIT_WINDOW, Event, Events,
    MAX_GROUP_MEMBERS, MAX_RECENT_EMOJI, MAX_SENT_EDITS, StorageFailure, TYPING_TIMEOUT, spawn,
    spawn_with_options,
};
pub use backend::{
    Ack, BackendError, CreatedGroup, Incoming, Linker, PeerIdentity, Resendable, RetryRequest,
    SignalBackend, StoredChats, Undecryptable,
};
pub use clock::{Clock, SystemClock};
pub use dirs::{create_private_dir, downloads_dir, runtime_dir};
pub use model::{
    Account, AccountSettings, Attachment, AttachmentId, AttachmentKind, AttachmentSource,
    AttachmentState, BlockedList, Change, ChatRef, ChatState, Contact, Content, ConversationId,
    ConversationSummary, Destination, Direction, Expiry, FileContents, GroupId, GroupInfo,
    GroupMasterKey, GroupMembers, GroupRef, GroupUpdate, Identity, IdentityKey, IncomingMessage,
    MAX_ATTACHMENT_SIZE, MAX_ATTACHMENTS, Member, Message, MessageBody, MessageChange, MessageId,
    MessagePreview, MessageRef, MessageStatus, NewAttachment, Outgoing, Peer, PhoneNumber, Quote,
    Quoted, ReceiptKind, RecentEmoji, RemoteAttachment, RequestResponse, SafetyNumber, SentMessage,
    ServiceId, StoredChat, StoredIdentity, Timestamp, Unsupported, Verification,
};
