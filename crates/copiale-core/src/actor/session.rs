//! Serving a linked account: keeps a connection open, stores messages and
//! answers queries.

use std::cell::Cell;
use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use futures::future::LocalBoxFuture;
use futures::stream::{Fuse, LocalBoxStream};
use futures::{FutureExt as _, StreamExt as _};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::time::Instant;

use super::downloads::{Downloads, Failure, Finished, Job};
use super::groups::{Groups, Update};
use super::outbox::{Outbox, Outcome, Sent};
use super::probe::{Check, Probe};
use super::retries::Limiter;
use super::run::{Core, ignored};
use super::{Command, DELETE_WINDOW, EDIT_WINDOW, Event, MAX_SENT_EDITS};
use crate::backend::{
    Ack, BackendError, Incoming, RetryRequest, SignalBackend, StoredChats, Undecryptable,
};
use crate::clock::Clock;
use crate::model::{
    Account, AccountSettings, Attachment, AttachmentId, AttachmentState, BlockedList, Change,
    ChatRef, ChatState, Content, ConversationId, Destination, FileContents, GroupId,
    GroupMasterKey, GroupRef, GroupUpdate, Identity, IdentityKey, IncomingMessage, Message,
    MessageBody, MessageChange, MessageId, MessageRef, MessageStatus, Outgoing, Peer, Quote,
    ReceiptKind, RecentEmoji, RequestResponse, SentMessage, ServiceId, Timestamp, Verification,
};
use crate::storage::{Database, Removed, StorageError};
use crate::text::save_name;

/// Why serving the account ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Served {
    Stop,
    /// Wipe local data and start over.
    Unlink,
}

/// Serves the account until told to stop or unlink.
pub(super) async fn serve<B: SignalBackend>(
    core: &mut Core,
    db: &Database,
    backend: B,
    linked_now: bool,
) -> Served {
    let mut session = Session::new(core, db, backend);
    session.start(linked_now).await;
    session.run().await
}

/// What serving keeps between events.
struct Session<'a, B> {
    core: &'a mut Core,
    db: &'a Database,
    backend: B,
    device_name: Option<String>,
    settings: AccountSettings,
    outbox: Outbox,
    downloads: Downloads,
    probe: Probe,
    groups: Groups,
    /// Retry requests we sent, and answered.
    retries_sent: Limiter,
    retries_answered: Limiter,
    /// No retry requests go out before this, after the server rate limited
    /// one; set by the requests running in the background.
    retries_paused_until: Rc<Cell<Option<Timestamp>>>,
    /// When we last told each conversation we're typing.
    typing_sent: HashMap<ConversationId, Instant>,
    next_expiry: Option<Timestamp>,
    /// Asked of the phone once connected.
    contacts_wanted: bool,
    settings_wanted: bool,
    blocked_wanted: bool,
    /// Results of contact sync requests running in the background.
    contacts_asked_tx: UnboundedSender<Result<(), BackendError>>,
    contacts_asked_rx: UnboundedReceiver<Result<(), BackendError>>,
    /// Results of Storage Service reads running in the background.
    stored_tx: UnboundedSender<StoredRead>,
    stored_rx: UnboundedReceiver<StoredRead>,
    /// Set while reading Storage Service; with `storage_wanted`, it's read
    /// again after.
    storage_reading: bool,
    storage_wanted: bool,
    /// The Storage Service manifest version last applied.
    storage_version: Option<u64>,
    incoming: Option<LocalBoxStream<'static, (Incoming, Ack)>>,
    connecting: Option<LocalBoxFuture<'static, Connecting>>,
    retry_at: Option<Instant>,
    /// Next reconnect delay.
    delay: Duration,
    /// Set while the stream ends after the connection was lost, with the
    /// reason; it reconnects once the stream has ended.
    draining: Option<Option<BackendError>>,
    /// Set once told to stop; serving ends with the stream.
    stopping: bool,
    /// When to stop waiting for the stream to end.
    drain_by: Option<Instant>,
    /// Set once the server rejected our credentials.
    revoked: bool,
    /// Accounts whose identity key changed, from the backend.
    identity_changes: Fuse<LocalBoxStream<'static, ServiceId>>,
    /// Verified accounts whose identity key changed since, by ID.
    unconfirmed: Vec<ServiceId>,
}

impl<'a, B: SignalBackend> Session<'a, B> {
    fn new(core: &'a mut Core, db: &'a Database, mut backend: B) -> Self {
        let (contacts_asked_tx, contacts_asked_rx) = unbounded_channel();
        let (stored_tx, stored_rx) = unbounded_channel();
        let options = &core.options;
        Self {
            device_name: backend.device_name(),
            settings: AccountSettings::default(),
            outbox: Outbox::new(options.send_timeout),
            downloads: Downloads::new(db.files()),
            probe: Probe::new(options.probe_interval, options.probe_timeout),
            groups: Groups::new(),
            retries_sent: Limiter::new(RETRY_WINDOW, MAX_RETRY_REQUESTS_PER_DEVICE),
            retries_answered: Limiter::new(RETRY_WINDOW, MAX_RETRIES_ANSWERED_PER_DEVICE),
            retries_paused_until: Rc::default(),
            typing_sent: HashMap::new(),
            next_expiry: None,
            contacts_wanted: false,
            settings_wanted: false,
            blocked_wanted: false,
            contacts_asked_tx,
            contacts_asked_rx,
            stored_tx,
            stored_rx,
            storage_reading: false,
            storage_wanted: false,
            storage_version: None,
            incoming: None,
            connecting: None,
            retry_at: Some(Instant::now()),
            delay: options.reconnect_initial,
            draining: None,
            stopping: false,
            drain_by: None,
            revoked: false,
            identity_changes: backend.identity_changes().fuse(),
            unconfirmed: Vec::new(),
            core,
            db,
            backend,
        }
    }

    /// Catches up on what happened while not running, and loads settings.
    async fn start(&mut self, linked_now: bool) {
        // Overdue while not running.
        self.next_expiry = self.expire().await;
        match self.db.fail_pending().await {
            Ok(0) => {}
            Ok(count) => tracing::info!(count, "unsent messages marked failed"),
            Err(error) => self.database_error(error),
        }
        let day_ago = self
            .now()
            .as_millis()
            .saturating_sub(PENDING_CHANGE_LIFETIME_MS);
        match self
            .db
            .expire_pending_changes(Timestamp::from_millis(day_ago))
            .await
        {
            Ok(0) => {}
            Ok(count) => tracing::info!(count, "edits and deletes for unknown messages dropped"),
            Err(error) => self.database_error(error),
        }
        self.forget_sends().await;
        match self.db.check_files().await {
            Ok(interrupted) => {
                for id in interrupted {
                    self.download(id).await;
                }
            }
            Err(error) => self.database_error(error),
        }
        // Contact names title conversations; settings stay off until the
        // phone says otherwise.
        self.contacts_wanted = linked_now || self.contacts_due().await;
        match self.db.settings().await {
            Ok(Some(settings)) if !linked_now => self.settings = settings,
            known => {
                if let Err(error) = known {
                    self.database_error(error);
                }
                self.settings_wanted = true;
            }
        }
        match self.db.storage_version().await {
            Ok(version) => self.storage_version = version,
            Err(error) => self.database_error(error),
        }
        // Before Storage Service was read, the phone's list is all there is.
        self.blocked_wanted = linked_now || self.storage_version.is_none();
        let me = ChatRef::Contact(self.backend.own_id());
        if let Err(error) = self.db.mark_accepted(me).await {
            self.database_error(error);
        }
        self.send_account();
        self.check_unconfirmed().await;
    }

    async fn run(mut self) -> Served {
        loop {
            if self.retry_at.is_some_and(|at| at <= Instant::now()) {
                self.connect();
            }
            let wait = self.retry_at.unwrap_or_else(Instant::now);
            let drain_wait = self.drain_by.unwrap_or_else(Instant::now);
            let served = tokio::select! {
                command = self.core.commands.recv(), if !self.stopping => self.command(command).await,
                result = maybe(&mut self.connecting) => {
                    self.connected(result);
                    None
                }
                item = next_incoming(&mut self.incoming) => match item {
                    Some((item, ack)) => {
                        self.receive(item).await;
                        // Stored, or dropped for good.
                        ack.ack();
                        None
                    }
                    None => self.closed().await,
                },
                update = self.groups.next() => {
                    self.group_update(update).await;
                    None
                }
                Some(peer) = self.identity_changes.next() => {
                    self.identity_changed(peer).await;
                    None
                }
                Some(asked) = self.contacts_asked_rx.recv() => {
                    self.contacts_asked(asked).await;
                    None
                }
                Some(read) = self.stored_rx.recv() => {
                    self.stored_read(read).await;
                    None
                }
                check = self.probe.next() => self.probe_check(check).await,
                outcome = self.outbox.finished() => self.send_outcome(outcome).await,
                finished = self.downloads.finished() => {
                    self.download_finished(finished).await;
                    None
                }
                () = tokio::time::sleep_until(wait), if self.retry_at.is_some() => None,
                () = tokio::time::sleep_until(drain_wait), if self.drain_by.is_some() => {
                    self.drain_timed_out().await
                }
                () = due(&*self.core.options.clock, self.next_expiry) => {
                    self.next_expiry = self.expire().await;
                    None
                }
            };
            if let Some(served) = served {
                return served;
            }
        }
    }

