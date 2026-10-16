//! The core's phases: unlock storage, set up the account, serve.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures::future::LocalBoxFuture;
use futures::stream::LocalBoxStream;
use futures::{FutureExt as _, StreamExt as _};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;
use tokio::time::Instant;

use super::groups::{Groups, Update};
use super::outbox::{Outbox, Outcome};
use super::probe::{Check, Probe};
use super::{Command, CoreOptions, DELETE_WINDOW, EDIT_WINDOW, Event, StorageFailure};
use crate::backend::{BackendError, Incoming, Linker, SignalBackend};
use crate::clock::Clock;
use crate::model::{
    AccountSettings, Change, ChatRef, Content, ConversationId, Destination, GroupRef, Message,
    MessageBody, MessageChange, MessageId, MessageRef, MessageStatus, Outgoing, Quote, ReceiptKind,
    ServiceId, Timestamp,
};
use crate::storage::{Database, KeyLookup, SecretStore, Storage, StorageError};

pub(super) struct Core {
    pub(super) storage: Storage,
    pub(super) secrets: Arc<dyn SecretStore + Send + Sync>,
    pub(super) options: CoreOptions,
    pub(super) commands: UnboundedReceiver<Command>,
    pub(super) events: UnboundedSender<Event>,
}

/// How a phase ended.
enum Step<T> {
    Done(T),
    /// Go back to the start of the phase.
    Restart,
    /// Shut down.
    Stop,
}

impl Core {
    pub(super) async fn run<L: Linker>(mut self, mut linker: L) {
        tracing::info!("core started");
        if let Some(db) = self.unlock().await {
            if let Some((backend, linked_now)) = self.set_up_account(&db, &mut linker).await {
                self.emit(Event::Ready);
                self.serve(&db, backend, linked_now).await;
            }
            // Before `Stopped`: the process may exit right after it.
            db.close().await;
        }
        tracing::info!("core stopped");
        self.emit(Event::Stopped);
    }

    fn emit(&self, event: Event) {
        // A closed receiver means the UI is gone; keep going until told to stop.
        self.events.send(event).ok();
    }

    /// Next command; `None` means stop.
    async fn next(&mut self) -> Option<Command> {
        match self.commands.recv().await {
            Some(Command::Shutdown) | None => None,
            Some(command) => Some(command),
        }
    }

    async fn unlock(&mut self) -> Option<Database> {
        loop {
            match self.try_unlock().await {
                Step::Done(db) => return Some(db),
                Step::Restart => continue,
                Step::Stop => return None,
            }
        }
    }

    async fn try_unlock(&mut self) -> Step<Database> {
        let (storage, secrets) = (self.storage.clone(), self.secrets.clone());
        let lookup = blocking(move || storage.find_key(&*secrets)).await;
        let opened = match lookup {
            Ok(KeyLookup::Found(key)) => self.storage.open(&key).await,
            Ok(KeyLookup::NeedsPassphrase) => return self.passphrase(false).await,
            Ok(KeyLookup::NeedsNewPassphrase) => return self.passphrase(true).await,
            Err(error) => Err(error),
        };
        match opened {
            Ok(db) => Step::Done(db),
            Err(error) => self.storage_failed(error).await,
        }
    }

    /// Asks for the existing passphrase, or a new one, until storage opens.
    async fn passphrase(&mut self, new: bool) -> Step<Database> {
        self.emit(Event::Locked {
            new_passphrase: new,
        });
        loop {
            let Some(command) = self.next().await else {
                return Step::Stop;
            };
            let passphrase = match command {
                Command::Unlock(passphrase) if !new => passphrase,
                Command::SetPassphrase(passphrase) if new => passphrase,
                other => {
                    ignored(&other);
                    continue;
                }
            };
            let storage = self.storage.clone();
            let key = blocking(move || {
                if new {
                    storage.new_passphrase_key(&passphrase)
                } else {
                    storage.key_from_passphrase(&passphrase)
                }
            })
            .await;
            let opened = match key {
                Ok(key) => self.storage.open(&key).await,
                Err(error) => Err(error),
            };
            match opened {
                Ok(db) => return Step::Done(db),
                Err(StorageError::WrongKey) if !new => self.emit(Event::WrongPassphrase),
                Err(error) => return self.storage_failed(error).await,
            }
        }
    }

