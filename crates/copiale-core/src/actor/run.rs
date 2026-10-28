//! The core's phases: unlock storage, set up the account, serve.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use futures::FutureExt as _;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;

use super::session::{Served, serve};
use super::{Command, CoreOptions, Event, StorageFailure};
use crate::backend::{BackendError, Linker};
use crate::storage::{Database, KeyLookup, SecretStore, Storage, StorageError, Wiped};

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
        while let Some(db) = self.unlock().await {
            // Closed before `Stopped`, as the process may exit right after
            // it, and before wiping the file.
            let served = closing(db, async |db| {
                let (backend, linked_now) = self.set_up_account(db, &mut linker).await?;
                self.emit(Event::Ready);
                Some(serve(&mut self, db, backend, linked_now).await)
            })
            .await;
            if served != Some(Served::Unlink) {
                break;
            }
            let (storage, secrets) = (self.storage.clone(), self.secrets.clone());
            match blocking(move || storage.wipe(&*secrets)).await {
                Ok(wiped) => {
                    tracing::info!("unlinked");
                    self.wiped(wiped);
                }
                // Opens what's left; the user can try again.
                Err(error) => self.emit(Event::DatabaseError(error.to_string())),
            }
        }
        tracing::info!("core stopped");
        self.emit(Event::Stopped);
    }

    pub(super) fn emit(&self, event: Event) {
        // A closed receiver means the UI is gone; keep going until told to stop.
        self.events.send(event).ok();
    }

    /// Removes the video sandbox's profile too; reports a key left behind.
    fn wiped(&self, wiped: Wiped) {
        if let Some(helper) = &self.options.video_helper
            && let Err(error) = helper.remove_sandbox()
        {
            tracing::warn!(%error, "can't remove the video sandbox");
        }
        if let Wiped::KeyLeft(error) = wiped {
            self.emit(Event::KeyNotRemoved(error));
        }
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
                        Ok(wiped) => {
                            self.wiped(wiped);
                            return Step::Restart;
                        }
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
}

fn failure(error: StorageError) -> StorageFailure {
    match error {
        StorageError::KeyMissing => StorageFailure::KeyMissing,
        StorageError::KeychainUnavailable(error) => StorageFailure::KeychainUnavailable(error),
        StorageError::WrongKey => StorageFailure::WrongKey,
        other => StorageFailure::Other(other.to_string()),
    }
}

pub(super) fn ignored(command: &Command) {
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

/// Runs `work` with the database, then closes it, also when `work` panics
/// (see [`Database::close`]); the panic then carries on.
async fn closing<T>(db: Database, work: impl AsyncFnOnce(&Database) -> T) -> T {
    let result = AssertUnwindSafe(work(&db)).catch_unwind().await;
    db.close().await;
    result.unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(work)
        .await
        .expect("blocking task panicked")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_name_mentions_copiale() {
        assert!(default_device_name().starts_with("Copiale"));
    }

    #[tokio::test]
    async fn a_panic_still_closes_the_database() {
        let dir = tempfile::tempdir().unwrap();
        let key = crate::storage::DatabaseKey::generate();
        let db = Database::open(&dir.path().join("test.db"), &key)
            .await
            .unwrap();
        let pool = db.pool().clone();

        let result = AssertUnwindSafe(closing(db, async |_| panic!("serving failed")))
            .catch_unwind()
            .await;

        assert!(result.is_err());
        assert!(pool.is_closed());
    }
}