    fn emit(&self, event: Event) {
        self.core.emit(event);
    }

    fn now(&self) -> Timestamp {
        self.core.options.clock.now()
    }

    fn database_error(&self, error: StorageError) {
        tracing::error!(%error, "database error");
        self.emit(Event::DatabaseError(error.to_string()));
    }

    fn send_account(&self) {
        self.emit(Event::Account(Account {
            device_name: self.device_name.clone(),
            settings: self.settings,
        }));
    }

    /// Handles a command; `Some` ends serving.
    async fn command(&mut self, command: Option<Command>) -> Option<Served> {
        match command {
            Some(Command::SendText {
                conversation,
                body,
                reply_to,
            }) => self.send_text(conversation, body, reply_to).await,
            Some(Command::RetrySend(id)) => self.retry_send(id).await,
            Some(Command::Edit { id, body }) => self.edit(id, body).await,
            Some(Command::Delete { id, everyone }) => self.delete(id, everyone).await,
            Some(Command::Download(id)) => self.download(id).await,
            Some(Command::LoadAttachment(id)) => self.load_attachment(id).await,
            Some(Command::SaveAttachment { id, path }) => self.save_attachment(id, path).await,
            Some(Command::SaveAttachments { ids, dir }) => self.save_attachments(ids, dir).await,
            Some(Command::Reconnect) if self.incoming.is_none() && self.connecting.is_none() => {
                self.retry_at = Some(Instant::now());
            }
            Some(Command::OpenConversation(peer)) => self.open_conversation(peer).await,
            Some(Command::LoadConversations) => self.send_conversations().await,
            Some(Command::LoadContacts) => self.send_contacts().await,
            Some(Command::LoadRecentEmoji) => self.send_recent_emoji().await,
            Some(Command::SetRecentEmoji(recent)) => self.set_recent_emoji(recent).await,
            Some(Command::LoadMessages {
                conversation,
                before,
                limit,
            }) => self.load_messages(conversation, before, limit).await,
            Some(Command::MarkRead(conversation)) => self.mark_read(conversation).await,
            Some(Command::LoadIdentity(peer)) => self.load_identity(peer).await,
            Some(Command::ConfirmIdentity(peer)) => self.confirm_identity(peer).await,
            Some(Command::Accept(conversation)) => self.accept(conversation).await,
            Some(Command::Block(conversation)) => self.block(conversation).await,
            Some(Command::DeleteRequest(conversation)) => self.delete_request(conversation).await,
            Some(Command::Typing {
                conversation,
                typing,
            }) if self.settings.typing_indicators => self.typing(conversation, typing).await,
            Some(Command::Unlink) => return Some(self.unlink().await),
            Some(Command::Shutdown) | None => return self.shut_down().await,
            Some(other) => ignored(&other),
        }
        None
    }

    /// Handles an item from the connection.
    async fn receive(&mut self, item: Incoming) {
        // Changes noticed while decrypting `item` come first.
        while let Some(Some(peer)) = self.identity_changes.next().now_or_never() {
            self.identity_changed(peer).await;
        }
        match item {
            Incoming::QueueEmpty => self.synced(),
            Incoming::Message(message) => self.message_received(message).await,
            Incoming::Sent(message) => self.sent_elsewhere(message).await,
            Incoming::Change(change) => self.change_received(change).await,
            Incoming::DeletedForMe {
                messages,
                conversations,
            } => self.deleted_elsewhere(messages, conversations).await,
            Incoming::Receipt {
                sender,
                kind,
                timestamps,
            } => self.receipt_received(sender, kind, timestamps).await,
            Incoming::ReadElsewhere { read, at } => self.read_elsewhere(read, at).await,
            Incoming::Typing {
                sender,
                group,
                typing,
            } => self.typing_received(sender, group, typing).await,
            Incoming::Settings {
                read_receipts,
                typing_indicators,
            } => self.settings_synced(read_receipts, typing_indicators).await,
            Incoming::Verified {
                peer,
                key,
                verified,
            } => self.verified_synced(peer, key, verified).await,
            Incoming::ContactsSynced { all } => self.contacts_synced(all).await,
            Incoming::RequestResponse { chat, response } => {
                self.request_answered(chat, response).await
            }
            Incoming::Blocked(list) => self.blocked_synced(list).await,
            Incoming::StorageChanged => self.read_storage(),
            Incoming::Undecryptable(failure) => self.undecryptable(failure).await,
            Incoming::RetryRequested {
                sender,
                device,
                sent_at,
                session_reset,
            } => {
                self.retry_requested(sender, device, sent_at, session_reset)
                    .await
            }
        }
    }

    /// Starts connecting.
    fn connect(&mut self) {
        self.retry_at = None;
        let receive =
            tokio::time::timeout(self.core.options.connect_timeout, self.backend.receive());
        self.connecting = Some(
            receive
                .map(|result| {
                    result.unwrap_or_else(|_| Err(BackendError::network("timed out connecting")))
                })
                .boxed_local(),
        );
    }

    fn connected(&mut self, result: Connecting) {
        self.connecting = None;
        match result {
            Ok(stream) => {
                self.incoming = Some(stream);
                self.revoked = false;
                self.emit(Event::Connected);
                self.probe.start();
                self.outbox.connected(&mut self.backend);
                self.downloads.connected(&mut self.backend);
                let timeout = self.core.options.connect_timeout;
                if std::mem::take(&mut self.contacts_wanted) {
                    let request = timed(timeout, self.backend.request_contact_sync());
                    let asked = self.contacts_asked_tx.clone();
                    tokio::task::spawn_local(async move {
                        asked.send(request.await).ok();
                    });
                }
                if std::mem::take(&mut self.settings_wanted) {
                    let request = timed(timeout, self.backend.request_settings_sync());
                    spawn_logged("requesting settings", request);
                }
                if std::mem::take(&mut self.blocked_wanted) {
                    let request = timed(timeout, self.backend.request_blocked_sync());
                    spawn_logged("requesting the blocked list", request);
                }
                self.read_storage();
            }
            Err(error) if error.is_unauthorized() => {
                tracing::warn!(%error, "credentials rejected");
                self.revoked = true;
                self.emit(Event::LinkRevoked);
            }
            Err(error) => {
                tracing::warn!(%error, "connection failed");
                self.disconnected(Some(error));
            }
        }
    }

    /// The stream ended; `Some` ends serving.
    async fn closed(&mut self) -> Option<Served> {
        if self.draining.is_none() && !self.stopping {
            tracing::info!("connection closed");
        }
        self.ended().await
    }

    async fn drain_timed_out(&mut self) -> Option<Served> {
        tracing::warn!("stopped waiting for messages already received");
        self.ended().await
    }

    /// Stops sending and checking at once, but lets what was received come
    /// through before reconnecting; `Some` ends serving.
    async fn connection_lost(&mut self, error: BackendError) -> Option<Served> {
        if self.draining.is_some() {
            return None;
        }
        self.probe.stop();
        self.outbox.disconnected();
        self.downloads.disconnected();
        self.draining = Some(Some(error));
        if self.incoming.is_none() {
            return self.ended().await;
        }
        self.backend.stop_receiving();
        self.drain_by = Some(Instant::now() + self.core.options.drain_timeout);
        None
    }

    /// Done with the stream: stops if told to, otherwise reconnects unless
    /// revoked; `Some` ends serving.
    async fn ended(&mut self) -> Option<Served> {
        self.incoming = None;
        self.drain_by = None;
        if self.stopping {
            self.contacts_asked_meanwhile().await;
            return Some(Served::Stop);
        }
        self.probe.stop();
        self.outbox.disconnected();
        self.downloads.disconnected();
        let error = self.draining.take().flatten();
        if error.as_ref().is_some_and(BackendError::is_unauthorized) {
            self.revoked = true;
            self.emit(Event::LinkRevoked);
        } else {
            self.disconnected(error);
        }
        None
    }

    /// Reports the disconnect and schedules a retry; backs off.
    fn disconnected(&mut self, error: Option<BackendError>) {
        let retry_in = self.delay;
        self.emit(Event::Disconnected { error, retry_in });
        self.delay = (self.delay * 2).min(self.core.options.reconnect_max);
        self.retry_at = Some(Instant::now() + retry_in);
    }

    async fn probe_check(&mut self, check: Check) -> Option<Served> {
        match check {
            Check::Due => {
                self.probe.run(&mut self.backend);
                None
            }
            Check::Failed(error) => {
                tracing::info!(%error, "connection check failed");
                self.connection_lost(error).await
            }
        }
    }

    async fn send_outcome(&mut self, outcome: Outcome) -> Option<Served> {
        match outcome {
            Outcome::Sent {
                id,
                change: false,
                sent,
            } => {
                self.log_send(sent).await;
                self.send_finished(id, Ok(())).await;
            }
            Outcome::Sent {
                id,
                change: true,
                sent,
            } => {
                tracing::debug!(message = id.get(), "change sent");
                self.log_send(sent).await;
            }
            Outcome::Failed {
                id,
                change: false,
                error,
            } => self.send_finished(id, Err(error)).await,
            Outcome::Failed {
                id,
                change: true,
                error,
            } => {
                tracing::warn!(message = id.get(), %error, "sending a change failed");
                self.emit(Event::ChangeFailed { id, error });
            }
            Outcome::Interrupted(error) => {
                tracing::info!(%error, "send interrupted");
                if self.incoming.is_some() {
                    return self.connection_lost(error).await;
                }
                return None;
            }
        }
        self.outbox.start_next(&mut self.backend);
        None
    }

