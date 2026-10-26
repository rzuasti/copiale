//! The core actor: owns storage and the Signal account on a dedicated
//! thread, and walks through startup (unlock, then link) before serving.

mod downloads;
mod groups;
mod outbox;
mod previews;
mod probe;
mod retries;
mod run;
mod session;
mod uploads;

use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::LocalSet;

use crate::backend::{BackendError, Linker};
use crate::clock::{Clock, SystemClock};
use crate::model::{
    Account, AttachmentId, Contact, ConversationId, ConversationSummary, FileContents, Identity,
    Message, MessageBody, MessageId, NewAttachment, RecentEmoji, ServiceId,
};
use crate::storage::{Passphrase, SecretStore, Storage};
use crate::video::VideoHelper;

/// Stack for the core thread. Generating Kyber prekeys while linking
/// overflows the 2 MiB default in debug builds.
pub(crate) const STACK_SIZE: usize = 16 * 1024 * 1024;

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
    /// Stores the text and copies of its attachments as pending
    /// ([`Event::MessageAdded`]) and sends it in the background: the
    /// attachments are uploaded first, then [`Event::MessageUpdated`] once
    /// sent, or [`Event::SendFailed`]. Sends go out one at a time, in order.
    /// Sending accepts a message request; nothing is sent to a blocked
    /// chat. [`Event::AttachFailed`], sending nothing, if an attachment
    /// can't be read or is too large; at most [`MAX_ATTACHMENTS`] go.
    ///
    /// [`MAX_ATTACHMENTS`]: crate::MAX_ATTACHMENTS
    SendText {
        conversation: ConversationId,
        body: MessageBody,
        /// The message it replies to, quoted.
        reply_to: Option<MessageId>,
        attachments: Vec<NewAttachment>,
    },
    /// Replaces the text of our message, within [`EDIT_WINDOW`] of sending
    /// it and up to [`MAX_SENT_EDITS`] times: [`Event::MessageUpdated`] at
    /// once, then sent in the background, or [`Event::ChangeFailed`].
    Edit { id: MessageId, body: MessageBody },
    /// Deletes a message: ours for everyone, within [`DELETE_WINDOW`] of
    /// sending it ([`Event::MessageUpdated`]), or any just for this account
    /// and its other devices ([`Event::MessageRemoved`]).
    Delete { id: MessageId, everyone: bool },
    /// Downloads an attachment that's on Signal's servers, or failed to
    /// download: [`Event::MessageUpdated`] as it starts and as it ends.
    Download(AttachmentId),
    /// Answered with [`Event::AttachmentLoaded`].
    LoadAttachment(AttachmentId),
    /// A video's first frame; answered with [`Event::PreviewLoaded`].
    LoadPreview(AttachmentId),
    /// Writes a downloaded attachment to `path`, replacing any file there;
    /// [`Event::SaveFailed`] if it can't.
    SaveAttachment { id: AttachmentId, path: PathBuf },
    /// Writes downloaded attachments into `dir`, each under its
    /// [`save_name`](crate::text::save_name), numbered rather than
    /// replacing a file; [`Event::SaveFailed`] for any it can't.
    SaveAttachments {
        ids: Vec<AttachmentId>,
        dir: PathBuf,
    },
    /// Sends a failed message again: [`Event::MessageUpdated`] as pending,
    /// then as for [`Command::SendText`].
    RetrySend(MessageId),
    /// Reconnects now instead of waiting for the next retry.
    Reconnect,
    /// Finds or creates the 1:1 conversation with an account; answered with
    /// [`Event::ConversationUpdated`].
    OpenConversation(ServiceId),
    /// Creates a group with this account as admin, and 1 to
    /// [`MAX_GROUP_MEMBERS`] others; those whose profile key is unknown are
    /// invited. The title is cleaned and at most [`MAX_GROUP_TITLE`]
    /// graphemes. Needs the server: [`Event::GroupCreated`] or
    /// [`Event::GroupNotCreated`]. One at a time; ignored while one runs.
    ///
    /// [`MAX_GROUP_TITLE`]: crate::text::MAX_GROUP_TITLE
    CreateGroup {
        title: String,
        members: Vec<ServiceId>,
    },
    /// Answered with [`Event::Conversations`].
    LoadConversations,
    /// Answered with [`Event::Contacts`].
    LoadContacts,
    /// Answered with [`Event::RecentEmoji`].
    LoadRecentEmoji,
    /// Replaces the recent emoji; at most [`MAX_RECENT_EMOJI`] are kept.
    SetRecentEmoji(RecentEmoji),
    /// Up to `limit` messages before `before` (or the newest); answered
    /// with [`Event::Messages`].
    LoadMessages {
        conversation: ConversationId,
        before: Option<MessageId>,
        limit: u32,
    },
    /// Marks incoming messages read; [`Event::ConversationUpdated`] follows,
    /// after [`Event::MessageUpdated`] for disappearing ones, whose countdown
    /// starts. Sends read receipts if the account allows them, unless it's a
    /// message request.
    MarkRead(ConversationId),
    /// The user started or stopped typing in a conversation; tells the
    /// other side if the account allows typing indicators, unless it's a
    /// message request. Repeat `true` freely while typing.
    Typing {
        conversation: ConversationId,
        typing: bool,
    },
    /// Answered with [`Event::Identity`].
    LoadIdentity(ServiceId),
    /// Accepts the changed safety number of an account verified on the
    /// phone ([`Verification::Changed`]): it counts as unverified, and
    /// sending to it works again. [`Event::Identity`] and
    /// [`Event::UnconfirmedIdentities`] follow.
    ///
    /// [`Verification::Changed`]: crate::model::Verification::Changed
    ConfirmIdentity(ServiceId),
    /// Accepts a message request, or unblocks a chat: read receipts and
    /// typing go out there again, and a 1:1 request's sender gets this
    /// account's profile. The phone learns of it.
    /// [`Event::ConversationUpdated`] follows.
    Accept(ConversationId),
    /// Blocks a chat: messages from the account, or in the group, are
    /// dropped. The phone learns of it. [`Event::ConversationUpdated`]
    /// follows.
    Block(ConversationId),
    /// Deletes a message request or a blocked chat, keeping a block: the
    /// phone learns of it. [`Event::ConversationRemoved`] follows.
    DeleteRequest(ConversationId),
    /// Removes this device from the account, unless the phone already did,
    /// then deletes all local data (database, salt, keychain key; not the
    /// config) and starts over: [`Event::Unlinking`], then
    /// [`Event::Unlinked`] or [`Event::Locked`], after
    /// [`Event::DeviceNotRemoved`] if removing failed. The UI must confirm
    /// first.
    Unlink,
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
    /// The device name and privacy settings: once ready, and again when
    /// the phone syncs settings.
    Account(Account),
    /// A new message was stored; [`Event::ConversationUpdated`] follows.
    MessageAdded(Message),
    ConversationUpdated(ConversationSummary),
    /// All conversations, most recently active first. Answers
    /// [`Command::LoadConversations`]; also sent when names change.
    Conversations(Vec<ConversationSummary>),
    /// Contacts synced from the phone, by name. Answers
    /// [`Command::LoadContacts`]; also sent when the phone syncs them.
    Contacts(Vec<Contact>),
    /// Answers [`Command::LoadRecentEmoji`].
    RecentEmoji(RecentEmoji),
    /// [`Command::CreateGroup`] made this conversation, accepted, after
    /// [`Event::MessageAdded`] for the line saying so and
    /// [`Event::ConversationUpdated`].
    GroupCreated(ConversationId),
    /// [`Command::CreateGroup`] failed. Past a timeout, the group may exist
    /// anyway.
    GroupNotCreated(BackendError),
    /// A page of messages, oldest first. Answers [`Command::LoadMessages`].
    Messages {
        conversation: ConversationId,
        before: Option<MessageId>,
        messages: Vec<Message>,
    },
    /// Reading or writing local data failed while running.
    DatabaseError(String),
    /// A stored message changed, e.g. its status.
    MessageUpdated(Message),
    /// A video's first frame as a PNG; `None` if there's none. Answers
    /// [`Command::LoadPreview`].
    PreviewLoaded {
        id: AttachmentId,
        contents: Option<FileContents>,
    },
    /// Saving an attachment failed.
    SaveFailed(String),
    /// A file to send couldn't be read, or is too large: the message wasn't
    /// sent.
    AttachFailed(String),
    /// A downloaded attachment's contents; `None` if it isn't here.
    /// Answers [`Command::LoadAttachment`].
    AttachmentLoaded {
        id: AttachmentId,
        contents: Option<FileContents>,
    },
    /// A message was deleted for this account only, or disappeared; gone
    /// from storage. [`Event::ConversationUpdated`] follows.
    MessageRemoved {
        conversation: ConversationId,
        id: MessageId,
    },
    /// A conversation and its messages were deleted.
    ConversationRemoved(ConversationId),
    /// `member` started or stopped typing in `conversation`. Treat it as
    /// stopped after [`TYPING_TIMEOUT`] without news, or once a message
    /// from them arrives.
    Typing {
        conversation: ConversationId,
        member: ServiceId,
        typing: bool,
    },
    /// Sending an edit or delete of message `id` failed; recipients may
    /// still see it as it was.
    ChangeFailed {
        id: MessageId,
        error: BackendError,
    },
    /// Sending failed; `message` is now marked failed.
    SendFailed {
        message: Message,
        error: BackendError,
    },
    /// An account's safety number and verification. Answers
    /// [`Command::LoadIdentity`]; also sent when they change.
    Identity(Identity),
    /// Accounts verified on the phone whose identity key changed since:
    /// sending to them fails, with [`BackendError::is_untrusted_identity`],
    /// until [`Command::ConfirmIdentity`]. Sent when the list changes, and
    /// once ready unless it's empty.
    UnconfirmedIdentities(Vec<ServiceId>),
    /// The server rejected this device's credentials, as once the phone
    /// unlinks it. The core stops reconnecting: offer [`Command::Unlink`],
    /// or [`Command::Reconnect`] in case it was a glitch.
    LinkRevoked,
    /// [`Command::Unlink`] started: the account's data is going.
    Unlinking,
    /// [`Command::Unlink`] couldn't remove this device from the account;
    /// local data goes anyway. The phone lists the device until it's
    /// removed there.
    DeviceNotRemoved(BackendError),
    /// Local data was wiped, but the keychain couldn't be reached to
    /// delete the database key; the user should delete it there
    /// ([`OsKeychain::entry_label`](crate::storage::OsKeychain::entry_label)).
    KeyNotRemoved(String),
    /// The connection failed to open (`Some`) or closed (`None`); the next
    /// attempt is in `retry_in`.
    Disconnected {
        error: Option<BackendError>,
        retry_in: Duration,
    },
    /// Last event: the core has stopped and closed storage.
    Stopped,
}