    /// Reports the failure and waits for [`Command::Retry`] or
    /// [`Command::StartOver`].
    async fn storage_failed<T>(&mut self, error: StorageError) -> Step<T> {
        tracing::warn!(%error, "storage failed");
        self.emit(Event::StorageFailed(failure(error)));
        loop {
            match self.next().await {
                None => return Step::Stop,
                Some(Command::Retry) => return Step::Restart,
                Some(Command::StartOver) => {
                    let (storage, secrets) = (self.storage.clone(), self.secrets.clone());
                    match blocking(move || storage.wipe(&*secrets)).await {
                        Ok(()) => return Step::Restart,
                        Err(error) => self.emit(Event::StorageFailed(failure(error))),
                    }
                }
                Some(other) => ignored(&other),
            }
        }
    }

    async fn set_up_account<L: Linker>(
        &mut self,
        db: &Database,
        linker: &mut L,
    ) -> Option<(L::Backend, bool)> {
        match linker.load(db).await {
            Ok(Some(backend)) => return Some((backend, false)),
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(%error, "loading the account failed");
                self.emit(Event::LinkFailed(error));
            }
        }
        let device_name = default_device_name();
        loop {
            self.emit(Event::Unlinked {
                device_name: device_name.clone(),
            });
            let name = loop {
                match self.next().await? {
                    Command::Link { device_name } => break device_name,
                    other => ignored(&other),
                }
            };
            match self.link(db, linker, &name).await {
                Step::Done(Ok(backend)) => return Some((backend, true)),
                Step::Done(Err(error)) => {
                    tracing::warn!(%error, "linking failed");
                    self.emit(Event::LinkFailed(error));
                }
                Step::Restart => {}
                Step::Stop => return None,
            }
        }
    }

    /// Runs one linking attempt; `Restart` means cancelled.
    async fn link<L: Linker>(
        &mut self,
        db: &Database,
        linker: &mut L,
        device_name: &str,
    ) -> Step<Result<L::Backend, BackendError>> {
        let (url_tx, mut url_rx) = oneshot::channel();
        let link = linker.link(db, device_name, url_tx);
        tokio::pin!(link);
        let mut url_pending = true;
        loop {
            tokio::select! {
                result = &mut link => return Step::Done(result),
                url = &mut url_rx, if url_pending => {
                    url_pending = false;
                    if let Ok(url) = url {
                        self.emit(Event::LinkUrl(url));
                    }
                }
                command = self.commands.recv() => match command {
                    Some(Command::CancelLink) => return Step::Restart,
                    Some(Command::Shutdown) | None => return Step::Stop,
                    Some(other) => ignored(&other),
                },
            }
        }
    }

    /// Keeps a connection open, stores messages and answers queries.
    async fn serve<B: SignalBackend>(&mut self, db: &Database, mut backend: B, linked_now: bool) {
        // Overdue while not running.
        let mut next_expiry = self.expire(db).await;
        match db.fail_pending().await {
            Ok(0) => {}
            Ok(count) => tracing::info!(count, "unsent messages marked failed"),
            Err(error) => self.database_error(error),
        }
        let day_ago = Timestamp::now()
            .as_millis()
            .saturating_sub(PENDING_CHANGE_LIFETIME_MS);
        match db
            .expire_pending_changes(Timestamp::from_millis(day_ago))
            .await
        {
            Ok(0) => {}
            Ok(count) => tracing::info!(count, "edits and deletes for unknown messages dropped"),
            Err(error) => self.database_error(error),
        }
        // Contact names title conversations; ask the phone if there are none,
        // or daily for changes it didn't push.
        if linked_now || self.contacts_due(db).await {
            let request = backend.request_contact_sync();
            match tokio::time::timeout(self.options.connect_timeout, request).await {
                Ok(Ok(())) => {
                    if let Err(error) = db.set_contacts_requested_at(Timestamp::now()).await {
                        self.database_error(error);
                    }
                }
                Ok(Err(error)) => tracing::warn!(%error, "requesting contacts failed"),
                Err(_) => tracing::warn!("requesting contacts timed out"),
            }
        }

        // Privacy settings: off until the phone says otherwise.
        let mut settings = match db.settings().await {
            Ok(Some(settings)) if !linked_now => settings,
            known => {
                if let Err(error) = known {
                    self.database_error(error);
                }
                let request = backend.request_settings_sync();
                match tokio::time::timeout(self.options.connect_timeout, request).await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => tracing::warn!(%error, "requesting settings failed"),
                    Err(_) => tracing::warn!("requesting settings timed out"),
                }
                AccountSettings::default()
            }
        };
        let mut typing_sent = HashMap::new();

        let mut incoming: Option<LocalBoxStream<'static, Incoming>> = None;
        let mut connecting: Option<LocalBoxFuture<'static, Connecting>> = None;
        let mut retry_at = Some(Instant::now());
        let mut delay = self.options.reconnect_initial;
        let mut outbox = Outbox::new(self.options.send_timeout);
        let mut probe = Probe::new(self.options.probe_interval, self.options.probe_timeout);
        let mut groups = Groups::new();
        loop {
            if retry_at.is_some_and(|at| at <= Instant::now()) {
                retry_at = None;
                let receive = tokio::time::timeout(self.options.connect_timeout, backend.receive());
                connecting = Some(
                    receive
                        .map(|result| {
                            result.unwrap_or_else(|_| {
                                Err(BackendError::network("timed out connecting"))
                            })
                        })
                        .boxed_local(),
                );
            }
            let wait = retry_at.unwrap_or_else(Instant::now);
            // Set when the connection turns out to be gone, with the reason.
            let mut lost = None;

            tokio::select! {
                command = self.commands.recv() => match command {
                    Some(Command::SendText { conversation, body, reply_to }) => {
                        self.send_text(db, &mut outbox, &mut backend, conversation, body, reply_to).await;
                        // Recipients stop showing us typing when the message arrives.
                        typing_sent.remove(&conversation);
                    }
                    Some(Command::RetrySend(id)) => match db.retry_send(id).await {
                        Ok(Some((destination, message))) => {
                            self.emit(Event::MessageUpdated(message.clone()));
                            let outgoing = text_of(&message.content);
                            outbox.push(message.id, destination, outgoing, message.sent_at, &mut backend);
                        }
                        Ok(None) => tracing::debug!(message = id.get(), "nothing to retry"),
                        Err(error) => self.database_error(error),
                    },
                    Some(Command::Edit { id, body }) => {
                        self.edit(db, &mut outbox, &mut backend, id, body).await;
                    }
                    Some(Command::Delete { id, everyone }) => {
                        self.delete(db, &mut outbox, &mut backend, id, everyone).await;
                    }
                    Some(Command::Reconnect) if incoming.is_none() && connecting.is_none() => {
                        retry_at = Some(Instant::now());
                    }
                    Some(Command::OpenConversation(peer)) => {
                        match db.open_conversation(&Destination::Contact(peer)).await {
                            Ok(conversation) => self.send_conversation(db, conversation).await,
                            Err(error) => self.database_error(error),
                        }
                    }
                    Some(Command::LoadConversations) => self.send_conversations(db).await,
                    Some(Command::LoadContacts) => self.send_contacts(db).await,
                    Some(Command::LoadMessages { conversation, before, limit }) => {
                        match db.messages(conversation, before, limit.min(MAX_PAGE)).await {
                            Ok(messages) => self.emit(Event::Messages { conversation, before, messages }),
                            Err(error) => self.database_error(error),
                        }
                    }
                    Some(Command::MarkRead(conversation)) => match db.mark_read(conversation, self.options.clock.now()).await {
                        Ok(read) if read.is_empty() => {}
                        Ok(read) => {
                            let receipts = read
                                .iter()
                                .filter_map(|message| Some((message.sender?, message.sent_at)))
                                .collect();
                            self.countdowns_started(&read, &mut next_expiry);
                            send_receipts(&mut backend, settings, receipts);
                            self.send_conversation(db, conversation).await;
                        }
                        Err(error) => self.database_error(error),
                    },
                    Some(Command::Typing { conversation, typing }) if settings.typing_indicators => {
                        self.typing(db, &mut backend, &mut typing_sent, conversation, typing).await;
                    }
                    Some(Command::Shutdown) | None => break,
                    Some(other) => ignored(&other),
                },
                result = maybe(&mut connecting) => {
                    connecting = None;
                    match result {
                        Ok(stream) => {
                            incoming = Some(stream);
                            self.emit(Event::Connected);
                            probe.start();
                            outbox.connected(&mut backend);
                        }
                        Err(error) => {
                            tracing::warn!(%error, "connection failed");
                            retry_at = Some(self.disconnected(Some(error), &mut delay));
                        }
                    }
                }
                item = next_incoming(&mut incoming) => match item {
                    Some(Incoming::QueueEmpty) => {
                        // Healthy again: the next failure retries quickly.
                        delay = self.options.reconnect_initial;
                        self.emit(Event::Synced);
                    }
                    Some(Incoming::Message(mut message)) => {
                        let destination = message
                            .group
                            .map_or(Destination::Contact(message.sender), Destination::Group);
                        own_quotes(&mut message.content, backend.own_id());
                        let stored = Stored {
                            author: Some(message.sender),
                            sent_at: message.timestamp,
                            content: &message.content,
                            expire_timer: message.expire_timer,
                        };
                        self.store(db, &destination, stored, &mut next_expiry).await;
                        if let Some(group) = message.group {
                            self.check_group(db, &mut groups, &mut backend, group).await;
                        }
                    }
                    Some(Incoming::Sent(mut message)) => {
                        own_quotes(&mut message.content, backend.own_id());
                        let stored = Stored {
                            author: None,
                            sent_at: message.timestamp,
                            content: &message.content,
                            expire_timer: message.expire_timer,
                        };
                        self.store(db, &message.destination, stored, &mut next_expiry).await;
                        if let Destination::Group(group) = message.destination {
                            self.check_group(db, &mut groups, &mut backend, group).await;
                        }
                    }
                    Some(Incoming::Change(mut change)) => {
                        if let Change::Edit { content, .. } = &mut change.change {
                            own_quotes(content, backend.own_id());
                        }
                        match db.apply_change(&change).await {
                            Ok(Some(message)) => {
                                let conversation = message.conversation;
                                self.emit(Event::MessageUpdated(message));
                                self.send_conversation(db, conversation).await;
                            }
                            Ok(None) => tracing::debug!("change kept or ignored"),
                            Err(error) => self.database_error(error),
                        }
                    }
                    Some(Incoming::DeletedForMe { messages, conversations }) => {
                        let me = backend.own_id();
                        let messages: Vec<_> = messages
                            .into_iter()
                            .map(|message| MessageRef {
                                author: message.author.filter(|author| *author != me),
                                ..message
                            })
                            .collect();
                        self.delete_for_me(db, &messages, &conversations).await;
                    }
                    Some(Incoming::Receipt { sender, kind, timestamps }) => {
                        // Without our read receipts, theirs aren't shown either.
                        if kind == ReceiptKind::Read && !settings.read_receipts {
                            continue;
                        }
                        match db.apply_receipt(sender, kind, &timestamps).await {
                            Ok(messages) => {
                                for message in messages {
                                    self.emit(Event::MessageUpdated(message));
                                }
                            }
                            Err(error) => self.database_error(error),
                        }
                    }
                    Some(Incoming::ReadElsewhere { read, at }) => {
                        // Not later than here, so the countdown isn't longer.
                        let at = at.min(self.options.clock.now());
                        match db.mark_read_elsewhere(&read, at).await {
                            Ok(read) => {
                                self.countdowns_started(&read, &mut next_expiry);
                                let mut conversations = Vec::new();
                                for message in &read {
                                    if !conversations.contains(&message.conversation) {
                                        conversations.push(message.conversation);
                                    }
                                }
                                for conversation in conversations {
                                    self.send_conversation(db, conversation).await;
                                }
                            }
                            Err(error) => self.database_error(error),
                        }
                    }
                    Some(Incoming::Typing { sender, group, typing }) if settings.typing_indicators => {
                        match db.typing_conversation(sender, group).await {
                            Ok(Some(conversation)) => self.emit(Event::Typing {
                                conversation,
                                member: sender,
                                typing,
                            }),
                            Ok(None) => {}
                            Err(error) => self.database_error(error),
                        }
                    }
                    Some(Incoming::Typing { .. }) => {}
                    Some(Incoming::Settings { read_receipts, typing_indicators }) => {
                        match db.update_settings(read_receipts, typing_indicators).await {
                            Ok(updated) => {
                                tracing::info!(
                                    read_receipts = updated.read_receipts,
                                    typing_indicators = updated.typing_indicators,
                                    "settings synced"
                                );
                                settings = updated;
                            }
                            Err(error) => self.database_error(error),
                        }
                    }
                    Some(Incoming::ContactsSynced) => {
                        match db.contact_count().await {
                            Ok(count) => tracing::info!(count, "contacts synced"),
                            Err(error) => self.database_error(error),
                        }
                        self.send_conversations(db).await;
                        self.send_contacts(db).await;
                        // Member names may come from contacts.
                        match db.group_keys().await {
                            Ok(keys) => keys.into_iter().for_each(|key| groups.refresh(key, &mut backend)),
                            Err(error) => self.database_error(error),
                        }
                    }
                    None => {
                        tracing::info!("connection closed");
                        lost = Some(None);
                    }
                },
                update = groups.next() => match update {
                    Update::Info(key, info) => match db.set_group(key, &info).await {
                        Ok(Some(conversation)) => self.send_conversation(db, conversation).await,
                        Ok(None) => {}
                        Err(error) => self.database_error(error),
                    },
                    Update::Done(key) => groups.finished(key, &mut backend),
                },
                check = probe.next() => match check {
                    Check::Due => probe.run(&mut backend),
                    Check::Failed(error) => {
                        tracing::info!(%error, "connection check failed");
                        lost = Some(Some(error));
                    }
                },
                outcome = outbox.finished() => match outcome {
                    Outcome::Sent { id, change: false } => {
                        self.send_finished(db, id, Ok(()), &mut next_expiry).await;
                        outbox.start_next(&mut backend);
                    }
                    Outcome::Sent { id, change: true } => {
                        tracing::debug!(message = id.get(), "change sent");
                        outbox.start_next(&mut backend);
                    }
                    Outcome::Failed { id, change: false, error } => {
                        self.send_finished(db, id, Err(error), &mut next_expiry).await;
                        outbox.start_next(&mut backend);
                    }
                    Outcome::Failed { id, change: true, error } => {
                        tracing::warn!(message = id.get(), %error, "sending a change failed");
                        self.emit(Event::ChangeFailed { id, error });
                        outbox.start_next(&mut backend);
                    }
                    Outcome::Interrupted(error) => {
                        tracing::info!(%error, "send interrupted");
                        if incoming.is_some() {
                            lost = Some(Some(error));
                        }
                    }
                },
                () = tokio::time::sleep_until(wait), if retry_at.is_some() => {}
                () = due(&*self.options.clock, next_expiry) => {
                    next_expiry = self.expire(db).await;
                }
            }
            if let Some(error) = lost {
                incoming = None;
                probe.stop();
                outbox.disconnected();
                retry_at = Some(self.disconnected(error, &mut delay));
            }
        }
    }

    /// Removes messages and conversations deleted on another device.
    async fn delete_for_me(
        &self,
        db: &Database,
        messages: &[MessageRef],
        conversations: &[ChatRef],
    ) {
        match db.delete_messages(messages).await {
            Ok(deleted) => self.removed(db, deleted).await,
            Err(error) => self.database_error(error),
        }
        for chat in conversations {
            match db.delete_conversation(*chat).await {
                Ok(Some(conversation)) => self.emit(Event::ConversationRemoved(conversation)),
                Ok(None) => {}
                Err(error) => self.database_error(error),
            }
        }
    }

    /// Reports deleted messages, then their conversations.
    async fn removed(&self, db: &Database, deleted: Vec<(ConversationId, MessageId)>) {
        let mut changed = Vec::new();
        for (conversation, id) in deleted {
            self.emit(Event::MessageRemoved { conversation, id });
            if !changed.contains(&conversation) {
                changed.push(conversation);
            }
        }
        for conversation in changed {
            self.send_conversation(db, conversation).await;
        }
    }

    /// Deletes disappearing messages that are due; returns when the next
    /// one is.
    async fn expire(&self, db: &Database) -> Option<Timestamp> {
        let now = self.options.clock.now();
        match db.expire(now).await {
            Ok(expired) if expired.is_empty() => {}
            Ok(expired) => {
                tracing::debug!(count = expired.len(), "disappearing messages deleted");
                self.removed(db, expired).await;
            }
            Err(error) => self.database_error(error),
        }
        match db.next_expiry().await {
            Ok(next) => next,
            Err(error) => {
                self.database_error(error);
                Some(Timestamp::from_millis(now.as_millis() + EXPIRY_RETRY_MS))
            }
        }
    }

    /// Reports messages whose countdown just started, and schedules them.
    fn countdowns_started(&self, messages: &[Message], next_expiry: &mut Option<Timestamp>) {
        for message in messages {
            if message.expiry.is_some_and(|expiry| expiry.at.is_some()) {
                self.emit(Event::MessageUpdated(message.clone()));
                schedule(next_expiry, message);
            }
        }
    }

    /// Tells the conversation we're typing, at most every
    /// [`TYPING_RESEND`], or that we stopped.
    async fn typing<B: SignalBackend>(
        &self,
        db: &Database,
        backend: &mut B,
        typing_sent: &mut HashMap<ConversationId, Instant>,
        conversation: ConversationId,
        typing: bool,
    ) {
        if typing {
            let recent = typing_sent
                .get(&conversation)
                .is_some_and(|at: &Instant| at.elapsed() < TYPING_RESEND);
            if recent {
                return;
            }
        } else if typing_sent.remove(&conversation).is_none() {
            return;
        }
        let destination = match db.destination(conversation).await {
            // Nobody else sees Note to Self.
            Ok(Some(Destination::Contact(peer))) if peer == backend.own_id() => return,
            Ok(Some(destination)) => destination,
            Ok(None) => return,
            Err(error) => return self.database_error(error),
        };
        if typing {
            typing_sent.insert(conversation, Instant::now());
        }
        spawn_logged("sending typing", backend.send_typing(destination, typing));
    }

    /// Refreshes the group's stored state if it's missing or older than
    /// `group`'s revision.
    async fn check_group<B: SignalBackend>(
        &self,
        db: &Database,
        groups: &mut Groups,
        backend: &mut B,
        group: GroupRef,
    ) {
        match db.group_revision(group.key).await {
            Ok(Some(stored)) if stored.is_none_or(|stored| stored < group.revision) => {
                groups.refresh(group.key, backend);
            }
            Ok(_) => {}
            Err(error) => self.database_error(error),
        }
    }

    /// Stores the text as pending and queues it.
    async fn send_text<B: SignalBackend>(
        &self,
        db: &Database,
        outbox: &mut Outbox,
        backend: &mut B,
        conversation: ConversationId,
        body: MessageBody,
        reply_to: Option<MessageId>,
    ) {
        if body.as_str().trim().is_empty() {
            tracing::debug!("empty message not sent");
            return;
        }
        let quote = match reply_to {
            Some(id) => match db.message(id).await {
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
        let destination = match db.destination(conversation).await {
            Ok(Some(destination)) => destination,
            Ok(None) => {
                tracing::debug!(conversation = conversation.get(), "no such conversation");
                return;
            }
            Err(error) => return self.database_error(error),
        };
        // presage applies the same timer when sending.
        let expire_timer = match backend.expire_timer(destination).await {
            Ok(timer) => timer,
            Err(error) => {
                tracing::warn!(%error, "reading the disappearing messages timer failed");
                None
            }
        };
        // Signal identifies a message by author and sent time, so each needs
        // its own.
        let mut sent_at = Timestamp::now();
        let message = loop {
            match db
                .add_pending(conversation, sent_at, &content, expire_timer)
                .await
            {
                Ok(Some(message)) => break message,
                Ok(None) => sent_at = Timestamp::from_millis(sent_at.as_millis() + 1),
                Err(error) => return self.database_error(error),
            }
        };
        self.emit(Event::MessageAdded(message.clone()));
        self.send_conversation(db, conversation).await;
        let outgoing = text_of(&message.content);
        outbox.push(message.id, destination, outgoing, message.sent_at, backend);
    }

    /// Our sent message `id`, if it may be changed for everyone within
    /// `window`, with where it went.
    async fn own_sent(
        &self,
        db: &Database,
        id: MessageId,
        window: Duration,
    ) -> Option<(Message, Destination)> {
        let message = match db.message(id).await {
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
        let age = Timestamp::now()
            .as_millis()
            .saturating_sub(message.sent_at.as_millis());
        if message.sender.is_some() || !sent || u128::from(age) > window.as_millis() {
            tracing::debug!(message = id.get(), "message can't be changed");
            return None;
        }
        match db.destination(message.conversation).await {
            Ok(Some(destination)) => Some((message, destination)),
            Ok(None) => None,
            Err(error) => {
                self.database_error(error);
                None
            }
        }
    }

    /// Replaces our message's text here, then for everyone.
    async fn edit<B: SignalBackend>(
        &self,
        db: &Database,
        outbox: &mut Outbox,
        backend: &mut B,
        id: MessageId,
        body: MessageBody,
    ) {
        if body.as_str().trim().is_empty() {
            return;
        }
        let Some((message, destination)) = self.own_sent(db, id, EDIT_WINDOW).await else {
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
        let at = Timestamp::now();
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
        if !self.apply_own_change(db, &change).await {
            return;
        }
        let outgoing = Outgoing::Edit {
            target: message.sent_at,
            body,
            quote: quote.clone(),
        };
        outbox.push(id, destination, outgoing, at, backend);
    }

    /// Deletes a message: ours for everyone, or any just for this account.
    async fn delete<B: SignalBackend>(
        &self,
        db: &Database,
        outbox: &mut Outbox,
        backend: &mut B,
        id: MessageId,
        everyone: bool,
    ) {
        if everyone {
            let Some((message, destination)) = self.own_sent(db, id, DELETE_WINDOW).await else {
                return;
            };
            let change = MessageChange {
                destination,
                author: None,
                target: message.sent_at,
                change: Change::Delete,
            };
            if self.apply_own_change(db, &change).await {
                let outgoing = Outgoing::Delete {
                    target: message.sent_at,
                };
                outbox.push(id, destination, outgoing, Timestamp::now(), backend);
            }
            return;
        }
        let (message, destination) = match db.message(id).await {
            Ok(Some(message)) => match db.destination(message.conversation).await {
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
        self.delete_for_me(db, &[deleted], &[]).await;
        spawn_logged(
            "syncing a delete",
            backend.sync_deleted_for_me(destination, vec![(message.sender, message.sent_at)]),
        );
    }

    /// Applies our edit or delete here; `false` if it didn't apply.
    async fn apply_own_change(&self, db: &Database, change: &MessageChange) -> bool {
        match db.apply_change(change).await {
            Ok(Some(message)) => {
                let conversation = message.conversation;
                self.emit(Event::MessageUpdated(message));
                self.send_conversation(db, conversation).await;
                true
            }
            Ok(None) => false,
            Err(error) => {
                self.database_error(error);
                false
            }
        }
    }

    async fn send_finished(
        &self,
        db: &Database,
        id: MessageId,
        result: Result<(), BackendError>,
        next_expiry: &mut Option<Timestamp>,
    ) {
        let status = match result {
            Ok(()) => MessageStatus::Sent,
            Err(_) => MessageStatus::Failed,
        };
        let now = self.options.clock.now();
        let message = match db.set_status(id, status, now).await {
            Ok(Some(message)) => message,
            Ok(None) => return,
            Err(error) => return self.database_error(error),
        };
        match result {
            Ok(()) => {
                tracing::debug!(message = id.get(), "message sent");
                schedule(next_expiry, &message);
                self.emit(Event::MessageUpdated(message));
            }
            Err(error) => {
                tracing::warn!(message = id.get(), %error, "send failed");
                self.emit(Event::SendFailed { message, error });
            }
        }
    }

    /// Reports the disconnect and returns when to retry; backs off.
    fn disconnected(&self, error: Option<BackendError>, delay: &mut Duration) -> Instant {
        let retry_in = *delay;
        self.emit(Event::Disconnected { error, retry_in });
        *delay = (*delay * 2).min(self.options.reconnect_max);
        Instant::now() + retry_in
    }

    /// Stores a received message, unless it's ours and already gone on the
    /// device that sent it.
    async fn store(
        &self,
        db: &Database,
        destination: &Destination,
        message: Stored<'_>,
        next_expiry: &mut Option<Timestamp>,
    ) {
        let now = self.options.clock.now();
        if message.author.is_none()
            && let Some(timer) = message.expire_timer
            && message.sent_at.as_millis() + u64::from(timer) * 1000 <= now.as_millis()
        {
            tracing::debug!("expired message not stored");
            return;
        }
        let added = db
            .add_message(
                destination,
                message.author,
                message.sent_at,
                message.content,
                message.expire_timer,
                now,
            )
            .await;
        match added {
            Ok(Some(message)) => {
                let conversation = message.conversation;
                tracing::debug!(conversation = conversation.get(), "message stored");
                schedule(next_expiry, &message);
                self.emit(Event::MessageAdded(message));
                self.send_conversation(db, conversation).await;
            }
            Ok(None) => tracing::debug!("duplicate message ignored"),
            Err(error) => self.database_error(error),
        }
    }

    async fn send_conversation(&self, db: &Database, conversation: ConversationId) {
        match db.conversation(conversation).await {
            Ok(Some(summary)) => self.emit(Event::ConversationUpdated(summary)),
            Ok(None) => {}
            Err(error) => self.database_error(error),
        }
    }

    async fn send_contacts(&self, db: &Database) {
        match db.contacts().await {
            Ok(contacts) => self.emit(Event::Contacts(contacts)),
            Err(error) => self.database_error(error),
        }
    }

    /// Whether to ask the phone for contacts: none stored, or not asked for
    /// a day.
    async fn contacts_due(&self, db: &Database) -> bool {
        match (db.has_contacts().await, db.contacts_requested_at().await) {
            (Ok(true), Ok(Some(at))) => {
                Timestamp::now().as_millis().saturating_sub(at.as_millis()) >= CONTACTS_REFRESH_MS
            }
            _ => true,
        }
    }

    async fn send_conversations(&self, db: &Database) {
        match db.conversations().await {
            Ok(conversations) => self.emit(Event::Conversations(conversations)),
            Err(error) => self.database_error(error),
        }
    }

    fn database_error(&self, error: StorageError) {
        tracing::error!(%error, "database error");
        self.emit(Event::DatabaseError(error.to_string()));
    }
}

/// A received message to store; `author` is `None` for this account's.
struct Stored<'a> {
    author: Option<ServiceId>,
    sent_at: Timestamp,
    content: &'a Content,
    expire_timer: Option<u32>,
}

/// When expiring fails, how long until trying again.
const EXPIRY_RETRY_MS: u64 = 60_000;

/// Brings the next expiry forward if `message`'s countdown ends sooner.
fn schedule(next_expiry: &mut Option<Timestamp>, message: &Message) {
    if let Some(at) = message.expiry.and_then(|expiry| expiry.at) {
        *next_expiry = Some(next_expiry.map_or(at, |next| next.min(at)));
    }
}

/// Resolves when the next expiry may be due; never without one.
fn due(clock: &dyn Clock, at: Option<Timestamp>) -> LocalBoxFuture<'static, ()> {
    match at {
        Some(at) => clock.sleep_until(at),
        None => std::future::pending().boxed_local(),
    }
}

fn failure(error: StorageError) -> StorageFailure {
    match error {
        StorageError::KeyMissing => StorageFailure::KeyMissing,
        StorageError::KeychainUnavailable(error) => StorageFailure::KeychainUnavailable(error),
        StorageError::WrongKey => StorageFailure::WrongKey,
        other => StorageFailure::Other(other.to_string()),
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

/// How often contacts are asked of the phone, besides the syncs it pushes.
const CONTACTS_REFRESH_MS: u64 = 24 * 3600 * 1000;

/// How long an edit or delete waits for its message.
const PENDING_CHANGE_LIFETIME_MS: u64 = 24 * 3600 * 1000;

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

/// How often a typing indicator is renewed while typing; recipients drop
/// it after [`super::TYPING_TIMEOUT`].
const TYPING_RESEND: Duration = Duration::from_secs(10);

/// Tells our other devices we read these messages, and their authors too
/// if the account sends read receipts.
fn send_receipts<B: SignalBackend>(
    backend: &mut B,
    settings: AccountSettings,
    read: Vec<(ServiceId, Timestamp)>,
) {
    if settings.read_receipts {
        let me = backend.own_id();
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
                backend.send_read_receipts(author, timestamps),
            );
        }
    }
    spawn_logged("syncing reads", backend.send_read_sync(read));
}

/// Runs a background send, logging failure. Must run inside a `LocalSet`.
fn spawn_logged(what: &'static str, send: LocalBoxFuture<'static, Result<(), BackendError>>) {
    tokio::task::spawn_local(async move {
        if let Err(error) = send.await {
            tracing::info!(%error, "{what} failed");
        }
    });
}

/// Largest page [`Command::LoadMessages`] returns.
const MAX_PAGE: u32 = 500;

fn ignored(command: &Command) {
    tracing::debug!(?command, "command ignored in the current state");
}

/// "Copiale on <host>", using the short host name.
pub(crate) fn default_device_name() -> String {
    let host = gethostname::gethostname();
    let host = host.to_string_lossy();
    match host.split('.').next().map(str::trim) {
        Some(host) if !host.is_empty() => format!("Copiale on {host}"),
        _ => "Copiale".into(),
    }
}

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(work)
        .await
        .expect("blocking task panicked")
}

/// Next item from the stream; never resolves without one.
/// A connection attempt's result.
type Connecting = Result<LocalBoxStream<'static, Incoming>, BackendError>;

/// The future's output; never resolves while there is none.
async fn maybe<T>(future: &mut Option<LocalBoxFuture<'static, T>>) -> T {
    match future {
        Some(future) => future.await,
        None => std::future::pending().await,
    }
}

async fn next_incoming(stream: &mut Option<LocalBoxStream<'static, Incoming>>) -> Option<Incoming> {
    match stream {
        Some(stream) => stream.next().await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_name_mentions_copiale() {
        assert!(default_device_name().starts_with("Copiale"));
    }
}
