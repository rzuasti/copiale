//! Contacts synced from the phone, as presage stores them.
//!
//! presage also saves every sender with a profile key as a contact, named
//! by their profile, "on first sight"; those count as unknown here.

use std::collections::HashSet;

use presage::libsignal_service::prelude::ProfileKey;
use presage::libsignal_service::profile_name::ProfileName;
use presage::libsignal_service::protocol::{Aci, ServiceId as SignalServiceId};
use presage::model::contacts::Contact as StoredContact;
use presage::store::ContentsStore as _;

use super::StorageError;
use super::database::{Database, database_error};
use super::messages::{millis, timestamp};
use crate::model::{Contact, PhoneNumber, ServiceId, Timestamp};
use crate::text::clean_name;

const CONTACTS_REQUESTED_AT: &str = "contacts_requested_at";

impl Database {
    /// Whether any contacts have been synced from the phone.
    pub(crate) async fn has_contacts(&self) -> Result<bool, StorageError> {
        Ok(self.contact_count().await? > 0)
    }

    /// Contacts synced from the phone.
    pub(crate) async fn contact_count(&self) -> Result<usize, StorageError> {
        let contacts = self.presage.contacts().await.map_err(presage_error)?;
        Ok(contacts.filter_map(Result::ok).filter(synced).count())
    }

