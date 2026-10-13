//! Signal layer, models, storage and async runtime for Copiale.
//!
//! Has no UI dependencies; the UI talks to it through commands and events.

mod actor;
mod backend;
mod model;

pub use actor::{Command, CoreHandle, CoreStopped, Event, Events, spawn};
pub use backend::{BackendError, Incoming, SignalBackend};
pub use model::{IncomingMessage, MessageBody, ServiceId, Timestamp};
