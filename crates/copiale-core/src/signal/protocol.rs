//! presage's protocol store for this account's ACI, noting identity key
//! changes and refusing to send to a verified key's replacement.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use presage::libsignal_service::pre_keys::{KyberPreKeyStoreExt, PreKeysStore};
use presage::libsignal_service::prelude::{SessionStoreExt, Uuid};
use presage::libsignal_service::protocol::{
    DeviceId, Direction, IdentityChange, IdentityKey, IdentityKeyPair, IdentityKeyStore,
    KyberPreKeyId, KyberPreKeyRecord, KyberPreKeyStore, PreKeyId, PreKeyRecord, PreKeyStore,
    ProtocolAddress, ProtocolStore, PublicKey, SenderKeyRecord, SenderKeyStore,
    ServiceId as SignalServiceId, SessionRecord, SessionStore, SignalProtocolError, SignedPreKeyId,
    SignedPreKeyRecord, SignedPreKeyStore,
};
use presage::store::Store;
use presage_store_sqlite::SqliteStore;
use tokio::sync::mpsc::UnboundedSender;

use crate::model::ServiceId;
use crate::storage::VerifiedKeys;

type Inner = <SqliteStore as Store>::AciStore;

type Result<T> = std::result::Result<T, SignalProtocolError>;

/// Delegates to presage's store, except for trusting and saving identities.
#[derive(Clone)]
pub(crate) struct AciStore {
    inner: Inner,
    verified: VerifiedKeys,
    /// Accounts whose identity key changed.
    changes: UnboundedSender<ServiceId>,
}

impl AciStore {
    pub(crate) fn new(
        inner: Inner,
        verified: VerifiedKeys,
        changes: UnboundedSender<ServiceId>,
    ) -> Self {
        Self {
            inner,
            verified,
            changes,
        }
    }
}

/// The account at `address`, if it's an ACI.
fn aci(address: &ProtocolAddress) -> Option<ServiceId> {
    match SignalServiceId::parse_from_service_id_string(address.name())? {
        SignalServiceId::Aci(aci) => Some(ServiceId::new(Uuid::from(aci))),
        SignalServiceId::Pni(_) => None,
    }
}

impl ProtocolStore for AciStore {}

#[async_trait(?Send)]
impl IdentityKeyStore for AciStore {
    async fn get_identity_key_pair(&self) -> Result<IdentityKeyPair> {
        self.inner.get_identity_key_pair().await
    }

    async fn get_local_registration_id(&self) -> Result<u32> {
        self.inner.get_local_registration_id().await
    }

    /// Reports a key replacing a different one; presage's store reports
    /// every save of a known address as a replacement.
    async fn save_identity(
        &mut self,
        address: &ProtocolAddress,
        identity: &IdentityKey,
    ) -> Result<IdentityChange> {
        let previous = self.inner.get_identity(address).await?;
        self.inner.save_identity(address, identity).await?;
        let changed = previous.is_some_and(|previous| previous != *identity);
        if changed && let Some(peer) = aci(address) {
            tracing::info!(?peer, "identity key changed");
            self.changes.send(peer).ok();
        }
        Ok(IdentityChange::from_changed(changed))
    }

    /// Receives from any key, as Signal does; sends only to the key
    /// verified on the phone, if any. A refused key is saved, so the change
    /// shows and the user can confirm it.
    async fn is_trusted_identity(
        &self,
        address: &ProtocolAddress,
        identity: &IdentityKey,
        direction: Direction,
    ) -> Result<bool> {
        let Some(peer) = aci(address).filter(|_| direction == Direction::Sending) else {
            return self
                .inner
                .is_trusted_identity(address, identity, direction)
                .await;
        };
        let verified = self.verified.get(peer.uuid()).await.map_err(|error| {
            SignalProtocolError::InvalidState("verified identities", error.to_string())
        })?;
        let trusted = verified.is_none_or(|key| key.as_bytes() == &*identity.serialize());
        if !trusted {
            tracing::warn!(?peer, "not sending to a changed verified identity");
            self.clone().save_identity(address, identity).await?;
        }
        Ok(trusted)
    }

    async fn get_identity(&self, address: &ProtocolAddress) -> Result<Option<IdentityKey>> {
        self.inner.get_identity(address).await
    }
}

#[async_trait(?Send)]
impl SessionStore for AciStore {
    async fn load_session(&self, address: &ProtocolAddress) -> Result<Option<SessionRecord>> {
        self.inner.load_session(address).await
    }

    async fn store_session(
        &mut self,
        address: &ProtocolAddress,
        record: &SessionRecord,
    ) -> Result<()> {
        self.inner.store_session(address, record).await
    }
}