    async fn group_update(&mut self, update: Update) {
        match update {
            Update::Info(key, info) => match self.db.set_group(key, &info).await {
                Ok(Some(conversation)) => self.send_conversation(conversation).await,
                Ok(None) => {}
                Err(error) => self.database_error(error),
            },
            Update::Done(key) => self.groups.finished(key, &mut self.backend),
        }
    }

    // Commands.

    /// Stores the text as pending and queues it.
    async fn send_text(
        &mut self,
        conversation: ConversationId,
        body: MessageBody,
        reply_to: Option<MessageId>,
    ) {
        // Recipients stop showing us typing when the message arrives.
        self.typing_sent.remove(&conversation);
        if body.as_str().trim().is_empty() {
            tracing::debug!("empty message not sent");
            return;
        }
        let quote = match reply_to {
            Some(id) => match self.db.message(id).await {
                Ok(Some(quoted)) if quoted.conversation == conversation => quote_of(&quoted),
                Ok(_) => None,
                Err(error) => return self.database_error(error),
            },
            None => None,
        };
        let content = Content::Text {
            body,
            attachments: Vec::new(),
            quote,
        };
        let destination = match self.db.destination(conversation).await {
            Ok(Some(destination)) => destination,
            Ok(None) => {
                tracing::debug!(conversation = conversation.get(), "no such conversation");
                return;
            }
            Err(error) => return self.database_error(error),
        };
        // Replying accepts a request, as in Signal: it shares our profile.
        let chat = chat_of(destination);
        match self.db.is_blocked(chat).await {
            Ok(true) => return tracing::debug!("not sending to a blocked chat"),
            Ok(false) => {}
            Err(error) => return self.database_error(error),
        }
        if let Err(error) = self.db.mark_accepted(chat).await {
            return self.database_error(error);
        }
        // presage applies the same timer when sending.
        let expire_timer = match self.backend.expire_timer(destination).await {
            Ok(timer) => timer,
            Err(error) => {
                tracing::warn!(%error, "reading the disappearing messages timer failed");
                None
            }
        };
        // Signal identifies a message by author and sent time, so each needs
        // its own.
        let now = self.now();
        let mut sent_at = now;
        let message = loop {
            match self
                .db
                .add_pending(conversation, sent_at, &content, expire_timer, now)
                .await
            {
                Ok(Some(message)) => break message,
                Ok(None) => sent_at = Timestamp::from_millis(sent_at.as_millis() + 1),
                Err(error) => return self.database_error(error),
            }
        };
        self.emit(Event::MessageAdded(message.clone()));
        self.send_conversation(conversation).await;
        if self.awaits_confirmation(conversation).await {
            return self
                .send_finished(message.id, Err(BackendError::untrusted_identity()))
                .await;
        }
        let outgoing = text_of(&message.content);
        self.outbox.push(
            message.id,
            destination,
            outgoing,
            message.sent_at,
            &mut self.backend,
        );
    }

    async fn retry_send(&mut self, id: MessageId) {
        match self.db.retry_send(id).await {
            Ok(Some((destination, message))) => {
                self.emit(Event::MessageUpdated(message.clone()));
                if self.awaits_confirmation(message.conversation).await {
                    return self
                        .send_finished(message.id, Err(BackendError::untrusted_identity()))
                        .await;
                }
                let outgoing = text_of(&message.content);
                self.outbox.push(
                    message.id,
                    destination,
                    outgoing,
                    message.sent_at,
                    &mut self.backend,
                );
            }
            Ok(None) => tracing::debug!(message = id.get(), "nothing to retry"),
            Err(error) => self.database_error(error),
        }
    }

    /// Replaces our message's text here, then for everyone.
    async fn edit(&mut self, id: MessageId, body: MessageBody) {
        if body.as_str().trim().is_empty() {
            return;
        }
        let Some((message, destination)) = self.own_sent(id, EDIT_WINDOW).await else {
            return;
        };
        if message.edits >= MAX_SENT_EDITS {
            tracing::debug!(message = id.get(), "edit limit reached");
            return;
        }
        let Content::Text {
            attachments, quote, ..
        } = &message.content
        else {
            return;
        };
        if !attachments.is_empty() {
            tracing::debug!(
                message = id.get(),
                "messages with attachments aren't edited"
            );
            return;
        }
        let at = self.now();
        let change = MessageChange {
            destination,
            author: None,
            target: message.sent_at,
            server_at: at,
            change: Change::Edit {
                content: Content::Text {
                    body: body.clone(),
                    attachments: Vec::new(),
                    quote: quote.clone(),
                },
                at,
            },
        };
        if !self.apply_own_change(&change).await {
            return;
        }
        if self.awaits_confirmation(message.conversation).await {
            return self.change_refused(id);
        }
        let outgoing = Outgoing::Edit {
            target: message.sent_at,
            body,
            quote: quote.clone(),
        };
        self.outbox
            .push(id, destination, outgoing, at, &mut self.backend);
    }

    /// Deletes a message: ours for everyone, or any just for this account.
    async fn delete(&mut self, id: MessageId, everyone: bool) {
        if everyone {
            let Some((message, destination)) = self.own_sent(id, DELETE_WINDOW).await else {
                return;
            };
            let change = MessageChange {
                destination,
                author: None,
                target: message.sent_at,
                server_at: self.now(),
                change: Change::Delete,
            };
            if self.apply_own_change(&change).await {
                if self.awaits_confirmation(message.conversation).await {
                    return self.change_refused(id);
                }
                let outgoing = Outgoing::Delete {
                    target: message.sent_at,
                };
                let at = self.now();
                self.outbox
                    .push(id, destination, outgoing, at, &mut self.backend);
            }
            return;
        }
        let (message, destination) = match self.db.message(id).await {
            Ok(Some(message)) => match self.db.destination(message.conversation).await {
                Ok(Some(destination)) => (message, destination),
                Ok(None) => return,
                Err(error) => return self.database_error(error),
            },
            Ok(None) => return,
            Err(error) => return self.database_error(error),
        };
        let chat = match destination {
            Destination::Contact(peer) => ChatRef::Contact(peer),
            Destination::Group(group) => ChatRef::Group(group.key.id()),
        };
        let deleted = MessageRef {
            chat,
            author: message.sender,
            sent_at: message.sent_at,
        };
        self.delete_for_me(&[deleted], &[]).await;
        spawn_logged(
            "syncing a delete",
            self.backend
                .sync_deleted_for_me(destination, vec![(message.sender, message.sent_at)]),
        );
    }

    async fn open_conversation(&self, peer: ServiceId) {
        match self.db.open_conversation(&Destination::Contact(peer)).await {
            Ok(conversation) => self.send_conversation(conversation).await,
            Err(error) => self.database_error(error),
        }
    }

    async fn load_messages(
        &self,
        conversation: ConversationId,
        before: Option<MessageId>,
        limit: u32,
    ) {
        match self
            .db
            .messages(conversation, before, limit.min(MAX_PAGE))
            .await
        {
            Ok(messages) => self.emit(Event::Messages {
                conversation,
                before,
                messages,
            }),
            Err(error) => self.database_error(error),
        }
    }

    async fn mark_read(&mut self, conversation: ConversationId) {
        match self.db.mark_read(conversation, self.now()).await {
            Ok(read) if read.is_empty() => {}
            Ok(read) => {
                let receipts = read
                    .iter()
                    .filter_map(|message| Some((message.sender?, message.sent_at)))
                    .collect();
                self.countdowns_started(&read);
                let accepted = self.accepted(conversation).await;
                self.send_receipts(receipts, accepted);
                self.send_conversation(conversation).await;
            }
            Err(error) => self.database_error(error),
        }
    }

    /// Tells the conversation we're typing, at most every
    /// [`TYPING_RESEND`], or that we stopped.
    async fn typing(&mut self, conversation: ConversationId, typing: bool) {
        if typing {
            let recent = self
                .typing_sent
                .get(&conversation)
                .is_some_and(|at: &Instant| at.elapsed() < TYPING_RESEND);
            if recent {
                return;
            }
        } else if self.typing_sent.remove(&conversation).is_none() {
            return;
        }
        let destination = match self.db.destination(conversation).await {
            // Nobody else sees Note to Self.
            Ok(Some(Destination::Contact(peer))) if peer == self.backend.own_id() => return,
            Ok(Some(destination)) => destination,
            Ok(None) => return,
            Err(error) => return self.database_error(error),
        };
        if typing && !self.accepted(conversation).await {
            return;
        }
        if typing {
            self.typing_sent.insert(conversation, Instant::now());
        }
        spawn_logged(
            "sending typing",
            self.backend.send_typing(destination, typing),
        );
    }

    /// Local data goes, so what's still arriving can too.
    async fn unlink(&mut self) -> Served {
        self.emit(Event::Unlinking);
        if !self.revoked {
            self.remove_device().await;
        }
        Served::Unlink
    }

    /// Stops at once without a connection; otherwise once what was
    /// received is stored.
    async fn shut_down(&mut self) -> Option<Served> {
        if self.incoming.is_none() {
            self.contacts_asked_meanwhile().await;
            return Some(Served::Stop);
        }
        self.stopping = true;
        self.backend.stop_receiving();
        self.drain_by
            .get_or_insert(Instant::now() + self.core.options.drain_timeout);
        None
    }

