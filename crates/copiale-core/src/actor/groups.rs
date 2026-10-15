//! Keeps stored group state current, in the background.

use std::collections::HashSet;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::backend::SignalBackend;
use crate::model::{GroupInfo, GroupMasterKey};

pub(super) struct Groups {
    refreshing: HashSet<GroupMasterKey>,
    updates_tx: UnboundedSender<Update>,
    updates: UnboundedReceiver<Update>,
}

pub(super) enum Update {
    /// The group's state, to store.
    Info(GroupMasterKey, GroupInfo),
    /// A refresh finished.
    Done(GroupMasterKey),
}

impl Groups {
    pub(super) fn new() -> Self {
        let (updates_tx, updates) = unbounded_channel();
        Self {
            refreshing: HashSet::new(),
            updates_tx,
            updates,
        }
    }

    /// Reads the group's state, then again after fetching the profiles of
    /// members without a name. Does nothing if a refresh is running. Must
    /// run inside a `LocalSet`.
    pub(super) fn refresh<B: SignalBackend>(&mut self, key: GroupMasterKey, backend: &mut B) {
        if !self.refreshing.insert(key) {
            return;
        }
        let first = backend.group(key);
        let fetch = backend.fetch_member_profiles(key);
        let again = backend.group(key);
        let updates = self.updates_tx.clone();
        tokio::task::spawn_local(async move {
            match first.await {
                Ok(Some(info)) => {
                    let unnamed = info.members.iter().any(|member| member.name.is_none());
                    updates.send(Update::Info(key, info)).ok();
                    if unnamed {
                        match fetch.await {
                            Ok(()) => match again.await {
                                Ok(Some(info)) => {
                                    updates.send(Update::Info(key, info)).ok();
                                }
                                Ok(None) => {}
                                Err(error) => tracing::warn!(%error, "reading a group failed"),
                            },
                            Err(error) => tracing::info!(%error, "fetching member profiles failed"),
                        }
                    }
                }
                Ok(None) => tracing::debug!(group = ?key.id(), "group state unknown"),
                Err(error) => tracing::warn!(%error, "reading a group failed"),
            }
            updates.send(Update::Done(key)).ok();
        });
    }

    /// The next update; call [`Self::finished`] for [`Update::Done`].
    pub(super) async fn next(&mut self) -> Update {
        self.updates.recv().await.expect("the sender lives in self")
    }

    pub(super) fn finished(&mut self, key: GroupMasterKey) {
        self.refreshing.remove(&key);
    }
}