#[async_trait(?Send)]
impl SessionStoreExt for AciStore {
    async fn get_sub_device_sessions(&self, name: &SignalServiceId) -> Result<Vec<DeviceId>> {
        self.inner.get_sub_device_sessions(name).await
    }

    async fn delete_session(&self, address: &ProtocolAddress) -> Result<()> {
        self.inner.delete_session(address).await
    }

    async fn delete_all_sessions(&self, name: &SignalServiceId) -> Result<usize> {
        self.inner.delete_all_sessions(name).await
    }
}

#[async_trait(?Send)]
impl PreKeyStore for AciStore {
    async fn get_pre_key(&self, prekey_id: PreKeyId) -> Result<PreKeyRecord> {
        self.inner.get_pre_key(prekey_id).await
    }

    async fn save_pre_key(&mut self, prekey_id: PreKeyId, record: &PreKeyRecord) -> Result<()> {
        self.inner.save_pre_key(prekey_id, record).await
    }

    async fn remove_pre_key(&mut self, prekey_id: PreKeyId) -> Result<()> {
        self.inner.remove_pre_key(prekey_id).await
    }
}

#[async_trait(?Send)]
impl PreKeysStore for AciStore {
    async fn next_pre_key_id(&self) -> Result<u32> {
        self.inner.next_pre_key_id().await
    }

    async fn next_signed_pre_key_id(&self) -> Result<u32> {
        self.inner.next_signed_pre_key_id().await
    }

    async fn next_pq_pre_key_id(&self) -> Result<u32> {
        self.inner.next_pq_pre_key_id().await
    }

    async fn signed_pre_keys_count(&self) -> Result<usize> {
        self.inner.signed_pre_keys_count().await
    }

    async fn kyber_pre_keys_count(&self, last_resort: bool) -> Result<usize> {
        self.inner.kyber_pre_keys_count(last_resort).await
    }

    async fn signed_prekey_id(&self) -> Result<Option<SignedPreKeyId>> {
        self.inner.signed_prekey_id().await
    }

    async fn last_resort_kyber_prekey_id(&self) -> Result<Option<KyberPreKeyId>> {
        self.inner.last_resort_kyber_prekey_id().await
    }
}

#[async_trait(?Send)]
impl SignedPreKeyStore for AciStore {
    async fn get_signed_pre_key(
        &self,
        signed_prekey_id: SignedPreKeyId,
    ) -> Result<SignedPreKeyRecord> {
        self.inner.get_signed_pre_key(signed_prekey_id).await
    }

    async fn save_signed_pre_key(
        &mut self,
        signed_prekey_id: SignedPreKeyId,
        record: &SignedPreKeyRecord,
    ) -> Result<()> {
        self.inner
            .save_signed_pre_key(signed_prekey_id, record)
            .await
    }
}

#[async_trait(?Send)]
impl KyberPreKeyStore for AciStore {
    async fn get_kyber_pre_key(&self, kyber_prekey_id: KyberPreKeyId) -> Result<KyberPreKeyRecord> {
        self.inner.get_kyber_pre_key(kyber_prekey_id).await
    }

    async fn save_kyber_pre_key(
        &mut self,
        kyber_prekey_id: KyberPreKeyId,
        record: &KyberPreKeyRecord,
    ) -> Result<()> {
        self.inner.save_kyber_pre_key(kyber_prekey_id, record).await
    }

    async fn mark_kyber_pre_key_used(
        &mut self,
        kyber_prekey_id: KyberPreKeyId,
        ec_prekey_id: SignedPreKeyId,
        base_key: &PublicKey,
    ) -> Result<()> {
        self.inner
            .mark_kyber_pre_key_used(kyber_prekey_id, ec_prekey_id, base_key)
            .await
    }
}

#[async_trait(?Send)]
impl KyberPreKeyStoreExt for AciStore {
    async fn store_last_resort_kyber_pre_key(
        &mut self,
        kyber_prekey_id: KyberPreKeyId,
        record: &KyberPreKeyRecord,
    ) -> Result<()> {
        self.inner
            .store_last_resort_kyber_pre_key(kyber_prekey_id, record)
            .await
    }

    async fn load_last_resort_kyber_pre_keys(&self) -> Result<Vec<KyberPreKeyRecord>> {
        self.inner.load_last_resort_kyber_pre_keys().await
    }

    async fn remove_kyber_pre_key(&mut self, kyber_prekey_id: KyberPreKeyId) -> Result<()> {
        self.inner.remove_kyber_pre_key(kyber_prekey_id).await
    }