    // Incoming.

    fn synced(&mut self) {
        // Healthy again: the next failure retries quickly.
        self.delay = self.core.options.reconnect_initial;
        self.emit(Event::Synced);
    }

    async fn message_received(&mut self, mut message: IncomingMessage) {
        let group_id = message.group.map(|group| group.key.id());
        if self.blocked(Some(message.sender), group_id).await {
            return;
        }
        if let Some(group) = message.group
            && !self
                .group_accepts(group, Some(message.sender), &message.content)
                .await
        {
            return;
        }
        if let Some(group) = message.group {
            self.added_by_accepted(group, message.sender, &message.content)
                .await;
        }
        let destination = message
            .group
            .map_or(Destination::Contact(message.sender), Destination::Group);
        own_quotes(&mut message.content, self.backend.own_id());
        self.store(
            &destination,
            Some(message.sender),
            message.timestamp,
            &message.content,
            message.expire_timer,
            Some(message.server_at),
        )
        .await;
        if let Some(group) = message.group {
            self.check_group(group).await;
        }
    }

    /// Stores our message sent from another device.
    async fn sent_elsewhere(&mut self, mut message: SentMessage) {
        if let Destination::Group(group) = message.destination
            && !self.group_accepts(group, None, &message.content).await
        {
            return;
        }
        // Sent with our profile, so accepted on the phone.
        if let Err(error) = self.db.mark_accepted(chat_of(message.destination)).await {
            self.database_error(error);
        }
        own_quotes(&mut message.content, self.backend.own_id());
        self.store(
            &message.destination,
            None,
            message.timestamp,
            &message.content,
            message.expire_timer,
            None,
        )
        .await;
        if let Destination::Group(group) = message.destination {
            self.check_group(group).await;
        }
    }

    async fn change_received(&mut self, mut change: MessageChange) {
        let group_id = match change.destination {
            Destination::Group(group) => Some(group.key.id()),
            Destination::Contact(_) => None,
        };
        if self.blocked(change.author, group_id).await {
            return;
        }
        if let Destination::Group(group) = change.destination
            && !self.in_group(group, change.author).await
        {
            return;
        }
        if let Change::Edit { content, .. } = &mut change.change {
            own_quotes(content, self.backend.own_id());
        }
        match self.db.apply_change(&change, self.now()).await {
            Ok(Some(message)) => {
                let conversation = message.conversation;
                let (author, sent_at) = (message.sender, message.sent_at);
                self.emit(Event::MessageUpdated(message));
                self.quote_changed(conversation, author, sent_at).await;
                self.send_conversation(conversation).await;
            }
            Ok(None) => tracing::debug!("change kept or ignored"),
            Err(error) => self.database_error(error),
        }
    }

    async fn deleted_elsewhere(&mut self, messages: Vec<MessageRef>, conversations: Vec<ChatRef>) {
        let me = self.backend.own_id();
        let messages: Vec<_> = messages
            .into_iter()
            .map(|message| MessageRef {
                author: message.author.filter(|author| *author != me),
                ..message
            })
            .collect();
        self.delete_for_me(&messages, &conversations).await;
    }

    async fn receipt_received(
        &mut self,
        sender: ServiceId,
        kind: ReceiptKind,
        timestamps: Vec<Timestamp>,
    ) {
        // Without our read receipts, theirs aren't shown either.
        if kind == ReceiptKind::Read && !self.settings.read_receipts {
            return;
        }
        match self.db.apply_receipt(sender, kind, &timestamps).await {
            Ok(messages) => {
                for message in messages {
                    self.emit(Event::MessageUpdated(message));
                }
            }
            Err(error) => self.database_error(error),
        }
    }

    async fn read_elsewhere(&mut self, read: Vec<(ServiceId, Timestamp)>, at: Timestamp) {
        // Not later than here, so the countdown isn't longer.
        let at = at.min(self.now());
        match self.db.mark_read_elsewhere(&read, at).await {
            Ok(read) => {
                self.countdowns_started(&read);
                let mut conversations = Vec::new();
                for message in &read {
                    if !conversations.contains(&message.conversation) {
                        conversations.push(message.conversation);
                    }
                }
                for conversation in conversations {
                    self.send_conversation(conversation).await;
                }
            }
            Err(error) => self.database_error(error),
        }
    }

    async fn typing_received(&self, sender: ServiceId, group: Option<GroupId>, typing: bool) {
        if !self.settings.typing_indicators || self.blocked(Some(sender), group).await {
            return;
        }
        match self.db.typing_conversation(sender, group).await {
            Ok(Some(conversation)) => self.emit(Event::Typing {
                conversation,
                member: sender,
                typing,
            }),
            Ok(None) => {}
            Err(error) => self.database_error(error),
        }
    }

    async fn settings_synced(
        &mut self,
        read_receipts: Option<bool>,
        typing_indicators: Option<bool>,
    ) {
        match self
            .db
            .update_settings(read_receipts, typing_indicators)
            .await
        {
            Ok(updated) => {
                tracing::info!(
                    read_receipts = updated.read_receipts,
                    typing_indicators = updated.typing_indicators,
                    "settings synced"
                );
                self.settings = updated;
                self.send_account();
            }
            Err(error) => self.database_error(error),
        }
    }

    /// Asks the sender to resend a message that couldn't be decrypted, and
    /// shows a placeholder until it arrives.
    async fn undecryptable(&mut self, failure: Undecryptable) {
        if self.blocked(Some(failure.sender), failure.group).await {
            return;
        }
        tracing::warn!(
            sender = ?failure.sender,
            device = failure.device,
            "a message couldn't be decrypted"
        );
        self.ask_resend(
            failure.sender,
            failure.device,
            failure.sent_at,
            failure.retry,
        );
        // Our other devices send syncs, which have no chat of their own.
        if !failure.shown || failure.sender == self.backend.own_id() {
            return;
        }
        match self
            .db
            .add_placeholder(failure.sender, failure.group, failure.sent_at, self.now())
            .await
        {
            Ok(Some(message)) => {
                let conversation = message.conversation;
                self.emit(Event::MessageAdded(message));
                self.send_conversation(conversation).await;
            }
            Ok(None) => {}
            Err(error) => self.database_error(error),
        }
    }

    /// Asks for a message to be sent again, unless asked already, too often,
    /// or the server rate limited us lately.
    fn ask_resend(
        &mut self,
        sender: ServiceId,
        device: u32,
        sent_at: Timestamp,
        retry: RetryRequest,
    ) {
        if self
            .retries_paused_until
            .get()
            .is_some_and(|until| self.now() < until)
        {
            return tracing::debug!("retry requests paused while rate limited");
        }
        if !self
            .retries_sent
            .allow(sender, device, sent_at, Instant::now())
        {
            return tracing::debug!("retry request already sent, or too many");
        }
        let send = self.backend.send_retry_request(sender, retry);
        let paused_until = self.retries_paused_until.clone();
        let clock = self.core.options.clock.clone();
        tokio::task::spawn_local(async move {
            match send.await {
                Ok(()) => {}
                Err(error) if error.is_rate_limited() => {
                    let wait = error.retry_after().unwrap_or(RATE_LIMIT_PAUSE);
                    let wait_ms = u64::try_from(wait.as_millis()).unwrap_or(u64::MAX);
                    let until = clock.now().as_millis().saturating_add(wait_ms);
                    let until = Timestamp::from_millis(until);
                    if paused_until.get().is_none_or(|paused| paused < until) {
                        tracing::info!(
                            seconds = wait.as_secs(),
                            "retry requests rate limited; paused"
                        );
                        paused_until.set(Some(until));
                    }
                }
                Err(error) => tracing::info!(%error, "sending a retry request failed"),
            }
        });
    }

    /// Resends our message that `sender`'s `device` couldn't decrypt, if
    /// still kept and that device got it; otherwise starts a new session if
    /// theirs was reset.
    async fn retry_requested(
        &mut self,
        sender: ServiceId,
        device: u32,
        sent_at: Timestamp,
        session_reset: bool,
    ) {
        if self.blocked(Some(sender), None).await
            || !self
                .retries_answered
                .allow(sender, device, sent_at, Instant::now())
        {
            tracing::info!("retry request blocked, already answered, or too many");
            return;
        }
        let since =
            Timestamp::from_millis(self.now().as_millis().saturating_sub(SEND_LOG_LIFETIME_MS));
        match self.db.resendable(sender, device, sent_at, since).await {
            Ok(Some(content)) => spawn_logged(
                "resending a message",
                self.backend.resend(sender, content, sent_at),
            ),
            Ok(None) if session_reset => spawn_logged(
                "sending a null message",
                self.backend.send_null_message(sender),
            ),
            Ok(None) => tracing::info!("nothing to resend"),
            Err(error) => self.database_error(error),
        }
    }

    /// With `all` the phone's contacts, drops those it no longer has.
    async fn contacts_synced(&mut self, all: Option<Vec<ServiceId>>) {
        if let Some(all) = all {
            match self.db.prune_contacts(&all).await {
                Ok(0) => {}
                Ok(removed) => tracing::info!(removed, "contacts removed on the phone"),
                Err(error) => self.database_error(error),
            }
        }
        match self.db.contact_count().await {
            Ok(count) => tracing::info!(count, "contacts synced"),
            Err(error) => self.database_error(error),
        }
        self.send_conversations().await;
        self.send_contacts().await;
        // Member names may come from contacts.
        match self.db.group_keys().await {
            Ok(keys) => keys
                .into_iter()
                .for_each(|key| self.groups.refresh(key, &mut self.backend)),
            Err(error) => self.database_error(error),
        }
    }

