//! Serving a linked account: keeps a connection open, stores messages and
//! answers queries.

use std::collections::HashMap;
use std::time::Duration;

use futures::future::LocalBoxFuture;
use futures::stream::LocalBoxStream;
use futures::{FutureExt as _, StreamExt as _};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::time::Instant;

use super::groups::{Groups, Update};
use super::outbox::{Outbox, Outcome};
use super::probe::{Check, Probe};
use super::run::{Core, ignored};
use super::{Command, DELETE_WINDOW, EDIT_WINDOW, Event};
use crate::backend::{BackendError, Incoming, SignalBackend};
use crate::clock::Clock;
use crate::model::{
    Account, AccountSettings, Change, ChatRef, Content, ConversationId, Destination, GroupId,
    GroupRef, IncomingMessage, Message, MessageBody, MessageChange, MessageId, MessageRef,
    MessageStatus, Outgoing, Quote, ReceiptKind, SentMessage, ServiceId, Timestamp,
};
use crate::storage::{Database, StorageError};

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
    probe: Probe,
    groups: Groups,
    /// When we last told each conversation we're typing.
    typing_sent: HashMap<ConversationId, Instant>,
    next_expiry: Option<Timestamp>,
    /// Asked of the phone once connected.
    contacts_wanted: bool,
    settings_wanted: bool,
    /// Results of contact sync requests running in the background.
    contacts_asked_tx: UnboundedSender<Result<(), BackendError>>,
    contacts_asked_rx: UnboundedReceiver<Result<(), BackendError>>,
    incoming: Option<LocalBoxStream<'static, Incoming>>,
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
}

impl<'a, B: SignalBackend> Session<'a, B> {
    fn new(core: &'a mut Core, db: &'a Database, backend: B) -> Self {
        let (contacts_asked_tx, contacts_asked_rx) = unbounded_channel();
        let options = &core.options;
        Self {
            device_name: backend.device_name(),
            settings: AccountSettings::default(),
            outbox: Outbox::new(options.send_timeout),
            probe: Probe::new(options.probe_interval, options.probe_timeout),
            groups: Groups::new(),
            typing_sent: HashMap::new(),
            next_expiry: None,
            contacts_wanted: false,
            settings_wanted: false,
            contacts_asked_tx,
            contacts_asked_rx,
            incoming: None,
            connecting: None,
            retry_at: Some(Instant::now()),
            delay: options.reconnect_initial,
            draining: None,
            stopping: false,
            drain_by: None,
            revoked: false,
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
        self.send_account();
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
                    Some(item) => {
                        self.receive(item).await;
                        None
                    }
                    None => self.closed().await,
                },
                update = self.groups.next() => {
                    self.group_update(update).await;
                    None
                }
                Some(asked) = self.contacts_asked_rx.recv() => {
                    self.contacts_asked(asked).await;
                    None
                }
                check = self.probe.next() => self.probe_check(check).await,
                outcome = self.outbox.finished() => self.send_outcome(outcome).await,
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
            Some(Command::Reconnect) if self.incoming.is_none() && self.connecting.is_none() => {
                self.retry_at = Some(Instant::now());
            }
            Some(Command::OpenConversation(peer)) => self.open_conversation(peer).await,
            Some(Command::LoadConversations) => self.send_conversations().await,
            Some(Command::LoadContacts) => self.send_contacts().await,
            Some(Command::LoadMessages {
                conversation,
                before,
                limit,
            }) => self.load_messages(conversation, before, limit).await,
            Some(Command::MarkRead(conversation)) => self.mark_read(conversation).await,
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
            Incoming::ContactsSynced => self.contacts_synced().await,
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
            Outcome::Sent { id, change: false } => self.send_finished(id, Ok(())).await,
            Outcome::Sent { id, change: true } => {
                tracing::debug!(message = id.get(), "change sent")
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
                change: Change::Delete,
            };
            if self.apply_own_change(&change).await {
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
                self.send_receipts(receipts);
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
        )
        .await;
        if let Some(group) = message.group {
            self.check_group(group).await;
        }
    }

    /// Stores our message sent from another device.
    async fn sent_elsewhere(&mut self, mut message: SentMessage) {
        own_quotes(&mut message.content, self.backend.own_id());
        self.store(
            &message.destination,
            None,
            message.timestamp,
            &message.content,
            message.expire_timer,
        )
        .await;
        if let Destination::Group(group) = message.destination {
            self.check_group(group).await;
        }
    }

