//! Contacts synced from the phone, as presage stores them.

use presage::libsignal_service::protocol::{Aci, ServiceId as SignalServiceId};
use presage::store::ContentsStore as _;

use super::StorageError;
use super::database::{Database, database_error};
use super::messages::{millis, timestamp};
use crate::model::{Contact, PhoneNumber, ServiceId, Timestamp};

const CONTACTS_REQUESTED_AT: &str = "contacts_requested_at";

impl Database {
    /// Whether any contacts have been synced from the phone.
    pub(crate) async fn has_contacts(&self) -> Result<bool, StorageError> {
        Ok(self.contact_count().await? > 0)
    }

    /// Contacts synced from the phone.
    pub(crate) async fn contact_count(&self) -> Result<usize, StorageError> {
        let contacts = self.presage.contacts().await.map_err(presage_error)?;
        Ok(contacts.count())
    }

    /// Contacts with a name or number, by name.
    pub(crate) async fn contacts(&self) -> Result<Vec<Contact>, StorageError> {
        let stored: Vec<_> = self
            .presage
            .contacts()
            .await
            .map_err(presage_error)?
            .filter_map(Result::ok)
            .collect();
        let mut contacts = Vec::with_capacity(stored.len());
        for contact in stored {
            let id = ServiceId::new(contact.uuid);
            let name = match blank_to_none(contact.name) {
                Some(name) => Some(name),
                None => self.profile_name(id).await,
            };
            let phone = contact
                .phone_number
                .map(|number| PhoneNumber::new(number.to_string()));
            if name.is_some() || phone.is_some() {
                contacts.push(Contact { id, name, phone });
            }
        }
        contacts.sort_by_cached_key(|contact| {
            let name = contact.name.as_deref().map(str::to_lowercase);
            let phone = contact
                .phone
                .as_ref()
                .map(|phone| phone.as_str().to_owned());
            (name.is_none(), name, phone)
        });
        Ok(contacts)
    }

    /// The contact's name, else their cached profile name; `None` if
    /// unknown or blank.
    pub(super) async fn contact_name(&self, peer: ServiceId) -> Option<String> {
        let id: SignalServiceId = Aci::from(peer.uuid()).into();
        match self.presage.contact_by_id(&id).await {
            Ok(Some(contact)) => {
                if let Some(name) = blank_to_none(contact.name) {
                    return Some(name);
                }
            }
            Ok(None) => {}
            Err(error) => tracing::warn!(%error, "reading a contact failed"),
        }
        self.profile_name(peer).await
    }

    async fn profile_name(&self, peer: ServiceId) -> Option<String> {
        let id: SignalServiceId = Aci::from(peer.uuid()).into();
        let key = self.presage.profile_key(&id).await.ok().flatten()?;
        let profile = self
            .presage
            .profile(peer.uuid(), key)
            .await
            .ok()
            .flatten()?;
        blank_to_none(profile.name?.to_string())
    }

    /// When contacts were last asked of the phone.
    pub(crate) async fn contacts_requested_at(&self) -> Result<Option<Timestamp>, StorageError> {
        let value: Option<String> = sqlx::query_scalar("SELECT value FROM app_meta WHERE key = ?")
            .bind(CONTACTS_REQUESTED_AT)
            .fetch_optional(&self.app)
            .await
            .map_err(database_error)?;
        Ok(value.and_then(|value| value.parse().ok()).map(timestamp))
    }

    pub(crate) async fn set_contacts_requested_at(
        &self,
        at: Timestamp,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT INTO app_meta (key, value) VALUES (?, ?)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        )
        .bind(CONTACTS_REQUESTED_AT)
        .bind(millis(at).to_string())
        .execute(&self.app)
        .await
        .map_err(database_error)?;
        Ok(())
    }
}

fn blank_to_none(name: String) -> Option<String> {
    let trimmed = name.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn presage_error(error: impl std::error::Error) -> StorageError {
    StorageError::Database(error.to_string())
}

#[cfg(test)]
mod tests {
    use presage::libsignal_service::prelude::phonenumber;
    use presage::model::contacts::Contact as StoredContact;
    use tempfile::TempDir;
    use uuid::Uuid;

    use super::*;
    use crate::model::{Content, Destination};
    use crate::storage::DatabaseKey;

    async fn db() -> (TempDir, Database) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("test.db"), &DatabaseKey::generate())
            .await
            .unwrap();
        (dir, db)
    }

    fn peer(n: u128) -> ServiceId {
        ServiceId::new(Uuid::from_u128(n))
    }

    async fn save(db: &Database, n: u128, name: &str, phone: Option<&str>) {
        let contact = StoredContact {
            uuid: Uuid::from_u128(n),
            phone_number: phone.map(|phone| phonenumber::parse(None, phone).unwrap()),
            name: name.into(),
            verified: Default::default(),
            profile_key: Vec::new(),
            expire_timer: 0,
            expire_timer_version: 1,
            inbox_position: 0,
            avatar: None,
        };
        db.presage.clone().save_contact(&contact).await.unwrap();
    }

    async fn incoming(db: &Database, n: u128) -> crate::model::Message {
        db.add_message(
            &Destination::Contact(peer(n)),
            Some(peer(n)),
            Timestamp::from_millis(100),
            &Content::text("hi"),
            None,
            Timestamp::from_millis(0),
        )
        .await
        .unwrap()
        .unwrap()
    }

    #[tokio::test]
    async fn titles_come_from_synced_contacts() {
        let (_dir, db) = db().await;
        let named = incoming(&db, 1).await;
        let unnamed = incoming(&db, 2).await;
        assert!(!db.has_contacts().await.unwrap());

        save(&db, 1, "Alice", None).await;
        save(&db, 2, "  ", None).await;

        assert!(db.has_contacts().await.unwrap());
        assert_eq!(db.contact_count().await.unwrap(), 2);
        let named = db.conversation(named.conversation).await.unwrap().unwrap();
        let unnamed = db
            .conversation(unnamed.conversation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(named.title.as_deref(), Some("Alice"));
        assert_eq!(unnamed.title, None);
        db.close().await;
    }

    #[tokio::test]
    async fn contacts_are_listed_by_name_then_number() {
        let (_dir, db) = db().await;
        save(&db, 1, "bob", None).await;
        save(&db, 2, " Alice ", Some("+14155550123")).await;
        save(&db, 3, "", Some("+14155550100")).await;
        save(&db, 4, "", None).await;

        let contacts = db.contacts().await.unwrap();

        let listed: Vec<_> = contacts
            .iter()
            .map(|contact| {
                let phone = contact.phone.as_ref().map(PhoneNumber::as_str);
                (contact.id, contact.name.as_deref(), phone)
            })
            .collect();
        assert_eq!(
            listed,
            [
                (peer(2), Some("Alice"), Some("+14155550123")),
                (peer(1), Some("bob"), None),
                (peer(3), None, Some("+14155550100")),
            ]
        );
        db.close().await;
    }

    #[tokio::test]
    async fn contact_requests_are_remembered() {
        let (_dir, db) = db().await;
        assert_eq!(db.contacts_requested_at().await.unwrap(), None);
        let at = Timestamp::from_millis(1_800_000_000_000);
        db.set_contacts_requested_at(at).await.unwrap();
        assert_eq!(db.contacts_requested_at().await.unwrap(), Some(at));
        db.close().await;
    }
}