    // Message requests.

    /// Accepts a request, or unblocks; tells the phone, and a 1:1 request's
    /// sender who we are.
    async fn accept(&mut self, conversation: ConversationId) {
        let Some((chat, state)) = self.chat_state(conversation).await else {
            return;
        };
        if state == ChatState::Accepted {
            return;
        }
        if let Err(error) = self.db.accept(chat).await {
            return self.database_error(error);
        }
        tracing::info!(conversation = conversation.get(), ?state, "chat accepted");
        spawn_logged(
            "syncing an accepted request",
            self.backend
                .sync_request_response(chat, RequestResponse::Accept),
        );
        match (state, chat) {
            (ChatState::Blocked, _) => self.sync_blocked().await,
            (ChatState::Request, ChatRef::Contact(peer)) => {
                spawn_logged("sharing our profile", self.backend.send_profile_key(peer))
            }
            _ => {}
        }
        self.send_conversation(conversation).await;
    }

    async fn block(&mut self, conversation: ConversationId) {
        let Some((chat, state)) = self.chat_state(conversation).await else {
            return;
        };
        if chat == ChatRef::Contact(self.backend.own_id()) {
            return;
        }
        match self.db.set_blocked(chat, true).await {
            Ok(true) => {}
            Ok(false) => return,
            Err(error) => return self.database_error(error),
        }
        tracing::info!(conversation = conversation.get(), "chat blocked");
        self.typing_sent.remove(&conversation);
        if state == ChatState::Request {
            spawn_logged(
                "syncing a blocked request",
                self.backend
                    .sync_request_response(chat, RequestResponse::Block),
            );
        }
        self.sync_blocked().await;
        self.send_conversation(conversation).await;
    }

    /// Deletes a request or blocked chat here and on the phone.
    async fn delete_request(&mut self, conversation: ConversationId) {
        let Some((chat, state)) = self.chat_state(conversation).await else {
            return;
        };
        if state == ChatState::Accepted {
            return tracing::debug!("not a request");
        }
        self.delete_chat(chat).await;
        spawn_logged(
            "syncing a deleted request",
            self.backend
                .sync_request_response(chat, RequestResponse::Delete),
        );
    }

    /// Applies an answer to a request given on another device.
    async fn request_answered(&mut self, chat: ChatRef, response: RequestResponse) {
        tracing::info!(?response, "request answered on another device");
        let changed = match response {
            RequestResponse::Accept => self.db.accept(chat).await,
            RequestResponse::Block | RequestResponse::BlockAndDelete => {
                self.db.set_blocked(chat, true).await
            }
            RequestResponse::Delete => Ok(false),
        };
        match changed {
            Ok(true) => self.send_chats(&[chat]).await,
            Ok(false) => {}
            Err(error) => self.database_error(error),
        }
        if matches!(
            response,
            RequestResponse::Delete | RequestResponse::BlockAndDelete
        ) {
            self.delete_chat(chat).await;
        }
    }

    async fn blocked_synced(&mut self, list: BlockedList) {
        match self.db.set_blocked_list(&list).await {
            Ok(changed) => {
                tracing::info!(
                    accounts = list.accounts.len(),
                    groups = list.groups.len(),
                    "blocked list synced"
                );
                self.send_chats(&changed).await;
            }
            Err(error) => self.database_error(error),
        }
    }

    /// Reads Storage Service in the background, unless already reading.
    fn read_storage(&mut self) {
        if self.storage_reading {
            self.storage_wanted = true;
            return;
        }
        self.storage_reading = true;
        let timeout = self.core.options.connect_timeout;
        let request =
            tokio::time::timeout(timeout, self.backend.stored_chats(self.storage_version));
        let stored = self.stored_tx.clone();
        tokio::task::spawn_local(async move {
            let read = request
                .await
                .unwrap_or_else(|_| Err(BackendError::network("timed out")));
            stored.send(read).ok();
        });
    }

    /// Applies what Storage Service holds, if it changed.
    async fn stored_read(&mut self, read: StoredRead) {
        self.storage_reading = false;
        match read {
            Ok(Some(stored)) => match self
                .db
                .apply_stored(&stored.chats, &stored.identities, stored.version)
                .await
            {
                Ok((changed, verified)) => {
                    self.storage_version = Some(stored.version);
                    tracing::info!(
                        version = stored.version,
                        chats = stored.chats.len(),
                        changed = changed.len(),
                        verified = verified.len(),
                        "storage service read"
                    );
                    self.send_chats(&changed).await;
                    for peer in &verified {
                        self.load_identity(*peer).await;
                    }
                    if !verified.is_empty() {
                        self.check_unconfirmed().await;
                    }
                }
                Err(error) => self.database_error(error),
            },
            Ok(None) => tracing::debug!("storage service unchanged, or no storage key"),
            Err(error) => tracing::info!(%error, "reading storage service failed"),
        }
        if std::mem::take(&mut self.storage_wanted) {
            self.read_storage();
        }
    }

    /// Tells this account's other devices the whole blocked list.
    async fn sync_blocked(&mut self) {
        match self.db.blocked().await {
            Ok(list) => spawn_logged("syncing the blocked list", self.backend.sync_blocked(list)),
            Err(error) => self.database_error(error),
        }
    }

    /// Accepts the group if an accepted account added us.
    async fn added_by_accepted(&mut self, group: GroupRef, author: ServiceId, content: &Content) {
        let me = self.backend.own_id();
        let Content::GroupChanged(updates) = content else {
            return;
        };
        if !updates.contains(&GroupUpdate::Added(me)) {
            return;
        }
        match self.db.is_accepted(ChatRef::Contact(author)).await {
            Ok(true) => {
                if let Err(error) = self.db.mark_accepted(ChatRef::Group(group.key.id())).await {
                    self.database_error(error);
                }
            }
            Ok(false) => {}
            Err(error) => self.database_error(error),
        }
    }

    /// Whether `sender` (`None` for this account) or `group` is blocked.
    async fn blocked(&self, sender: Option<ServiceId>, group: Option<GroupId>) -> bool {
        let sender = sender.filter(|sender| *sender != self.backend.own_id());
        let chats = sender
            .map(ChatRef::Contact)
            .into_iter()
            .chain(group.map(ChatRef::Group));
        for chat in chats {
            match self.db.is_blocked(chat).await {
                Ok(true) => {
                    tracing::debug!("dropped content from a blocked account or group");
                    return true;
                }
                Ok(false) => {}
                Err(error) => self.database_error(error),
            }
        }
        false
    }

    /// Whether the user accepted `conversation`, or nobody wrote there yet.
    async fn accepted(&self, conversation: ConversationId) -> bool {
        self.chat_state(conversation)
            .await
            .is_some_and(|(_, state)| state == ChatState::Accepted)
    }

    /// Who `conversation` is with, and its state.
    async fn chat_state(&self, conversation: ConversationId) -> Option<(ChatRef, ChatState)> {
        match self.db.conversation(conversation).await {
            Ok(Some(summary)) => {
                let chat = match summary.peer {
                    Peer::Contact(peer) => ChatRef::Contact(peer),
                    Peer::Group(group) => ChatRef::Group(group),
                };
                Some((chat, summary.state))
            }
            Ok(None) => None,
            Err(error) => {
                self.database_error(error);
                None
            }
        }
    }

    /// Deletes the conversation with `chat`, if any.
    async fn delete_chat(&self, chat: ChatRef) {
        match self.db.delete_conversation(chat).await {
            Ok(Some(conversation)) => self.emit(Event::ConversationRemoved(conversation)),
            Ok(None) => {}
            Err(error) => self.database_error(error),
        }
    }

    /// Reports the conversations with `chats` whose state changed.
    async fn send_chats(&self, chats: &[ChatRef]) {
        for chat in chats {
            match self.db.chat_conversation(*chat).await {
                Ok(Some(conversation)) => self.send_conversation(conversation).await,
                Ok(None) => {}
                Err(error) => self.database_error(error),
            }
        }
    }

    // Identities.

    /// Notes `peer`'s new identity key in the chats with them.
    async fn identity_changed(&mut self, peer: ServiceId) {
        if peer == self.backend.own_id() {
            return;
        }
        match self.db.add_safety_number_changed(peer, self.now()).await {
            Ok(lines) => {
                for line in lines {
                    let conversation = line.conversation;
                    self.emit(Event::MessageAdded(line));
                    self.send_conversation(conversation).await;
                }
            }
            Err(error) => self.database_error(error),
        }
        self.identity_updated(peer).await;
    }

    async fn verified_synced(&mut self, peer: ServiceId, key: IdentityKey, verified: bool) {
        match self.db.set_verified(peer, verified.then_some(&key)).await {
            Ok(true) => {
                tracing::info!(?peer, verified, "verification synced");
                self.identity_updated(peer).await;
            }
            Ok(false) => {}
            Err(error) => self.database_error(error),
        }
    }

