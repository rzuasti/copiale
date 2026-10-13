//! presage store over [`SqliteStore`] that doesn't keep messages: Copiale's
//! own tables hold them, with edits, deletes and expiry applied.

use std::future::Future;
use std::ops::RangeBounds;

use presage::AvatarBytes;
use presage::libsignal_service::Profile;
use presage::libsignal_service::libsignal_account_keys::AccountEntropyPool;
use presage::libsignal_service::prelude::{Content, MasterKey, ProfileKey, Uuid};
use presage::libsignal_service::protocol::{IdentityKeyPair, SenderCertificate, ServiceId};
use presage::libsignal_service::zkgroup::GroupMasterKeyBytes;
use presage::manager::RegistrationData;
use presage::model::contacts::Contact;
use presage::model::groups::Group;
use presage::store::{ContentsStore, StateStore, StickerPack, Store, Thread};
use presage_store_sqlite::{SqliteStore, SqliteStoreError};

/// Delegates to [`SqliteStore`], except that [`ContentsStore::save_message`]
/// stores nothing.
#[derive(Clone)]
pub(crate) struct AppStore {
    inner: SqliteStore,
}

impl AppStore {
    pub(crate) fn new(inner: SqliteStore) -> Self {
        Self { inner }
    }
}

type Error = SqliteStoreError;

impl StateStore for AppStore {
    type StateStoreError = Error;

    fn load_registration_data(
        &self,
    ) -> impl Future<Output = Result<Option<RegistrationData>, Error>> {
        self.inner.load_registration_data()
    }

    fn set_aci_identity_key_pair(
        &self,
        key_pair: IdentityKeyPair,
    ) -> impl Future<Output = Result<(), Error>> {
        self.inner.set_aci_identity_key_pair(key_pair)
    }

    fn set_pni_identity_key_pair(
        &self,
        key_pair: IdentityKeyPair,
    ) -> impl Future<Output = Result<(), Error>> {
        self.inner.set_pni_identity_key_pair(key_pair)
    }

    fn save_registration_data(
        &mut self,
        state: &RegistrationData,
    ) -> impl Future<Output = Result<(), Error>> + Send {
        self.inner.save_registration_data(state)
    }

    fn sender_certificate(&self) -> impl Future<Output = Result<Option<SenderCertificate>, Error>> {
        self.inner.sender_certificate()
    }

    fn save_sender_certificate(
        &self,
        certificate: &SenderCertificate,
    ) -> impl Future<Output = Result<(), Error>> {
        self.inner.save_sender_certificate(certificate)
    }

    fn is_registered(&self) -> impl Future<Output = bool> {
        self.inner.is_registered()
    }

    fn clear_registration(&mut self) -> impl Future<Output = Result<(), Error>> {
        self.inner.clear_registration()
    }

    fn fetch_master_key(&self) -> impl Future<Output = Result<Option<MasterKey>, Error>> {
        self.inner.fetch_master_key()
    }

    fn store_master_key(
        &self,
        master_key: Option<&MasterKey>,
    ) -> impl Future<Output = Result<(), Error>> {
        self.inner.store_master_key(master_key)
    }

    fn fetch_account_entropy_pool(
        &self,
    ) -> impl Future<Output = Result<Option<AccountEntropyPool>, Error>> {
        self.inner.fetch_account_entropy_pool()
    }

    fn store_account_entropy_pool(
        &self,
        aep: Option<&AccountEntropyPool>,
    ) -> impl Future<Output = Result<(), Error>> {
        self.inner.store_account_entropy_pool(aep)
    }
}

impl ContentsStore for AppStore {
    type ContentsStoreError = Error;
    type ContactsIter = <SqliteStore as ContentsStore>::ContactsIter;
    type GroupsIter = <SqliteStore as ContentsStore>::GroupsIter;
    type MessagesIter = <SqliteStore as ContentsStore>::MessagesIter;
    type StickerPacksIter = <SqliteStore as ContentsStore>::StickerPacksIter;

    fn clear_profiles(&mut self) -> impl Future<Output = Result<(), Error>> {
        self.inner.clear_profiles()
    }

    fn clear_contents(&mut self) -> impl Future<Output = Result<(), Error>> {
        self.inner.clear_contents()
    }

    fn clear_messages(&mut self) -> impl Future<Output = Result<(), Error>> {
        self.inner.clear_messages()
    }

    fn clear_thread(&mut self, thread: &Thread) -> impl Future<Output = Result<(), Error>> {
        self.inner.clear_thread(thread)
    }

    /// Stores nothing: messages live in Copiale's tables.
    async fn save_message(&self, _thread: &Thread, _message: Content) -> Result<(), Error> {
        Ok(())
    }

    fn delete_message(
        &mut self,
        thread: &Thread,
        timestamp: u64,
    ) -> impl Future<Output = Result<bool, Error>> {
        self.inner.delete_message(thread, timestamp)
    }

    fn message(
        &self,
        thread: &Thread,
        timestamp: u64,
    ) -> impl Future<Output = Result<Option<Content>, Error>> {
        self.inner.message(thread, timestamp)
    }

