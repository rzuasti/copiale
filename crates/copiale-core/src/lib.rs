//! Signal layer, models, storage and async runtime for Copiale.
//!
//! Has no UI dependencies; the UI talks to it through commands and events.

mod actor;
mod backend;
pub mod config;
mod dirs;
pub mod logging;
mod model;
pub mod signal;
pub mod storage;

/// Reverse-DNS application ID: Wayland app ID, macOS bundle ID, keychain
/// service, Flatpak ID.
pub const APP_ID: &str = "page.codeberg.korba.Copiale";

pub use actor::{
    Command, CoreHandle, CoreOptions, CoreStopped, Event, Events, StorageFailure, spawn,
    spawn_with_options,
};
pub use backend::{BackendError, Incoming, Linker, SignalBackend};
pub use model::{
    ConversationId, ConversationSummary, Destination, Direction, GroupId, GroupInfo,
    GroupMasterKey, GroupRef, IncomingMessage, Member, Message, MessageBody, MessageId,
    MessagePreview, MessageStatus, Peer, PhoneNumber, SentMessage, ServiceId, Timestamp,
};