    async fn mark_all_one_time_kyber_pre_keys_stale_if_necessary(
        &mut self,
        stale_time: DateTime<Utc>,
    ) -> Result<()> {
        self.inner
            .mark_all_one_time_kyber_pre_keys_stale_if_necessary(stale_time)
            .await
    }

    async fn delete_all_stale_one_time_kyber_pre_keys(
        &mut self,
        threshold: DateTime<Utc>,
        min_count: usize,
    ) -> Result<()> {
        self.inner
            .delete_all_stale_one_time_kyber_pre_keys(threshold, min_count)
            .await
    }
}

#[async_trait(?Send)]
impl SenderKeyStore for AciStore {
    async fn store_sender_key(
        &mut self,
        sender: &ProtocolAddress,
        distribution_id: Uuid,
        record: &SenderKeyRecord,
    ) -> Result<()> {
        self.inner
            .store_sender_key(sender, distribution_id, record)
            .await
    }

    async fn load_sender_key(
        &mut self,
        sender: &ProtocolAddress,
        distribution_id: Uuid,
    ) -> Result<Option<SenderKeyRecord>> {
        self.inner.load_sender_key(sender, distribution_id).await
    }
}

#[cfg(test)]
mod tests {
    use presage::libsignal_service::protocol::{Aci, IdentityKeyPair, Pni};
    use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

    use super::*;
    use crate::model::IdentityKey as Key;
    use crate::storage::{Database, DatabaseKey};

    const PEER: Uuid = Uuid::from_u128(0x1234);

    async fn store() -> (
        tempfile::TempDir,
        Database,
        AciStore,
        UnboundedReceiver<ServiceId>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("test.db"), &DatabaseKey::generate())
            .await
            .unwrap();
        let (changes, changed) = unbounded_channel();
        let store = AciStore::new(
            db.presage_store().aci_protocol_store(),
            db.verified_keys(),
            changes,
        );
        (dir, db, store, changed)
    }

    fn address(id: SignalServiceId) -> ProtocolAddress {
        ProtocolAddress::new(id.service_id_string(), DeviceId::try_from(1u32).unwrap())
    }

    fn new_key() -> IdentityKey {
        *IdentityKeyPair::generate(&mut rand::rng()).identity_key()
    }

    #[tokio::test]
    async fn reports_only_keys_replacing_different_ones() {
        let (_dir, db, mut store, mut changed) = store().await;
        let peer = address(Aci::from(PEER).into());
        let (first, second) = (new_key(), new_key());

        for _ in 0..2 {
            let change = store.save_identity(&peer, &first).await.unwrap();
            assert_eq!(change, IdentityChange::NewOrUnchanged);
        }
        assert!(changed.try_recv().is_err());
        let change = store.save_identity(&peer, &second).await.unwrap();
        assert_eq!(change, IdentityChange::ReplacedExisting);
        assert_eq!(changed.try_recv().unwrap(), ServiceId::new(PEER));

        let pni = address(Pni::from(PEER).into());
        store.save_identity(&pni, &first).await.unwrap();
        store.save_identity(&pni, &second).await.unwrap();
        assert!(changed.try_recv().is_err(), "only ACIs are reported");
        db.close().await;
    }

    #[tokio::test]
    async fn sends_only_to_the_verified_key_and_saves_a_refused_one() {
        let (_dir, db, mut store, mut changed) = store().await;
        let peer = address(Aci::from(PEER).into());
        let (verified, other) = (new_key(), new_key());
        store.save_identity(&peer, &verified).await.unwrap();
        let trusted = |key, direction| {
            let store = store.clone();
            let peer = peer.clone();
            async move {
                store
                    .is_trusted_identity(&peer, &key, direction)
                    .await
                    .unwrap()
            }
        };
        assert!(trusted(other, Direction::Sending).await, "nothing verified");
        db.set_verified(
            ServiceId::new(PEER),
            Some(&Key::new(verified.serialize().into_vec())),
        )
        .await
        .unwrap();

        assert!(trusted(verified, Direction::Sending).await);
        assert!(trusted(other, Direction::Receiving).await);
        assert!(changed.try_recv().is_err());
        assert!(!trusted(other, Direction::Sending).await);
        assert_eq!(store.get_identity(&peer).await.unwrap(), Some(other));
        assert_eq!(changed.try_recv().unwrap(), ServiceId::new(PEER));
        // Encrypting with the session to the refused key is refused too.
        assert!(!trusted(other, Direction::Sending).await);
        assert!(changed.try_recv().is_err(), "already saved");

        db.set_verified(ServiceId::new(PEER), None).await.unwrap();
        assert!(trusted(other, Direction::Sending).await, "confirmed");
        db.close().await;
    }
}