    /// Accepts `peer`'s changed key: they count as unverified.
    async fn confirm_identity(&mut self, peer: ServiceId) {
        let changed = self
            .identity(peer)
            .await
            .is_some_and(|identity| identity.verification == Verification::Changed);
        if !changed {
            tracing::debug!(?peer, "no changed safety number to confirm");
            return;
        }
        match self.db.set_verified(peer, None).await {
            Ok(_) => {
                tracing::info!(?peer, "changed safety number confirmed");
                self.identity_updated(peer).await;
            }
            Err(error) => self.database_error(error),
        }
    }

    async fn load_identity(&mut self, peer: ServiceId) {
        if let Some(identity) = self.identity(peer).await {
            self.emit(Event::Identity(identity));
        }
    }

    /// Reports `peer`'s identity, and which await confirmation, after a
    /// change.
    async fn identity_updated(&mut self, peer: ServiceId) {
        self.load_identity(peer).await;
        self.check_unconfirmed().await;
    }

    /// `peer`'s safety number and verification; `None` if unreadable.
    async fn identity(&mut self, peer: ServiceId) -> Option<Identity> {
        let current = match self.backend.identity(peer).await {
            Ok(current) => current,
            Err(error) => {
                tracing::warn!(%error, "reading an identity failed");
                return None;
            }
        };
        let verified = match self.db.verified_key(peer).await {
            Ok(verified) => verified,
            Err(error) => {
                self.database_error(error);
                return None;
            }
        };
        let key = current.as_ref().map(|current| &current.key);
        Some(Identity {
            peer,
            verification: verification(verified.as_ref(), key),
            safety_number: current.map(|current| current.safety_number),
        })
    }

    /// Finds the verified identities that changed; reports them if that
    /// list changed. Unreadable ones keep their state.
    async fn check_unconfirmed(&mut self) {
        let verified = match self.db.verified().await {
            Ok(verified) => verified,
            Err(error) => return self.database_error(error),
        };
        let mut unconfirmed = Vec::new();
        for (peer, key) in verified {
            let changed = match self.backend.identity(peer).await {
                Ok(current) => {
                    let current = current.as_ref().map(|current| &current.key);
                    verification(Some(&key), current) == Verification::Changed
                }
                Err(error) => {
                    tracing::warn!(%error, "reading an identity failed");
                    self.unconfirmed.contains(&peer)
                }
            };
            if changed {
                unconfirmed.push(peer);
            }
        }
        if unconfirmed != self.unconfirmed {
            self.unconfirmed.clone_from(&unconfirmed);
            self.emit(Event::UnconfirmedIdentities(unconfirmed));
        }
    }

    /// Whether `conversation` has a recipient whose changed safety number
    /// awaits confirmation, so sending there fails.
    async fn awaits_confirmation(&self, conversation: ConversationId) -> bool {
        if self.unconfirmed.is_empty() {
            return false;
        }
        let summary = match self.db.conversation(conversation).await {
            Ok(Some(summary)) => summary,
            Ok(None) => return false,
            Err(error) => {
                self.database_error(error);
                return false;
            }
        };
        let blocked = match summary.peer {
            Peer::Contact(peer) => self.unconfirmed.contains(&peer),
            Peer::Group(_) => summary
                .members
                .iter()
                .any(|member| self.unconfirmed.contains(&member.id)),
        };
        if blocked {
            tracing::info!(
                conversation = conversation.get(),
                "not sending until a changed safety number is confirmed"
            );
        }
        blocked
    }

    /// An edit or delete not sent, for an unconfirmed safety number.
    fn change_refused(&self, id: MessageId) {
        self.emit(Event::ChangeFailed {
            id,
            error: BackendError::untrusted_identity(),
        });
    }

    // Helpers.

    /// Keeps a send to resend on request, and forgets old ones.
    async fn log_send(&self, sent: Sent) {
        let logged = self
            .db
            .log_send(
                sent.sent_at,
                sent.message_sent_at,
                &sent.resendable,
                self.now(),
            )
            .await;
        match logged {
            Ok(()) => self.forget_sends().await,
            Err(error) => self.database_error(error),
        }
    }

    async fn forget_sends(&self) {
        let before = self.now().as_millis().saturating_sub(SEND_LOG_LIFETIME_MS);
        if let Err(error) = self.db.forget_sends(Timestamp::from_millis(before)).await {
            self.database_error(error);
        }
    }

    /// Removes this device from the account before its data goes.
    async fn remove_device(&mut self) {
        let backend = &mut self.backend;
        let removal = async move {
            match backend.remove_device().await {
                // The server can drop the connection on removing the
                // device, before answering; trying again then finds the
                // credentials rejected.
                Err(error) if error.is_network() => backend.remove_device().await,
                result => result,
            }
        };
        match timed(self.core.options.unlink_timeout, removal).await {
            Ok(()) => tracing::info!("device removed from the account"),
            // Already removed.
            Err(error) if error.is_unauthorized() => {}
            Err(error) => {
                tracing::warn!(%error, "removing the device failed");
                self.emit(Event::DeviceNotRemoved(error));
            }
        }
    }

    /// Removes messages and conversations deleted on another device.
    async fn delete_for_me(&self, messages: &[MessageRef], conversations: &[ChatRef]) {
        match self.db.delete_messages(messages).await {
            Ok(deleted) => self.removed(deleted).await,
            Err(error) => self.database_error(error),
        }
        for chat in conversations {
            match self.db.delete_conversation(*chat).await {
                Ok(Some(conversation)) => self.emit(Event::ConversationRemoved(conversation)),
                Ok(None) => {}
                Err(error) => self.database_error(error),
            }
        }
    }

    /// Reports deleted messages, replies to them, then their conversations.
    async fn removed(&self, deleted: Vec<Removed>) {
        let mut changed = Vec::new();
        for removed in &deleted {
            self.emit(Event::MessageRemoved {
                conversation: removed.conversation,
                id: removed.id,
            });
            if !changed.contains(&removed.conversation) {
                changed.push(removed.conversation);
            }
        }
        for removed in deleted {
            self.quote_changed(removed.conversation, removed.author, removed.sent_at)
                .await;
        }
        for conversation in changed {
            self.send_conversation(conversation).await;
        }
    }

    /// Reports replies to a message again, as it was stored, changed or
    /// deleted.
    async fn quote_changed(
        &self,
        conversation: ConversationId,
        author: Option<ServiceId>,
        sent_at: Timestamp,
    ) {
        match self.db.replies(conversation, author, sent_at).await {
            Ok(replies) => {
                for reply in replies {
                    self.emit(Event::MessageUpdated(reply));
                }
            }
            Err(error) => self.database_error(error),
        }
    }

    /// Deletes disappearing messages that are due; returns when the next
    /// one is.
    async fn expire(&self) -> Option<Timestamp> {
        let now = self.now();
        match self.db.expire(now).await {
            Ok(expired) if expired.is_empty() => {}
            Ok(expired) => {
                tracing::debug!(count = expired.len(), "disappearing messages deleted");
                self.removed(expired).await;
            }
            Err(error) => self.database_error(error),
        }
        match self.db.next_expiry().await {
            Ok(next) => next,
            Err(error) => {
                self.database_error(error);
                Some(Timestamp::from_millis(now.as_millis() + EXPIRY_RETRY_MS))
            }
        }
    }

    /// Brings the next expiry forward if `message`'s countdown ends sooner.
    fn schedule(&mut self, message: &Message) {
        if let Some(at) = message.expiry.and_then(|expiry| expiry.at) {
            self.next_expiry = Some(self.next_expiry.map_or(at, |next| next.min(at)));
        }
    }

    /// Reports messages whose countdown just started, and schedules them.
    fn countdowns_started(&mut self, messages: &[Message]) {
        for message in messages {
            if message.expiry.is_some_and(|expiry| expiry.at.is_some()) {
                self.emit(Event::MessageUpdated(message.clone()));
                self.schedule(message);
            }
        }
    }

    /// Tells our other devices we read these messages, and their authors too
    /// if `to_authors` and the account sends read receipts.
    fn send_receipts(&mut self, read: Vec<(ServiceId, Timestamp)>, to_authors: bool) {
        if to_authors && self.settings.read_receipts {
            let me = self.backend.own_id();
            let mut by_author: Vec<(ServiceId, Vec<Timestamp>)> = Vec::new();
            for (author, sent_at) in read.iter().filter(|(author, _)| *author != me) {
                match by_author.iter_mut().find(|(known, _)| known == author) {
                    Some((_, timestamps)) => timestamps.push(*sent_at),
                    None => by_author.push((*author, vec![*sent_at])),
                }
            }
            for (author, timestamps) in by_author {
                spawn_logged(
                    "sending read receipts",
                    self.backend.send_read_receipts(author, timestamps),
                );
            }
        }
        spawn_logged("syncing reads", self.backend.send_read_sync(read));
    }

    /// Refreshes the group's stored state if it's missing or older than
    /// `group`'s revision.
    async fn check_group(&mut self, group: GroupRef) {
        match self.db.group_revision(group.key).await {
            Ok(Some(stored)) if stored.is_none_or(|stored| stored < group.revision) => {
                self.groups.refresh(group.key, &mut self.backend);
            }
            Ok(_) => {}
            Err(error) => self.database_error(error),
        }
    }

