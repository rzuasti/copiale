//! The core's phases: unlock storage, set up the account, serve.

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
use super::{Command, CoreOptions, Event, StorageFailure};
use crate::backend::{BackendError, Incoming, Linker, SignalBackend};
use crate::model::{
    Content, ConversationId, Destination, GroupRef, MessageBody, MessageId, MessageStatus,
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
        match db.fail_pending().await {
            Ok(0) => {}
            Ok(count) => tracing::info!(count, "unsent messages marked failed"),
            Err(error) => self.database_error(error),
        }
        // Contact names title conversations; ask the phone if there are none.
        if linked_now || !db.has_contacts().await.unwrap_or(false) {
            let request = backend.request_contact_sync();
            match tokio::time::timeout(self.options.connect_timeout, request).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => tracing::warn!(%error, "requesting contacts failed"),
                Err(_) => tracing::warn!("requesting contacts timed out"),
            }
        }

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
                    Some(Command::SendText { conversation, body }) => {
                        self.send_text(db, &mut outbox, &mut backend, conversation, body).await;
                    }
                    Some(Command::RetrySend(id)) => match db.retry_send(id).await {
                        Ok(Some((destination, message))) => {
                            self.emit(Event::MessageUpdated(message.clone()));
                            outbox.push(destination, &message, &mut backend);
                        }
                        Ok(None) => tracing::debug!(message = id.get(), "nothing to retry"),
                        Err(error) => self.database_error(error),
                    },
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
                    Some(Command::LoadMessages { conversation, before, limit }) => {
                        match db.messages(conversation, before, limit.min(MAX_PAGE)).await {
                            Ok(messages) => self.emit(Event::Messages { conversation, before, messages }),
                            Err(error) => self.database_error(error),
                        }
                    }
                    Some(Command::MarkRead(conversation)) => match db.mark_read(conversation).await {
                        Ok(true) => self.send_conversation(db, conversation).await,
                        Ok(false) => {}
                        Err(error) => self.database_error(error),
                    },
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
                    Some(Incoming::Message(message)) => {
                        let destination = message
                            .group
                            .map_or(Destination::Contact(message.sender), Destination::Group);
                        self.store(db, &destination, Some(message.sender), message.timestamp, &message.content).await;
                        if let Some(group) = message.group {
                            self.check_group(db, &mut groups, &mut backend, group).await;
                        }
                    }
                    Some(Incoming::Sent(message)) => {
                        self.store(db, &message.destination, None, message.timestamp, &message.content).await;
                        if let Destination::Group(group) = message.destination {
                            self.check_group(db, &mut groups, &mut backend, group).await;
                        }
                    }
                    Some(Incoming::ContactsSynced) => {
                        match db.contact_count().await {
                            Ok(count) => tracing::info!(count, "contacts synced"),
                            Err(error) => self.database_error(error),
                        }
                        self.send_conversations(db).await;
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
                    Outcome::Sent(id) => {
                        self.send_finished(db, id, Ok(())).await;
                        outbox.start_next(&mut backend);
                    }
                    Outcome::Failed(id, error) => {
                        self.send_finished(db, id, Err(error)).await;
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
            }
            if let Some(error) = lost {
                incoming = None;
                probe.stop();
                outbox.disconnected();
                retry_at = Some(self.disconnected(error, &mut delay));
            }
        }
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
    ) {
        if body.as_str().trim().is_empty() {
            tracing::debug!("empty message not sent");
            return;
        }
        let destination = match db.destination(conversation).await {
            Ok(Some(destination)) => destination,
            Ok(None) => {
                tracing::debug!(conversation = conversation.get(), "no such conversation");
                return;
            }
            Err(error) => return self.database_error(error),
        };
        // Signal identifies a message by author and sent time, so each needs
        // its own.
        let mut sent_at = Timestamp::now();
        let message = loop {
            match db.add_pending(conversation, sent_at, &body).await {
                Ok(Some(message)) => break message,
                Ok(None) => sent_at = Timestamp::from_millis(sent_at.as_millis() + 1),
                Err(error) => return self.database_error(error),
            }
        };
        self.emit(Event::MessageAdded(message.clone()));
        self.send_conversation(db, conversation).await;
        outbox.push(destination, &message, backend);
    }

    async fn send_finished(&self, db: &Database, id: MessageId, result: Result<(), BackendError>) {
        let status = match result {
            Ok(()) => MessageStatus::Sent,
            Err(_) => MessageStatus::Failed,
        };
        let message = match db.set_status(id, status).await {
            Ok(Some(message)) => message,
            Ok(None) => return,
            Err(error) => return self.database_error(error),
        };
        match result {
            Ok(()) => {
                tracing::debug!(message = id.get(), "message sent");
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

    /// Stores a received message; `author` is `None` for this account's.
    async fn store(
        &self,
        db: &Database,
        destination: &Destination,
        author: Option<ServiceId>,
        sent_at: Timestamp,
        content: &Content,
    ) {
        match db.add_message(destination, author, sent_at, content).await {
            Ok(Some(message)) => {
                let conversation = message.conversation;
                tracing::debug!(conversation = conversation.get(), "message stored");
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

fn failure(error: StorageError) -> StorageFailure {
    match error {
        StorageError::KeyMissing => StorageFailure::KeyMissing,
        StorageError::KeychainUnavailable(error) => StorageFailure::KeychainUnavailable(error),
        StorageError::WrongKey => StorageFailure::WrongKey,
        other => StorageFailure::Other(other.to_string()),
    }
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
