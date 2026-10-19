//! Keeps stored group state current, in the background.

use std::collections::HashMap;
use std::time::Duration;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::time::Instant;

use crate::backend::SignalBackend;
use crate::model::{GroupInfo, GroupMasterKey};

pub(super) struct Groups {
    /// Groups being refreshed, and whether to refresh again afterwards.
    refreshing: HashMap<GroupMasterKey, bool>,
    /// When groups were last fetched from the server.
    fetched: HashMap<GroupMasterKey, Instant>,
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
            refreshing: HashMap::new(),
            fetched: HashMap::new(),
            updates_tx,
            updates,
        }
    }

    /// Reads the group's state, then again after fetching the profiles of
    /// members without a name. If a refresh is running, another follows it.
    /// Must run inside a `LocalSet`.
    pub(super) fn refresh<B: SignalBackend>(&mut self, key: GroupMasterKey, backend: &mut B) {
        if let Some(again) = self.refreshing.get_mut(&key) {
            *again = true;
            return;
        }
        self.refreshing.insert(key, false);
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

    /// Whether the group may be fetched from the server at `now`: once per
    /// [`FETCH_INTERVAL`], so messages from non-members can't make us
    /// fetch it again and again. Notes the fetch if so.
    pub(super) fn may_fetch(&mut self, key: GroupMasterKey, now: Instant) -> bool {
        self.fetched
            .retain(|_, at| now.saturating_duration_since(*at) < FETCH_INTERVAL);
        if self.fetched.contains_key(&key) {
            return false;
        }
        self.fetched.insert(key, now);
        true
    }

    /// The next update; call [`Self::finished`] for [`Update::Done`].
    pub(super) async fn next(&mut self) -> Update {
        self.updates.recv().await.expect("the sender lives in self")
    }

    pub(super) fn finished<B: SignalBackend>(&mut self, key: GroupMasterKey, backend: &mut B) {
        if self.refreshing.remove(&key) == Some(true) {
            self.refresh(key, backend);
        }
    }
}

/// How often a group may be fetched from the server when a message doesn't
/// fit its stored state.
const FETCH_INTERVAL: Duration = Duration::from_secs(60);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_are_fetched_once_a_minute() {
        let mut groups = Groups::new();
        let (a, b) = (GroupMasterKey::new([1; 32]), GroupMasterKey::new([2; 32]));
        let start = Instant::now();

        assert!(groups.may_fetch(a, start));
        assert!(!groups.may_fetch(a, start + FETCH_INTERVAL / 2));
        assert!(groups.may_fetch(b, start + FETCH_INTERVAL / 2));
        assert!(groups.may_fetch(a, start + FETCH_INTERVAL));
        assert_eq!(groups.fetched.len(), 2);
    }
}
