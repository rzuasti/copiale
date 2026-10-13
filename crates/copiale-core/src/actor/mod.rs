//! The core actor: owns storage and the Signal account on a dedicated
//! thread, and walks through startup (unlock, then link) before serving.

mod run;

use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::LocalSet;

use crate::backend::{BackendError, Linker};
use crate::model::{
    ConversationId, ConversationSummary, Message, MessageBody, MessageId, ServiceId, Timestamp,
};
use crate::storage::{Passphrase, SecretStore, Storage};

/// Request from the UI to the core. Ignored when it doesn't fit the
/// current state.
#[derive(Debug)]
pub enum Command {
    /// Unlocks a passphrase-encrypted database ([`Event::Locked`]).
    Unlock(Passphrase),
    /// Sets the passphrase for a new database without a keychain.
    SetPassphrase(Passphrase),
    /// Starts linking as a secondary device ([`Event::Unlinked`]).
    Link { device_name: String },
    /// Stops linking and returns to [`Event::Unlinked`].
    CancelLink,
    /// Tries opening storage again after [`Event::StorageFailed`].
    Retry,
    /// Deletes all local data and starts from scratch, after
    /// [`Event::StorageFailed`]. The UI must confirm first.
    StartOver,
    SendText {
        recipient: ServiceId,
        body: MessageBody,
    },
    /// Reconnects now instead of waiting for the next retry.
    Reconnect,
    /// Answered with [`Event::Conversations`].
    LoadConversations,
    /// Up to `limit` messages before `before` (or the newest); answered
    /// with [`Event::Messages`].
    LoadMessages {
        conversation: ConversationId,
        before: Option<MessageId>,
        limit: u32,
    },
    /// Marks incoming messages read; [`Event::ConversationUpdated`] follows.
    MarkRead(ConversationId),
    /// Stops the core; [`Event::Stopped`] follows.
    Shutdown,
}

/// Notification from the core to the UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// A passphrase is needed: a new one (`true`) or the existing one.
    Locked {
        new_passphrase: bool,
    },
    /// The passphrase didn't open the database; still locked.
    WrongPassphrase,
    /// Storage can't be opened; offer [`Command::Retry`] and
    /// [`Command::StartOver`].
    StorageFailed(StorageFailure),
    /// No linked account; `device_name` is the suggested name.
    Unlinked {
        device_name: String,
    },
    /// Show this provisioning URL as a QR code for the phone to scan.
    LinkUrl(String),
    /// Linking failed; still unlinked.
    LinkFailed(BackendError),
    /// Storage is open and the account linked; messaging events follow.
    Ready,
    /// Connected to Signal; queued messages follow, then [`Event::Synced`].
    Connected,
    /// Queued messages have been delivered; the core is live.
    Synced,
    /// A new message was stored; [`Event::ConversationUpdated`] follows.
    MessageAdded(Message),
    ConversationUpdated(ConversationSummary),
    /// All conversations, most recently active first. Answers
    /// [`Command::LoadConversations`]; also sent when names change.
    Conversations(Vec<ConversationSummary>),
    /// A page of messages, oldest first. Answers [`Command::LoadMessages`].
    Messages {
        conversation: ConversationId,
        before: Option<MessageId>,
        messages: Vec<Message>,
    },
    /// Reading or writing local data failed while running.
    DatabaseError(String),
    MessageSent {
        recipient: ServiceId,
        timestamp: Timestamp,
    },
    SendFailed {
        recipient: ServiceId,
        timestamp: Timestamp,
        error: BackendError,
    },
    /// The connection failed to open (`Some`) or closed (`None`); the next
    /// attempt is in `retry_in`.
    Disconnected {
        error: Option<BackendError>,
        retry_in: Duration,
    },
    /// Last event: the core has stopped and closed storage.
    Stopped,
}

/// Why storage can't be opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StorageFailure {
    /// The database exists but its key is gone from the keychain.
    KeyMissing,
    /// The keychain holding the key can't be reached (missing or locked).
    KeychainUnavailable(String),
    /// The key in the keychain doesn't open the database.
    WrongKey,
    Other(String),
}

/// The core is no longer running.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("core has stopped")]
pub struct CoreStopped;

/// UI-side handle to a running core.
///
/// Dropping it stops the core without waiting; prefer [`Self::shutdown`]
/// before the process exits.
pub struct CoreHandle {
    commands: UnboundedSender<Command>,
    thread: JoinHandle<()>,
}

impl CoreHandle {
    pub fn send(&self, command: Command) -> Result<(), CoreStopped> {
        self.commands.send(command).map_err(|_| CoreStopped)
    }

    /// Stops the core and waits until it has closed storage.
    pub fn shutdown(self) -> std::thread::Result<()> {
        self.commands.send(Command::Shutdown).ok();
        self.thread.join()
    }
}

/// Events from the core, in order.
pub struct Events(UnboundedReceiver<Event>);

impl Events {
    /// Next event; `None` once the core has stopped and all events are read.
    pub async fn recv(&mut self) -> Option<Event> {
        self.0.recv().await
    }
}

/// Core tuning; the defaults suit the app.
#[derive(Clone, Debug)]
pub struct CoreOptions {
    /// First reconnect delay; doubles per failure up to `reconnect_max`.
    pub reconnect_initial: Duration,
    pub reconnect_max: Duration,
}

impl Default for CoreOptions {
    fn default() -> Self {
        Self {
            reconnect_initial: Duration::from_secs(1),
            reconnect_max: Duration::from_secs(60),
        }
    }
}

/// Starts the core on its own thread with default options.
///
/// `linker` runs on that thread, so the linker and its backend need not be
/// `Send`.
pub fn spawn<L, F>(
    storage: Storage,
    secrets: Arc<dyn SecretStore + Send + Sync>,
    linker: F,
) -> std::io::Result<(CoreHandle, Events)>
where
    F: FnOnce() -> L + Send + 'static,
    L: Linker + 'static,
{
    spawn_with_options(storage, secrets, linker, CoreOptions::default())
}

/// [`spawn`] with explicit options.
pub fn spawn_with_options<L, F>(
    storage: Storage,
    secrets: Arc<dyn SecretStore + Send + Sync>,
    linker: F,
    options: CoreOptions,
) -> std::io::Result<(CoreHandle, Events)>
where
    F: FnOnce() -> L + Send + 'static,
    L: Linker + 'static,
{
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let (command_tx, command_rx) = unbounded_channel();
    let (event_tx, event_rx) = unbounded_channel();
    let thread = std::thread::Builder::new()
        .name("copiale-core".into())
        .spawn(move || {
            let core = run::Core {
                storage,
                secrets,
                options,
                commands: command_rx,
                events: event_tx,
            };
            LocalSet::new().block_on(&runtime, core.run(linker()));
        })?;
    let handle = CoreHandle {
        commands: command_tx,
        thread,
    };
    Ok((handle, Events(event_rx)))
}
