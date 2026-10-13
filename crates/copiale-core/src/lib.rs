//! Signal layer, models, storage and async runtime for Copiale.
//!
//! Has no UI dependencies; the UI talks to it through commands and events.

mod actor;
mod backend;
pub mod config;
mod dirs;
mod model;
pub mod storage;

/// Reverse-DNS application ID: Wayland app ID, macOS bundle ID, keychain
/// service, Flatpak ID.
pub const APP_ID: &str = "page.codeberg.korba.Copiale";

pub use actor::{Command, CoreHandle, CoreStopped, Event, Events, spawn};
pub use backend::{BackendError, Incoming, SignalBackend};
pub use model::{IncomingMessage, MessageBody, ServiceId, Timestamp};