    /// Whether a message by `author` (`None` for this account) belongs in
    /// `group`: see [`Self::in_group`]. A group change must also be newer
    /// than the stored group, so it isn't shown twice.
    async fn group_accepts(
        &mut self,
        group: GroupRef,
        author: Option<ServiceId>,
        content: &Content,
    ) -> bool {
        if !self.in_group(group, author).await {
            return false;
        }
        if !matches!(content, Content::GroupChanged(_)) {
            return true;
        }
        match self.db.group_revision(group.key).await {
            Ok(Some(Some(stored))) if stored >= group.revision => {
                tracing::info!(group = ?group.key.id(), "dropped an old group change");
                false
            }
            Ok(_) => true,
            Err(error) => {
                self.database_error(error);
                false
            }
        }
    }

    /// Whether `sender` (`None` for this account) and this account are
    /// members of `group` as last fetched. If not, fetches it once, if
    /// [`Groups::may_fetch`], and checks again: the stored state may be
    /// older than the message.
    async fn in_group(&mut self, group: GroupRef, sender: Option<ServiceId>) -> bool {
        let me = self.backend.own_id();
        let author = sender.unwrap_or(me);
        if self.has_members(group.key, &[author, me]).await {
            return true;
        }
        if self.groups.may_fetch(group.key, sender, Instant::now()) {
            let timeout = self.core.options.connect_timeout;
            match timed(timeout, self.backend.refresh_group(group.key)).await {
                Ok(()) if self.has_members(group.key, &[author, me]).await => return true,
                Ok(()) => {}
                Err(error) => tracing::info!(%error, "fetching a group failed"),
            }
        }
        tracing::info!(
            group = ?group.key.id(),
            "dropped a group message: we or its author aren't members"
        );
        false
    }

    /// Whether the group's stored state has all of `members`.
    async fn has_members(&mut self, key: GroupMasterKey, members: &[ServiceId]) -> bool {
        match self.backend.group_members(key).await {
            Ok(Some(group)) => members.iter().all(|member| group.members.contains(member)),
            Ok(None) => false,
            Err(error) => {
                tracing::warn!(%error, "reading a group failed");
                false
            }
        }
    }

    /// Our sent message `id`, if it may be changed for everyone within
    /// `window`, with where it went.
    async fn own_sent(&self, id: MessageId, window: Duration) -> Option<(Message, Destination)> {
        let message = match self.db.message(id).await {
            Ok(Some(message)) => message,
            Ok(None) => return None,
            Err(error) => {
                self.database_error(error);
                return None;
            }
        };
        let sent = matches!(
            message.status,
            Some(MessageStatus::Sent | MessageStatus::Delivered | MessageStatus::Read)
        );
        let age = self
            .now()
            .as_millis()
            .saturating_sub(message.sent_at.as_millis());
        if message.sender.is_some() || !sent || u128::from(age) > window.as_millis() {
            tracing::debug!(message = id.get(), "message can't be changed");
            return None;
        }
        match self.db.destination(message.conversation).await {
            Ok(Some(destination)) => Some((message, destination)),
            Ok(None) => None,
            Err(error) => {
                self.database_error(error);
                None
            }
        }
    }

    /// Applies our edit or delete here; `false` if it didn't apply.
    async fn apply_own_change(&self, change: &MessageChange) -> bool {
        match self.db.apply_change(change, self.now()).await {
            Ok(Some(message)) => {
                let conversation = message.conversation;
                let (author, sent_at) = (message.sender, message.sent_at);
                self.emit(Event::MessageUpdated(message));
                self.quote_changed(conversation, author, sent_at).await;
                self.send_conversation(conversation).await;
                true
            }
            Ok(None) => false,
            Err(error) => {
                self.database_error(error);
                false
            }
        }
    }

    async fn send_finished(&mut self, id: MessageId, result: Result<(), BackendError>) {
        let status = match result {
            Ok(()) => MessageStatus::Sent,
            Err(_) => MessageStatus::Failed,
        };
        let message = match self.db.set_status(id, status, self.now()).await {
            Ok(Some(message)) => message,
            Ok(None) => return,
            Err(error) => return self.database_error(error),
        };
        match result {
            Ok(()) => {
                tracing::debug!(message = id.get(), "message sent");
                self.schedule(&message);
                self.emit(Event::MessageUpdated(message));
            }
            Err(error) => {
                tracing::warn!(message = id.get(), %error, "send failed");
                self.emit(Event::SendFailed { message, error });
            }
        }
    }

    /// Stores a received message, unless it's ours (no `author`) and
    /// already gone on the device that sent it.
    async fn store(
        &mut self,
        destination: &Destination,
        author: Option<ServiceId>,
        sent_at: Timestamp,
        content: &Content,
        expire_timer: Option<u32>,
        server_at: Option<Timestamp>,
    ) {
        let now = self.now();
        if author.is_none()
            && let Some(timer) = expire_timer
            && sent_at.as_millis() + u64::from(timer) * 1000 <= now.as_millis()
        {
            tracing::debug!("expired message not stored");
            return;
        }
        // It replaces the placeholder shown while it couldn't be decrypted.
        if let Some(author) = author {
            match self.db.take_placeholders(author, sent_at).await {
                Ok(placeholders) => self.removed(placeholders).await,
                Err(error) => self.database_error(error),
            }
        }
        let added = self
            .db
            .add_message(
                destination,
                author,
                sent_at,
                content,
                expire_timer,
                server_at,
                now,
            )
            .await;
        match added {
            Ok(Some(message)) => {
                let conversation = message.conversation;
                tracing::debug!(conversation = conversation.get(), "message stored");
                self.schedule(&message);
                let message = self.auto_download(message).await;
                self.emit(Event::MessageAdded(message));
                // Replies may arrive first.
                self.quote_changed(conversation, author, sent_at).await;
                self.send_conversation(conversation).await;
            }
            Ok(None) => tracing::debug!("duplicate message ignored"),
            Err(error) => self.database_error(error),
        }
    }

    /// Starts downloading what `message` downloads without asking, unless
    /// it's a message request; returns it as it now is.
    async fn auto_download(&mut self, message: Message) -> Message {
        let Content::Text { attachments, .. } = &message.content else {
            return message;
        };
        let remote = |attachment: &&Attachment| attachment.state == AttachmentState::Remote;
        if !attachments.iter().any(|attachment| remote(&attachment))
            || !self.accepted(message.conversation).await
        {
            return message;
        }
        let now = Instant::now();
        let wanted: Vec<AttachmentId> = attachments
            .iter()
            .filter(|attachment| self.downloads.auto(attachment, message.sender, now))
            .filter_map(|attachment| attachment.id)
            .collect();
        for id in wanted {
            self.start_download(id).await;
        }
        match self.db.message(message.id).await {
            Ok(Some(now)) => now,
            Ok(None) => message,
            Err(error) => {
                self.database_error(error);
                message
            }
        }
    }

    /// Downloads an attachment asked for, reporting its message.
    async fn download(&mut self, id: AttachmentId) {
        if let Some(message) = self.start_download(id).await {
            self.attachment_changed(message).await;
        }
    }

    /// Queues a download unless the attachment is here, going or gone;
    /// returns its message if queued.
    async fn start_download(&mut self, id: AttachmentId) -> Option<MessageId> {
        match self.db.start_download(id).await {
            Ok(Some((message, remote))) => {
                tracing::debug!(attachment = id.get(), "downloading an attachment");
                self.downloads.push(Job { id, remote }, &mut self.backend);
                Some(message)
            }
            Ok(None) => None,
            Err(error) => {
                self.database_error(error);
                None
            }
        }
    }

    async fn download_finished(&mut self, finished: Finished) {
        let Finished { job, result } = finished;
        let id = job.id;
        let stored = match result {
            Ok(file) => self.db.finish_download(id, file).await,
            Err(Failure::Backend(error)) if error.is_network() => {
                tracing::debug!(attachment = id.get(), %error, "download cut off");
                self.downloads.wait(job);
                return;
            }
            Err(Failure::Backend(error)) => {
                let state = if error.is_gone() {
                    AttachmentState::Expired
                } else {
                    AttachmentState::Failed
                };
                tracing::warn!(attachment = id.get(), %error, "download failed");
                self.db.stop_download(id, state).await
            }
            Err(Failure::Storage(error)) => {
                self.database_error(error.into());
                self.db.stop_download(id, AttachmentState::Failed).await
            }
        };
        match stored {
            Ok(Some(message)) => self.attachment_changed(message).await,
            Ok(None) => tracing::debug!(attachment = id.get(), "downloaded after it was gone"),
            Err(error) => self.database_error(error),
        }
        self.downloads.start(&mut self.backend);
    }

    async fn load_attachment(&self, id: AttachmentId) {
        let contents = match self.db.attachment_contents(id).await {
            Ok(contents) => contents.map(FileContents::new),
            Err(error) => {
                self.database_error(error);
                None
            }
        };
        self.emit(Event::AttachmentLoaded { id, contents });
    }

    async fn save_attachment(&self, id: AttachmentId, path: PathBuf) {
        let Some(contents) = self.saved_contents(id).await else {
            return;
        };
        let written = tokio::task::spawn_blocking(move || std::fs::write(path, contents)).await;
        self.saved(
            written
                .map_err(std::io::Error::other)
                .and_then(|written| written),
        );
    }