    fn thread_for_sender_and_timestamp(
        &self,
        sender: &ServiceId,
        timestamp: u64,
    ) -> impl Future<Output = Result<Option<Thread>, Error>> {
        self.inner
            .thread_for_sender_and_timestamp(sender, timestamp)
    }

    fn messages(
        &self,
        thread: &Thread,
        range: impl RangeBounds<u64>,
    ) -> impl Future<Output = Result<Self::MessagesIter, Error>> {
        self.inner.messages(thread, range)
    }

    fn expire_timer(
        &self,
        thread: &Thread,
    ) -> impl Future<Output = Result<Option<(u32, u32)>, Error>> {
        self.inner.expire_timer(thread)
    }

    fn update_expire_timer(
        &mut self,
        thread: &Thread,
        timer: u32,
        version: u32,
    ) -> impl Future<Output = Result<(), Error>> {
        self.inner.update_expire_timer(thread, timer, version)
    }

    fn clear_contacts(&mut self) -> impl Future<Output = Result<(), Error>> {
        self.inner.clear_contacts()
    }

    fn save_contact(
        &mut self,
        contact: &Contact,
    ) -> impl Future<Output = Result<(), Error>> + Send {
        self.inner.save_contact(contact)
    }

    fn contacts(&self) -> impl Future<Output = Result<Self::ContactsIter, Error>> {
        self.inner.contacts()
    }

    fn contact_by_id(
        &self,
        id: &ServiceId,
    ) -> impl Future<Output = Result<Option<Contact>, Error>> + Send {
        self.inner.contact_by_id(id)
    }

    fn clear_groups(&mut self) -> impl Future<Output = Result<(), Error>> {
        self.inner.clear_groups()
    }

    fn save_group(
        &self,
        master_key: GroupMasterKeyBytes,
        group: impl Into<Group>,
    ) -> impl Future<Output = Result<(), Error>> {
        self.inner.save_group(master_key, group)
    }

    fn groups(&self) -> impl Future<Output = Result<Self::GroupsIter, Error>> {
        self.inner.groups()
    }

    fn group(
        &self,
        master_key: GroupMasterKeyBytes,
    ) -> impl Future<Output = Result<Option<Group>, Error>> {
        self.inner.group(master_key)
    }

    fn save_group_avatar(
        &self,
        master_key: GroupMasterKeyBytes,
        avatar: &AvatarBytes,
    ) -> impl Future<Output = Result<(), Error>> {
        self.inner.save_group_avatar(master_key, avatar)
    }

    fn group_avatar(
        &self,
        master_key: GroupMasterKeyBytes,
    ) -> impl Future<Output = Result<Option<AvatarBytes>, Error>> {
        self.inner.group_avatar(master_key)
    }

    fn upsert_profile_key(
        &mut self,
        uuid: &Uuid,
        key: ProfileKey,
    ) -> impl Future<Output = Result<bool, Error>> + Send {
        self.inner.upsert_profile_key(uuid, key)
    }

    fn profile_key(
        &self,
        service_id: &ServiceId,
    ) -> impl Future<Output = Result<Option<ProfileKey>, Error>> + Send {
        self.inner.profile_key(service_id)
    }

    fn save_profile(
        &mut self,
        uuid: Uuid,
        key: ProfileKey,
        profile: Profile,
    ) -> impl Future<Output = Result<(), Error>> {
        self.inner.save_profile(uuid, key, profile)
    }

    fn profile(
        &self,
        uuid: Uuid,
        key: ProfileKey,
    ) -> impl Future<Output = Result<Option<Profile>, Error>> {
        self.inner.profile(uuid, key)
    }

    fn save_profile_avatar(
        &mut self,
        uuid: Uuid,
        key: ProfileKey,
        avatar: &AvatarBytes,
    ) -> impl Future<Output = Result<(), Error>> {
        self.inner.save_profile_avatar(uuid, key, avatar)
    }

    fn profile_avatar(
        &self,
        uuid: Uuid,
        key: ProfileKey,
    ) -> impl Future<Output = Result<Option<AvatarBytes>, Error>> {
        self.inner.profile_avatar(uuid, key)
    }

    fn add_sticker_pack(
        &mut self,
        pack: &StickerPack,
    ) -> impl Future<Output = Result<(), Error>> + Send {
        self.inner.add_sticker_pack(pack)
    }

    fn sticker_pack(&self, id: &[u8]) -> impl Future<Output = Result<Option<StickerPack>, Error>> {
        self.inner.sticker_pack(id)
    }

    fn remove_sticker_pack(&mut self, id: &[u8]) -> impl Future<Output = Result<bool, Error>> {
        self.inner.remove_sticker_pack(id)
    }

    fn sticker_packs(&self) -> impl Future<Output = Result<Self::StickerPacksIter, Error>> {
        self.inner.sticker_packs()
    }
}

impl Store for AppStore {
    type Error = Error;
    type AciStore = <SqliteStore as Store>::AciStore;
    type PniStore = <SqliteStore as Store>::PniStore;

    fn clear(&mut self) -> impl Future<Output = Result<(), Error>> + Send {
        self.inner.clear()
    }

    fn aci_protocol_store(&self) -> Self::AciStore {
        self.inner.aci_protocol_store()
    }

    fn pni_protocol_store(&self) -> Self::PniStore {
        self.inner.pni_protocol_store()
    }
}