/// How long after sending our message it may be edited, as in Signal.
pub const EDIT_WINDOW: Duration = Duration::from_secs(24 * 3600);
/// How many times our message may be edited, as in Signal.
pub const MAX_SENT_EDITS: u32 = 10;
/// Recent emoji kept ([`Command::SetRecentEmoji`]).
pub const MAX_RECENT_EMOJI: usize = 32;
/// How long after sending our message it may be deleted for everyone.
pub const DELETE_WINDOW: Duration = Duration::from_secs(24 * 3600);

/// Most members a new group may have besides this account; Signal's
/// limit is 1001 with it.
pub const MAX_GROUP_MEMBERS: usize = 1000;

/// How long a typing indicator lasts without being renewed.
pub const TYPING_TIMEOUT: Duration = Duration::from_secs(15);

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
    /// How long connecting, or asking the phone for contacts, may take.
    pub connect_timeout: Duration,
    /// Time between connection checks; a check unanswered within
    /// `probe_timeout` reconnects.
    pub probe_interval: Duration,
    pub probe_timeout: Duration,
    /// How long an outgoing message may wait for a connection before it
    /// fails.
    pub send_timeout: Duration,
    /// How long creating a group may take.
    pub group_timeout: Duration,
    /// How long losing the connection, or stopping, waits for what was
    /// already received to be stored; whatever is left after it is lost.
    pub drain_timeout: Duration,
    /// How long unlinking waits to remove the device from the account.
    pub unlink_timeout: Duration,
    /// Decodes videos' first frames; without it, videos show only their
    /// blurred preview.
    pub video_helper: Option<VideoHelper>,
    /// Times disappearing messages.
    pub clock: Arc<dyn Clock>,
}

impl Default for CoreOptions {
    fn default() -> Self {
        Self {
            reconnect_initial: Duration::from_secs(1),
            reconnect_max: Duration::from_secs(60),
            connect_timeout: Duration::from_secs(30),
            probe_interval: Duration::from_secs(20),
            probe_timeout: Duration::from_secs(10),
            send_timeout: Duration::from_secs(600),
            group_timeout: Duration::from_secs(120),
            drain_timeout: Duration::from_secs(10),
            unlink_timeout: Duration::from_secs(10),
            video_helper: None,
            clock: Arc::new(SystemClock),
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
        .stack_size(STACK_SIZE)
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