    async fn save_attachments(&self, ids: Vec<AttachmentId>, dir: PathBuf) {
        for id in ids {
            let attachment = match self.db.attachment(id).await {
                Ok(Some(attachment)) => attachment,
                Ok(None) => continue,
                Err(error) => return self.database_error(error),
            };
            let Some(contents) = self.saved_contents(id).await else {
                continue;
            };
            let name = save_name(
                attachment.name.as_deref(),
                attachment.content_type.as_deref(),
            );
            let dir = dir.clone();
            let written =
                tokio::task::spawn_blocking(move || write_new(&dir, &name, &contents)).await;
            self.saved(
                written
                    .map_err(std::io::Error::other)
                    .and_then(|written| written),
            );
        }
    }

    /// A downloaded attachment's contents to save; reports why if there
    /// are none.
    async fn saved_contents(&self, id: AttachmentId) -> Option<Vec<u8>> {
        match self.db.attachment_contents(id).await {
            Ok(Some(contents)) => Some(contents),
            Ok(None) => {
                self.emit(Event::SaveFailed("the attachment isn't downloaded".into()));
                None
            }
            Err(error) => {
                self.database_error(error);
                None
            }
        }
    }

    fn saved(&self, written: std::io::Result<()>) {
        match written {
            Ok(()) => tracing::debug!("attachment saved"),
            Err(error) => {
                tracing::warn!(%error, "saving an attachment failed");
                self.emit(Event::SaveFailed(error.to_string()));
            }
        }
    }

    async fn attachment_changed(&self, message: MessageId) {
        match self.db.message(message).await {
            Ok(Some(message)) => self.emit(Event::MessageUpdated(message)),
            Ok(None) => {}
            Err(error) => self.database_error(error),
        }
    }

    async fn send_conversation(&self, conversation: ConversationId) {
        match self.db.conversation(conversation).await {
            Ok(Some(summary)) => self.emit(Event::ConversationUpdated(summary)),
            Ok(None) => {}
            Err(error) => self.database_error(error),
        }
    }

    async fn send_conversations(&self) {
        match self.db.conversations().await {
            Ok(conversations) => self.emit(Event::Conversations(conversations)),
            Err(error) => self.database_error(error),
        }
    }

    async fn send_contacts(&self) {
        match self.db.contacts().await {
            Ok(contacts) => self.emit(Event::Contacts(contacts)),
            Err(error) => self.database_error(error),
        }
    }

    async fn send_recent_emoji(&self) {
        match self.db.recent_emoji().await {
            Ok(recent) => self.emit(Event::RecentEmoji(recent)),
            Err(error) => self.database_error(error),
        }
    }

    async fn set_recent_emoji(&self, recent: RecentEmoji) {
        if let Err(error) = self.db.set_recent_emoji(&recent).await {
            self.database_error(error);
        }
    }

    /// Notes when contacts were last asked of the phone.
    async fn contacts_asked(&self, result: Result<(), BackendError>) {
        match result {
            Ok(()) => {
                if let Err(error) = self.db.set_contacts_requested_at(self.now()).await {
                    self.database_error(error);
                }
            }
            Err(error) => tracing::warn!(%error, "requesting contacts failed"),
        }
    }

    /// Notes requests that finished but weren't handled yet, before
    /// stopping, so the next start doesn't ask again.
    async fn contacts_asked_meanwhile(&mut self) {
        while let Ok(result) = self.contacts_asked_rx.try_recv() {
            self.contacts_asked(result).await;
        }
    }

    /// Whether to ask the phone for contacts: never asked, or not for a
    /// day, or for an hour while none have come; the phone may ignore
    /// requests in quick succession.
    async fn contacts_due(&self) -> bool {
        let Ok(Some(at)) = self.db.contacts_requested_at().await else {
            return true;
        };
        let wait = match self.db.has_contacts().await {
            Ok(true) => CONTACTS_REFRESH_MS,
            _ => CONTACTS_RETRY_MS,
        };
        self.now().as_millis().saturating_sub(at.as_millis()) >= wait
    }
}

/// A connection attempt's result.
type Connecting = Result<LocalBoxStream<'static, (Incoming, Ack)>, BackendError>;

/// A Storage Service read's result.
type StoredRead = Result<Option<StoredChats>, BackendError>;

/// When expiring fails, how long until trying again.
const EXPIRY_RETRY_MS: u64 = 60_000;

/// How often contacts are asked of the phone, besides the syncs it pushes.
const CONTACTS_REFRESH_MS: u64 = 24 * 3600 * 1000;

/// How soon contacts are asked again while none have come.
const CONTACTS_RETRY_MS: u64 = 3600 * 1000;

/// How long an edit or delete waits for its message.
const PENDING_CHANGE_LIFETIME_MS: u64 = 24 * 3600 * 1000;

/// How long our sends are kept to resend on request, as in Signal.
const SEND_LOG_LIFETIME_MS: u64 = 24 * 3600 * 1000;

/// Most retry requests sent to one device per [`RETRY_WINDOW`]: the first
/// already makes it reset its session.
const MAX_RETRY_REQUESTS_PER_DEVICE: usize = 5;
/// Most retry requests answered for one device per [`RETRY_WINDOW`].
const MAX_RETRIES_ANSWERED_PER_DEVICE: usize = 100;
/// Each message is asked for, or resent, only once per window.
const RETRY_WINDOW: Duration = Duration::from_secs(3600);
/// How long retry requests pause after a rate limit that didn't say.
const RATE_LIMIT_PAUSE: Duration = Duration::from_secs(600);

/// How often a typing indicator is renewed while typing; recipients drop
/// it after [`super::TYPING_TIMEOUT`].
const TYPING_RESEND: Duration = Duration::from_secs(10);

/// Largest page [`Command::LoadMessages`] returns.
const MAX_PAGE: u32 = 500;

/// Writes `contents` into `dir` as `name`, or `name (2)` and so on if
/// taken.
fn write_new(dir: &Path, name: &str, contents: &[u8]) -> std::io::Result<()> {
    let (stem, extension) = match name.rfind('.') {
        Some(dot) if dot > 0 => name.split_at(dot),
        _ => (name, ""),
    };
    for n in 1..=MAX_NUMBERED {
        let path = match n {
            1 => dir.join(name),
            n => dir.join(format!("{stem} ({n}){extension}")),
        };
        match std::fs::File::create_new(&path) {
            Ok(mut file) => return file.write_all(contents),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::other("too many files with that name"))
}

/// Most numbered names [`write_new`] tries.
const MAX_NUMBERED: u32 = 1000;

/// Resolves when the next expiry may be due; never without one.
fn due(clock: &dyn Clock, at: Option<Timestamp>) -> LocalBoxFuture<'static, ()> {
    match at {
        Some(at) => clock.sleep_until(at),
        None => std::future::pending().boxed_local(),
    }
}

/// An identity's verification: `verified` is the key verified on the
/// phone, `current` the one sessions use.
fn verification(verified: Option<&IdentityKey>, current: Option<&IdentityKey>) -> Verification {
    match (verified, current) {
        (None, _) => Verification::Unverified,
        (Some(verified), Some(current)) if verified != current => Verification::Changed,
        (Some(_), _) => Verification::Verified,
    }
}

/// The chat messages to `destination` go to.
fn chat_of(destination: Destination) -> ChatRef {
    match destination {
        Destination::Contact(peer) => ChatRef::Contact(peer),
        Destination::Group(group) => ChatRef::Group(group.key.id()),
    }
}

/// A text message as sent: its body and quote.
fn text_of(content: &Content) -> Outgoing {
    match content {
        Content::Text { body, quote, .. } => Outgoing::Text {
            body: body.clone(),
            quote: quote.clone(),
        },
        _ => Outgoing::Text {
            body: MessageBody::new(""),
            quote: None,
        },
    }
}

/// How a reply quotes `message`; storage fills in what it says.
fn quote_of(message: &Message) -> Option<Quote> {
    (message.content != Content::Deleted).then_some(Quote {
        author: message.sender,
        sent_at: message.sent_at,
        original: None,
    })
}

/// Quotes of this account's messages name no author.
fn own_quotes(content: &mut Content, me: ServiceId) {
    if let Content::Text {
        quote: Some(quote), ..
    } = content
        && quote.author == Some(me)
    {
        quote.author = None;
    }
}

/// `request`, failing as a network error after `timeout`.
fn timed<'a>(
    timeout: Duration,
    request: impl Future<Output = Result<(), BackendError>> + 'a,
) -> LocalBoxFuture<'a, Result<(), BackendError>> {
    tokio::time::timeout(timeout, request)
        .map(|result| result.unwrap_or_else(|_| Err(BackendError::network("timed out"))))
        .boxed_local()
}

/// Runs a background send, logging failure. Must run inside a `LocalSet`.
fn spawn_logged(what: &'static str, send: LocalBoxFuture<'static, Result<(), BackendError>>) {
    tokio::task::spawn_local(async move {
        if let Err(error) = send.await {
            tracing::info!(%error, "{what} failed");
        }
    });
}

/// The future's output; never resolves while there is none.
async fn maybe<T>(future: &mut Option<LocalBoxFuture<'static, T>>) -> T {
    match future {
        Some(future) => future.await,
        None => std::future::pending().await,
    }
}

/// Next item from the stream; never resolves without one.
async fn next_incoming(
    stream: &mut Option<LocalBoxStream<'static, (Incoming, Ack)>>,
) -> Option<(Incoming, Ack)> {
    match stream {
        Some(stream) => stream.next().await,
        None => std::future::pending().await,
    }
}