    /// Contacts with a name or number, by name.
    pub(crate) async fn contacts(&self) -> Result<Vec<Contact>, StorageError> {
        let stored: Vec<_> = self
            .presage
            .contacts()
            .await
            .map_err(presage_error)?
            .filter_map(Result::ok)
            .filter(synced)
            .collect();
        let mut contacts = Vec::with_capacity(stored.len());
        for contact in stored {
            let id = ServiceId::new(contact.uuid);
            let name = match clean_name(&contact.name) {
                Some(name) => Some(name),
                None => self.profile_name(id).await,
            };
            let phone = contact
                .phone_number
                .map(|number| PhoneNumber::new(number.to_string()));
            if name.is_some() || phone.is_some() {
                let invite_only = self.profile_key(id).await.is_none();
                contacts.push(Contact {
                    id,
                    name,
                    phone,
                    invite_only,
                });
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

    /// Deletes synced contacts not in `kept`; returns how many.
    pub(crate) async fn prune_contacts(&self, kept: &[ServiceId]) -> Result<usize, StorageError> {
        let kept: HashSet<_> = kept.iter().map(ServiceId::uuid).collect();
        let stale: Vec<_> = self
            .presage
            .contacts()
            .await
            .map_err(presage_error)?
            .filter_map(Result::ok)
            .filter(|contact| synced(contact) && !kept.contains(&contact.uuid))
            .map(|contact| contact.uuid)
            .collect();
        let mut tx = self.begin_write().await?;
        for uuid in &stale {
            sqlx::query("DELETE FROM contacts WHERE uuid = ? AND length(profile_key) = 0")
                .bind(uuid.as_bytes().as_slice())
                .execute(&mut *tx)
                .await
                .map_err(database_error)?;
        }
        tx.commit().await.map_err(database_error)?;
        Ok(stale.len())
    }

    async fn profile_key(&self, peer: ServiceId) -> Option<ProfileKey> {
        let id: SignalServiceId = Aci::from(peer.uuid()).into();
        self.presage.profile_key(&id).await.ok().flatten()
    }

    async fn profile_name(&self, peer: ServiceId) -> Option<String> {
        let key = self.profile_key(peer).await?;
        let profile = self
            .presage
            .profile(peer.uuid(), key)
            .await
            .ok()
            .flatten()?;
        clean_name(&profile.name?.to_string())
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

/// Whether the phone synced `contact`: those carry no profile key, while
/// presage's first-sight ones always do.
pub(crate) fn synced(contact: &StoredContact) -> bool {
    contact.profile_key.is_empty()
}

/// The contact's name, else their profile name from its parts, else the
/// name presage saved on first sight, cleaned; `None` if unknown or blank.
pub(super) fn display_name(
    contact: Option<String>,
    given_name: Option<String>,
    family_name: Option<String>,
    first_sight: Option<String>,
) -> Option<String> {
    let profile = || {
        let name = ProfileName {
            given_name: given_name?,
            family_name,
        };
        clean_name(&name.to_string())
    };
    contact
        .as_deref()
        .and_then(clean_name)
        .or_else(profile)
        .or_else(|| first_sight.as_deref().and_then(clean_name))
}

fn presage_error(error: impl std::error::Error) -> StorageError {
    StorageError::Database(error.to_string())
}

#[cfg(test)]
mod tests {
    use presage::libsignal_service::Profile;
    use presage::libsignal_service::prelude::phonenumber;
    use uuid::Uuid;

    use super::*;
    use crate::model::{Content, Destination};
    use crate::storage::testing::{db, peer};

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

    /// Saves a contact as presage does on first sight.
    async fn save_first_sight(db: &Database, n: u128, name: &str) {
        let contact = StoredContact {
            uuid: Uuid::from_u128(n),
            phone_number: None,
            name: name.into(),
            verified: Default::default(),
            profile_key: vec![n as u8; 32],
            expire_timer: 0,
            expire_timer_version: 1,
            inbox_position: 0,
            avatar: None,
        };
        db.presage.clone().save_contact(&contact).await.unwrap();
    }

    #[test]
    fn synced_contacts_carry_no_profile_key() {
        let contact = presage::libsignal_service::models::Contact {
            uuid: Uuid::from_u128(1),
            phone_number: None,
            name: "Alice".into(),
            expire_timer: 0,
            expire_timer_version: 1,
            inbox_position: 0,
            avatar: None,
        };
        assert!(synced(&contact.into()));
    }

    #[tokio::test]
    async fn first_sight_contacts_are_not_listed() {
        let (_dir, db) = db().await;
        save_first_sight(&db, 1, "Stranger").await;
        assert!(!db.has_contacts().await.unwrap());
        assert!(db.contacts().await.unwrap().is_empty());

        save(&db, 2, "Alice", None).await;
        assert_eq!(db.contact_count().await.unwrap(), 1);
        let ids: Vec<_> = db.contacts().await.unwrap().iter().map(|c| c.id).collect();
        assert_eq!(ids, [peer(2)]);
        db.close().await;
    }

    #[tokio::test]
    async fn pruning_drops_only_synced_contacts_not_kept() {
        let (_dir, db) = db().await;
        save(&db, 1, "Alice", None).await;
        save(&db, 2, "Bob", None).await;
        save_first_sight(&db, 3, "Stranger").await;

        assert_eq!(db.prune_contacts(&[peer(1)]).await.unwrap(), 1);
        assert_eq!(db.prune_contacts(&[peer(1)]).await.unwrap(), 0);

        let ids: Vec<_> = db.contacts().await.unwrap().iter().map(|c| c.id).collect();
        assert_eq!(ids, [peer(1)]);
        let first_sight = db
            .presage
            .contact_by_id(&Aci::from(Uuid::from_u128(3)).into())
            .await
            .unwrap();
        assert!(first_sight.is_some());
        db.close().await;
    }

    #[tokio::test]
    async fn first_sight_names_come_after_profile_names() {
        let (_dir, db) = db().await;
        let conversations =
            [incoming(&db, 1).await, incoming(&db, 2).await].map(|message| message.conversation);
        save_first_sight(&db, 1, "Seen").await;
        save_first_sight(&db, 2, "Seen").await;
        save_profile(&db, 2, "Grace", None).await;

        let mut titles = Vec::new();
        for id in conversations {
            titles.push(db.conversation(id).await.unwrap().unwrap().title);
        }

        assert_eq!(titles, [Some("Seen".into()), Some("Grace".into())]);
        db.close().await;
    }

    async fn incoming(db: &Database, n: u128) -> crate::model::Message {
        db.add_message(
            &Destination::Contact(peer(n)),
            Some(peer(n)),
            Timestamp::from_millis(100),
            &Content::text("hi"),
            None,
            Some(Timestamp::from_millis(100)),
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

    async fn save_profile(db: &Database, n: u128, given_name: &str, family_name: Option<&str>) {
        let profile = Profile {
            name: Some(ProfileName {
                given_name: given_name.into(),
                family_name: family_name.map(Into::into),
            }),
            about: None,
            about_emoji: None,
            avatar: None,
            unrestricted_unidentified_access: false,
        };
        let key = ProfileKey::create([n as u8; 32]);
        db.presage
            .clone()
            .save_profile(Uuid::from_u128(n), key, profile)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn contacts_without_a_profile_key_are_invite_only() {
        let (_dir, db) = db().await;
        save(&db, 1, "Alice", None).await;
        save(&db, 2, "Bob", None).await;
        save_profile(&db, 2, "Bob", None).await;

        let invite_only: Vec<_> = db
            .contacts()
            .await
            .unwrap()
            .iter()
            .map(|contact| (contact.id, contact.invite_only))
            .collect();
        assert_eq!(invite_only, [(peer(1), true), (peer(2), false)]);
        db.close().await;
    }

    #[tokio::test]
    async fn titles_fall_back_to_profile_names() {
        let (_dir, db) = db().await;
        let conversations = [
            incoming(&db, 1).await,
            incoming(&db, 2).await,
            incoming(&db, 3).await,
            incoming(&db, 4).await,
        ]
        .map(|message| message.conversation);
        save(&db, 1, "Alice", None).await;
        save_profile(&db, 1, "Ali", None).await;
        save(&db, 2, " ", None).await;
        save_profile(&db, 2, "Ada", Some("Lovelace")).await;
        save_profile(&db, 3, "Grace", None).await;
        save_profile(&db, 4, " ", None).await;

        let mut titles = Vec::new();
        for id in conversations {
            titles.push(db.conversation(id).await.unwrap().unwrap().title);
        }
        let listed: Vec<_> = db
            .conversations()
            .await
            .unwrap()
            .into_iter()
            .map(|summary| (summary.id, summary.title))
            .collect();

        assert_eq!(
            titles,
            [
                Some("Alice".into()),
                Some("Ada Lovelace".into()),
                Some("Grace".into()),
                None,
            ]
        );
        for (id, title) in conversations.into_iter().zip(&titles) {
            assert!(listed.contains(&(id, title.clone())));
        }
        db.close().await;
    }

    #[tokio::test]
    async fn names_lose_bidi_controls_and_invisible_characters() {
        let (_dir, db) = db().await;
        let conversations =
            [incoming(&db, 1).await, incoming(&db, 2).await].map(|message| message.conversation);
        save(&db, 1, "Ali\u{202e}ce", None).await;
        save(&db, 2, "\u{200b}", None).await;
        save_profile(&db, 2, "Gr\u{2066}ace", Some("\u{feff}")).await;

        let mut titles = Vec::new();
        for id in conversations {
            titles.push(db.conversation(id).await.unwrap().unwrap().title);
        }
        let names: Vec<_> = db
            .contacts()
            .await
            .unwrap()
            .into_iter()
            .map(|contact| contact.name)
            .collect();

        assert_eq!(titles, [Some("Alice".into()), Some("Grace".into())]);
        assert_eq!(names, [Some("Alice".into()), Some("Grace".into())]);
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