    async fn change_received(&mut self, mut change: MessageChange) {
        if let Change::Edit { content, .. } = &mut change.change {
            own_quotes(content, self.backend.own_id());
        }
        match self.db.apply_change(&change, self.now()).await {
            Ok(Some(message)) => {
                let conversation = message.conversation;
                self.emit(Event::MessageUpdated(message));
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
        if !self.settings.typing_indicators {
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

    async fn contacts_synced(&mut self) {
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

    // Helpers.

    /// Removes this device from the account before its data goes.
    async fn remove_device(&mut self) {
        let timeout = self.core.options.unlink_timeout;
        match timed(timeout, self.backend.remove_device()).await {
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

    /// Reports deleted messages, then their conversations.
    async fn removed(&self, deleted: Vec<(ConversationId, MessageId)>) {
        let mut changed = Vec::new();
        for (conversation, id) in deleted {
            self.emit(Event::MessageRemoved { conversation, id });
            if !changed.contains(&conversation) {
                changed.push(conversation);
            }
        }
        for conversation in changed {
            self.send_conversation(conversation).await;
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
    /// if the account sends read receipts.
    fn send_receipts(&mut self, read: Vec<(ServiceId, Timestamp)>) {
        if self.settings.read_receipts {
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
                self.emit(Event::MessageUpdated(message));
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
    ) {
        let now = self.now();
        if author.is_none()
            && let Some(timer) = expire_timer
            && sent_at.as_millis() + u64::from(timer) * 1000 <= now.as_millis()
        {
            tracing::debug!("expired message not stored");
            return;
        }
        let added = self
            .db
            .add_message(destination, author, sent_at, content, expire_timer, now)
            .await;
        match added {
            Ok(Some(message)) => {
                let conversation = message.conversation;
                tracing::debug!(conversation = conversation.get(), "message stored");
                self.schedule(&message);
                self.emit(Event::MessageAdded(message));
                self.send_conversation(conversation).await;
            }
            Ok(None) => tracing::debug!("duplicate message ignored"),
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
type Connecting = Result<LocalBoxStream<'static, Incoming>, BackendError>;

/// When expiring fails, how long until trying again.
const EXPIRY_RETRY_MS: u64 = 60_000;

/// How often contacts are asked of the phone, besides the syncs it pushes.
const CONTACTS_REFRESH_MS: u64 = 24 * 3600 * 1000;

/// How soon contacts are asked again while none have come.
const CONTACTS_RETRY_MS: u64 = 3600 * 1000;

/// How long an edit or delete waits for its message.
const PENDING_CHANGE_LIFETIME_MS: u64 = 24 * 3600 * 1000;

/// How often a typing indicator is renewed while typing; recipients drop
/// it after [`super::TYPING_TIMEOUT`].
const TYPING_RESEND: Duration = Duration::from_secs(10);

/// Largest page [`Command::LoadMessages`] returns.
const MAX_PAGE: u32 = 500;

/// Resolves when the next expiry may be due; never without one.
fn due(clock: &dyn Clock, at: Option<Timestamp>) -> LocalBoxFuture<'static, ()> {
    match at {
        Some(at) => clock.sleep_until(at),
        None => std::future::pending().boxed_local(),
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

/// How a reply quotes `message`.
fn quote_of(message: &Message) -> Option<Quote> {
    let (text, attachment) = match &message.content {
        Content::Text {
            body, attachments, ..
        } => (body.as_str().to_string(), attachments.first().cloned()),
        Content::Deleted => return None,
        _ => (String::new(), None),
    };
    Some(Quote {
        author: message.sender,
        sent_at: message.sent_at,
        text,
        attachment,
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
fn timed(
    timeout: Duration,
    request: LocalBoxFuture<'static, Result<(), BackendError>>,
) -> LocalBoxFuture<'static, Result<(), BackendError>> {
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
async fn next_incoming(stream: &mut Option<LocalBoxStream<'static, Incoming>>) -> Option<Incoming> {
    match stream {
        Some(stream) => stream.next().await,
        None => std::future::pending().await,
    }
}
