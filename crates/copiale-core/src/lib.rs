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
pub mod signal;
pub mod storage;

/// Reverse-DNS application ID: Wayland app ID, macOS bundle ID, keychain
/// service, Flatpak ID.
pub const APP_ID: &str = "page.codeberg.korba.Copiale";

pub use actor::{
    Command, CoreHandle, CoreOptions, CoreStopped, DELETE_WINDOW, EDIT_WINDOW, Event, Events,
    StorageFailure, TYPING_TIMEOUT, spawn, spawn_with_options,
};
pub use backend::{
    BackendError, Incoming, Linker, PeerIdentity, Resendable, RetryRequest, SignalBackend,
    StoredChats, Undecryptable,
};
pub use clock::{Clock, SystemClock};
pub use dirs::{create_private_dir, runtime_dir};
pub use model::{
    Account, AccountSettings, Attachment, BlockedList, Change, ChatRef, ChatState, Contact,
    Content, ConversationId, ConversationSummary, Destination, Direction, Expiry, GroupId,
    GroupInfo, GroupMasterKey, GroupMembers, GroupRef, GroupUpdate, Identity, IdentityKey,
    IncomingMessage, Member, Message, MessageBody, MessageChange, MessageId, MessagePreview,
    MessageRef, MessageStatus, Outgoing, Peer, PhoneNumber, Quote, Quoted, ReceiptKind,
    RequestResponse, SafetyNumber, SentMessage, ServiceId, StoredChat, Timestamp, Unsupported,
    Verification,
};
