//! Received presage content as [`Incoming`] core events.

use presage::libsignal_service::cipher::DecryptionFailure;
use presage::libsignal_service::content::{Content, ContentBody};
use presage::libsignal_service::groups_v2::{GroupChange, GroupOperations, Role};
use presage::libsignal_service::prelude::{GroupSecretParams, ProtobufMessage as _};
use presage::libsignal_service::proto;
use presage::libsignal_service::proto::call_message::offer;
use presage::libsignal_service::proto::sync_message::{Content as SyncContent, Sent};
use presage::libsignal_service::proto::{
    AddressableMessage, AttachmentPointer, CallMessage, ConversationIdentifier, DataMessage,
    EditMessage, GroupContextV2, ReceiptMessage, SyncMessage, TypingMessage, addressable_message,
    attachment_pointer, conversation_identifier, data_message, receipt_message, typing_message,
};
use presage::libsignal_service::protocol::{Aci, ContentHint, ServiceId as SignalServiceId};
use presage::model::messages::Received;

use crate::backend::{Incoming, RetryRequest, Undecryptable};
use crate::model::{
    self, Attachment, Change, ChatRef, Destination, GroupId, GroupMasterKey, GroupRef, GroupUpdate,
    IncomingMessage, MessageBody, MessageChange, MessageRef, Quote, ReceiptKind, SentMessage,
    ServiceId, Timestamp, Unsupported,
};

/// What the core cares about so far; everything else is dropped.
pub(super) fn incoming(received: Received) -> Option<Incoming> {
    match received {
        Received::QueueEmpty => Some(Incoming::QueueEmpty),
        Received::Content(content) => signal(&content)
            .or_else(|| message_change(&content).map(Incoming::Change))
            .or_else(|| received_message(&content).map(Incoming::Message))
            .or_else(|| sent_message(&content).map(Incoming::Sent)),
        Received::Contacts => Some(Incoming::ContactsSynced),
        Received::DecryptionError(failure) => {
            Some(Incoming::Undecryptable(undecryptable(&failure)))
        }
        Received::RetryRequest {
            sender,
            sender_device,
            timestamp,
            session_archived,
        } => Some(Incoming::RetryRequested {
            sender: ServiceId::new(sender.raw_uuid()),
            device: sender_device.into(),
            sent_at: Timestamp::from_millis(timestamp),
            session_reset: session_archived,
        }),
    }
}

/// A message that failed to decrypt.
fn undecryptable(failure: &DecryptionFailure) -> Undecryptable {
    let group = failure
        .group_id
        .as_ref()
        .and_then(|id| <[u8; 32]>::try_from(id.as_slice()).ok())
        .map(GroupId::new);
    Undecryptable {
        sender: ServiceId::new(failure.sender.raw_uuid()),
        device: failure.sender_device.into(),
        sent_at: Timestamp::from_millis(failure.timestamp),
        group,
        shown: failure.content_hint != ContentHint::Implicit,
        retry: RetryRequest::new(
            failure.retry_request.serialized().to_vec(),
            failure.group_id.clone(),
        ),
    }
}

/// Receipts, typing, and reads and settings synced from other devices.
fn signal(content: &Content) -> Option<Incoming> {
    let sender = ServiceId::new(content.metadata.sender.raw_uuid());
    match &content.body {
        ContentBody::ReceiptMessage(ReceiptMessage { r#type, timestamp }) => {
            let kind = match receipt_message::Type::try_from(r#type.unwrap_or_default()) {
                Ok(receipt_message::Type::Delivery) => ReceiptKind::Delivered,
                Ok(receipt_message::Type::Read | receipt_message::Type::Viewed) => {
                    ReceiptKind::Read
                }
                Err(_) => return None,
            };
            Some(Incoming::Receipt {
                sender,
                kind,
                timestamps: timestamp
                    .iter()
                    .copied()
                    .map(Timestamp::from_millis)
                    .collect(),
            })
        }
        ContentBody::TypingMessage(TypingMessage {
            action, group_id, ..
        }) => {
            let group = match group_id {
                Some(id) => Some(GroupId::new(<[u8; 32]>::try_from(id.as_slice()).ok()?)),
                None => None,
            };
            let typing = *action == Some(typing_message::Action::Started.into());
            Some(Incoming::Typing {
                sender,
                group,
                typing,
            })
        }
        ContentBody::SynchronizeMessage(sync) if !sync.read.is_empty() => {
            let read = sync
                .read
                .iter()
                .filter_map(|read| {
                    let author = service_id(
                        read.sender_aci_binary.as_deref(),
                        read.sender_aci.as_deref(),
                    )?;
                    Some((author, Timestamp::from_millis(read.timestamp?)))
                })
                .collect();
            Some(Incoming::ReadElsewhere {
                read,
                at: Timestamp::from_millis(client_timestamp(content)),
            })
        }
        ContentBody::SynchronizeMessage(SyncMessage {
            content: Some(SyncContent::DeleteForMe(delete)),
            ..
        }) => {
            let messages = delete
                .message_deletes
                .iter()
                .filter_map(|deletes| {
                    let chat = chat_ref(deletes.conversation.as_ref()?)?;
                    Some(deletes.messages.iter().filter_map(move |message| {
                        Some(MessageRef {
                            chat,
                            author: Some(message_author(message)?),
                            sent_at: Timestamp::from_millis(message.sent_timestamp?),
                        })
                    }))
                })
                .flatten()
                .collect();
            let whole = delete
                .conversation_deletes
                .iter()
                .map(|delete| delete.conversation.as_ref());
            let local = delete
                .local_only_conversation_deletes
                .iter()
                .map(|delete| delete.conversation.as_ref());
            let conversations = whole.chain(local).flatten().filter_map(chat_ref).collect();
            Some(Incoming::DeletedForMe {
                messages,
                conversations,
            })
        }
        ContentBody::SynchronizeMessage(SyncMessage {
            content: Some(SyncContent::Configuration(configuration)),
            ..
        }) => Some(Incoming::Settings {
            read_receipts: configuration.read_receipts,
            typing_indicators: configuration.typing_indicators,
        }),
        _ => None,
    }
}

/// A conversation as another device names it; `None` for phone numbers
/// alone, which conversations aren't keyed by.
fn chat_ref(conversation: &ConversationIdentifier) -> Option<ChatRef> {
    match conversation.identifier.as_ref()? {
        conversation_identifier::Identifier::ThreadServiceId(id) => {
            let id = SignalServiceId::parse_from_service_id_string(id)?;
            Some(ChatRef::Contact(ServiceId::new(id.raw_uuid())))
        }
        conversation_identifier::Identifier::ThreadServiceIdBinary(id) => {
            let id = SignalServiceId::parse_from_service_id_binary(id)?;
            Some(ChatRef::Contact(ServiceId::new(id.raw_uuid())))
        }
        conversation_identifier::Identifier::ThreadGroupId(id) => Some(ChatRef::Group(
            GroupId::new(<[u8; 32]>::try_from(id.as_slice()).ok()?),
        )),
        conversation_identifier::Identifier::ThreadE164(_) => None,
    }
}

fn message_author(message: &AddressableMessage) -> Option<ServiceId> {
    let id = match message.author.as_ref()? {
        addressable_message::Author::AuthorServiceId(id) => {
            SignalServiceId::parse_from_service_id_string(id)?
        }
        addressable_message::Author::AuthorServiceIdBinary(id) => {
            SignalServiceId::parse_from_service_id_binary(id)?
        }
        addressable_message::Author::AuthorE164(_) => return None,
    };
    Some(ServiceId::new(id.raw_uuid()))
}

/// A message from another account, 1:1 or in a group, or a call to it.
fn received_message(content: &Content) -> Option<IncomingMessage> {
    let (group, timestamp, message, expire_timer) = match &content.body {
        ContentBody::DataMessage(message) => {
            let group = match &message.group_v2 {
                Some(context) => Some(group_ref(context)?),
                None => None,
            };
            let timestamp = message
                .timestamp
                .unwrap_or_else(|| client_timestamp(content));
            let content = data_content(message, group)?;
            (group, timestamp, content, expire_timer(message))
        }
        ContentBody::CallMessage(CallMessage {
            offer: Some(offer), ..
        }) => {
            let video = offer.r#type == Some(offer::Type::OfferVideoCall as i32);
            let call = model::Content::Call { video };
            (None, client_timestamp(content), call, None)
        }
        _ => return None,
    };
    Some(IncomingMessage {
        sender: ServiceId::new(content.metadata.sender.raw_uuid()),
        group,
        timestamp: Timestamp::from_millis(timestamp),
        content: message,
        expire_timer,
    })
}

/// A message this account sent from another device, 1:1 or in a group.
fn sent_message(content: &Content) -> Option<SentMessage> {
    let sent = sync_sent(content)?;
    let message = sent.message.as_ref()?;
    let destination = sent_destination(sent, message)?;
    Some(SentMessage {
        destination,
        timestamp: Timestamp::from_millis(sent.timestamp?),
        content: data_content(message, group_of(destination))?,
        expire_timer: expire_timer(message),
    })
}

/// The message's disappearing timer in seconds; `None` if off.
fn expire_timer(message: &DataMessage) -> Option<u32> {
    message.expire_timer.filter(|seconds| *seconds > 0)
}

/// A sync copy of a message sent from another of this account's devices.
fn sync_sent(content: &Content) -> Option<&Sent> {
    match &content.body {
        ContentBody::SynchronizeMessage(SyncMessage {
            content: Some(SyncContent::Sent(sent)),
            ..
        }) => Some(sent),
        _ => None,
    }
}

/// Where a message sent from another device went.
fn sent_destination(sent: &Sent, message: &DataMessage) -> Option<Destination> {
    if let Some(context) = &message.group_v2 {
        return Some(Destination::Group(group_ref(context)?));
    }
    let recipient = service_id(
        sent.destination_service_id_binary.as_deref(),
        sent.destination_service_id.as_deref(),
    )?;
    Some(Destination::Contact(recipient))
}

fn group_of(destination: Destination) -> Option<GroupRef> {
    match destination {
        Destination::Group(group) => Some(group),
        Destination::Contact(_) => None,
    }
}

/// An edit or delete of an earlier message, from its author or from
/// another of this account's devices.
fn message_change(content: &Content) -> Option<MessageChange> {
    let sender = ServiceId::new(content.metadata.sender.raw_uuid());
    let in_conversation = |message: &DataMessage| -> Option<Destination> {
        match &message.group_v2 {
            Some(context) => Some(Destination::Group(group_ref(context)?)),
            None => Some(Destination::Contact(sender)),
        }
    };
    let (destination, author, target, change) = match &content.body {
        ContentBody::DataMessage(
            message @ DataMessage {
                delete: Some(delete),
                ..
            },
        ) => (
            in_conversation(message)?,
            Some(sender),
            delete.target_sent_timestamp?,
            Change::Delete,
        ),
        ContentBody::EditMessage(EditMessage {
            target_sent_timestamp: Some(target),
            data_message: Some(message),
        }) => {
            let destination = in_conversation(message)?;
            let edit = Change::Edit {
                content: data_content(message, group_of(destination))?,
                at: Timestamp::from_millis(
                    message
                        .timestamp
                        .unwrap_or_else(|| client_timestamp(content)),
                ),
            };
            (destination, Some(sender), *target, edit)
        }
        ContentBody::SynchronizeMessage(_) => {
            let sent = sync_sent(content)?;
            if let Some(EditMessage {
                target_sent_timestamp: Some(target),
                data_message: Some(message),
            }) = &sent.edit_message
            {
                let destination = sent_destination(sent, message)?;
                let edit = Change::Edit {
                    content: data_content(message, group_of(destination))?,
                    at: Timestamp::from_millis(message.timestamp.or(sent.timestamp)?),
                };
                (destination, None, *target, edit)
            } else {
                let message = sent.message.as_ref()?;
                let target = message.delete.as_ref()?.target_sent_timestamp?;
                (
                    sent_destination(sent, message)?,
                    None,
                    target,
                    Change::Delete,
                )
            }
        }
        _ => return None,
    };
    Some(MessageChange {
        destination,
        author,
        target: Timestamp::from_millis(target),
        change,
    })
}

/// What a data message shows; `None` for ones that show nothing, such as
/// receipts of edits, deletes and votes, which later issues handle.
fn data_content(message: &DataMessage, group: Option<GroupRef>) -> Option<model::Content> {
    if let (Some(group), Some(change)) = (
        group,
        message
            .group_v2
            .as_ref()
            .and_then(|context| context.group_change.as_deref()),
    ) {
        let updates = group_updates(group.key, change)?;
        return (!updates.is_empty()).then_some(model::Content::GroupChanged(updates));
    }
    let flags = message.flags.unwrap_or(0);
    if flags & data_message::Flags::ExpirationTimerUpdate as u32 != 0 {
        let seconds = message.expire_timer.filter(|seconds| *seconds > 0);
        return Some(model::Content::TimerChanged(seconds));
    }
    if let Some(reaction) = &message.reaction {
        return match (&reaction.emoji, reaction.remove) {
            (Some(emoji), Some(false) | None) => Some(model::Content::Reaction {
                emoji: emoji.clone(),
            }),
            _ => None,
        };
    }
    if let Some(sticker) = &message.sticker {
        return Some(model::Content::Sticker {
            emoji: sticker.emoji.clone(),
        });
    }
    if message.is_view_once == Some(true) {
        return Some(model::Content::Unsupported(Unsupported::ViewOnce));
    }
    let body = message.body.clone().unwrap_or_default();
    let attachments: Vec<_> = message.attachments.iter().map(attachment).collect();
    if !body.is_empty() || !attachments.is_empty() {
        return Some(model::Content::Text {
            body: MessageBody::new(body),
            attachments,
            quote: message.quote.as_ref().and_then(quote),
        });
    }
    let unsupported = if !message.contact.is_empty() {
        Unsupported::Contact
    } else if message.payment.is_some() {
        Unsupported::Payment
    } else if message.poll_create.is_some() {
        Unsupported::Poll
    } else if message.gift_badge.is_some() {
        Unsupported::GiftBadge
    } else {
        return None;
    };
    Some(model::Content::Unsupported(unsupported))
}

fn attachment(pointer: &AttachmentPointer) -> Attachment {
    attachment_kind(
        pointer.content_type.as_deref().unwrap_or(""),
        pointer.flags.unwrap_or(0),
        pointer.file_name.as_deref(),
    )
}

fn attachment_kind(kind: &str, flags: u32, file_name: Option<&str>) -> Attachment {
    let has = |flag: attachment_pointer::Flags| flags & flag as u32 != 0;
    if has(attachment_pointer::Flags::Gif) || kind == "image/gif" {
        Attachment::Gif
    } else if kind.starts_with("image/") {
        Attachment::Image
    } else if kind.starts_with("video/") {
        Attachment::Video
    } else if kind.starts_with("audio/") && has(attachment_pointer::Flags::VoiceMessage) {
        Attachment::Voice
    } else if kind.starts_with("audio/") {
        Attachment::Audio
    } else {
        Attachment::File {
            name: file_name.map(Into::into),
        }
    }
}

/// The message a reply quotes; `None` without its author or time.
pub(super) fn quote(quote: &data_message::Quote) -> Option<Quote> {
    let author = service_id(
        quote.author_aci_binary.as_deref(),
        quote.author_aci.as_deref(),
    )?;
    let attachment = quote.attachments.first().map(|attachment| {
        attachment_kind(
            attachment.content_type.as_deref().unwrap_or(""),
            attachment
                .thumbnail
                .as_ref()
                .and_then(|t| t.flags)
                .unwrap_or(0),
            attachment.file_name.as_deref(),
        )
    });
    Some(Quote {
        author: Some(author),
        sent_at: Timestamp::from_millis(quote.id?),
        text: quote.text.clone().unwrap_or_default(),
        attachment,
    })
}

/// The changes in an encrypted group change; `None` if it can't be read.
fn group_updates(key: GroupMasterKey, change: &[u8]) -> Option<Vec<GroupUpdate>> {
    let change = proto::GroupChange::decode(change).ok()?;
    let params = GroupSecretParams::derive_from_master_key(
        presage::libsignal_service::prelude::GroupMasterKey::new(*key.as_bytes()),
    );
    let changes = match GroupOperations::new(params).decrypt_group_change(change) {
        Ok(changes) => changes,
        Err(error) => {
            tracing::warn!(?error, "a group change could not be decrypted");
            return None;
        }
    };
    let editor = changes.editor;
    let mut updates: Vec<_> = changes
        .changes
        .into_iter()
        .filter_map(|change| group_update(change, editor))
        .collect();
    updates.dedup();
    Some(updates)
}

/// How a change reads in the log; `None` for ones not worth a line.
fn group_update(change: GroupChange, editor: Aci) -> Option<GroupUpdate> {
    let id = |aci: Aci| ServiceId::new(aci.into());
    let editor_id = SignalServiceId::from(editor);
    Some(match change {
        GroupChange::NewMember(member) if member.aci == editor => GroupUpdate::Joined,
        GroupChange::NewMember(member) => GroupUpdate::Added(id(member.aci)),
        GroupChange::DeleteMember(aci) if aci == editor => GroupUpdate::Left,
        GroupChange::DeleteMember(aci) => GroupUpdate::Removed(id(aci)),
        GroupChange::ModifyMemberRole { aci, role } => GroupUpdate::Admin {
            member: id(aci),
            admin: role == Role::Administrator,
        },
        GroupChange::NewPendingMember(pending) => {
            GroupUpdate::Invited(ServiceId::new(pending.address.raw_uuid()))
        }
        GroupChange::PromotePendingMember { address, .. } if address == editor_id => {
            GroupUpdate::Joined
        }
        GroupChange::PromotePendingMember { address, .. } => {
            GroupUpdate::Added(ServiceId::new(address.raw_uuid()))
        }
        GroupChange::NewRequestingMember(_) => GroupUpdate::Requested,
        GroupChange::PromoteRequestingMember { aci, .. } => GroupUpdate::Added(id(aci)),
        GroupChange::Title(title) => GroupUpdate::Title(title),
        GroupChange::Description(_) => GroupUpdate::Description,
        GroupChange::Avatar(_) => GroupUpdate::Avatar,
        GroupChange::Timer(timer) => GroupUpdate::Timer(
            timer
                .map(|timer| timer.duration)
                .filter(|seconds| *seconds > 0),
        ),
        GroupChange::AttributeAccess(_)
        | GroupChange::MemberAccess(_)
        | GroupChange::InviteLinkAccess(_)
        | GroupChange::InviteLinkPassword(_)
        | GroupChange::AnnouncementOnly(_) => GroupUpdate::Settings,
        _ => return None,
    })
}

/// The group a message belongs to; `None` if its master key is malformed.
fn group_ref(context: &GroupContextV2) -> Option<GroupRef> {
    let key = <[u8; 32]>::try_from(context.master_key.as_deref()?).ok()?;
    Some(GroupRef {
        key: GroupMasterKey::new(key),
        revision: context.revision.unwrap_or(0),
    })
}

/// A service ID from its binary field, else its string field.
fn service_id(binary: Option<&[u8]>, string: Option<&str>) -> Option<ServiceId> {
    let id = binary
        .and_then(SignalServiceId::parse_from_service_id_binary)
        .or_else(|| string.and_then(SignalServiceId::parse_from_service_id_string))?;
    Some(ServiceId::new(id.raw_uuid()))
}

/// When the sender's device says it sent the message.
fn client_timestamp(content: &Content) -> u64 {
    content.metadata.client_timestamp.timestamp_millis().max(0) as u64
}

#[cfg(test)]
pub(super) mod tests {
    use chrono::{TimeZone as _, Utc};
    use presage::libsignal_service::groups_v2::Timer;
    use presage::libsignal_service::proto::{call_message, group_change, sync_message};
    use presage::libsignal_service::protocol::DeviceId;
    use uuid::Uuid;

    use super::*;

    pub(in crate::signal) const SENDER: Uuid = Uuid::from_u128(0x1234);
    pub(in crate::signal) const OTHER: Uuid = Uuid::from_u128(0x4321);
    const KEY: [u8; 32] = [9; 32];

    pub(in crate::signal) fn content(body: ContentBody) -> Content {
        let at = Utc.timestamp_millis_opt(1_700_000_000_123).unwrap();
        Content {
            metadata: presage::libsignal_service::content::Metadata {
                sender: Aci::from(SENDER).into(),
                destination: Aci::from(Uuid::from_u128(0x99)).into(),
                sender_device: DeviceId::try_from(1u32).unwrap(),
                pni_verified: None,
                client_timestamp: at,
                server_timestamp: at,
                needs_receipt: false,
                unidentified_sender: true,
                was_plaintext: false,
                server_guid: None,
            },
            body,
        }
    }

    fn data(message: DataMessage) -> Option<model::Content> {
        received_message(&content(ContentBody::DataMessage(message))).map(|m| m.content)
    }

    pub(in crate::signal) fn text(body: &str, timestamp: Option<u64>) -> ContentBody {
        ContentBody::DataMessage(DataMessage {
            body: Some(body.into()),
            timestamp,
            ..Default::default()
        })
    }

    fn pointer(content_type: &str, flags: u32, name: Option<&str>) -> AttachmentPointer {
        AttachmentPointer {
            content_type: Some(content_type.into()),
            flags: Some(flags),
            file_name: name.map(Into::into),
            ..Default::default()
        }
    }

    #[test]
    fn maps_one_to_one_text() {
        let received = Received::Content(Box::new(content(text("hi", Some(42)))));

        assert_eq!(
            incoming(received),
            Some(Incoming::Message(IncomingMessage {
                sender: ServiceId::new(SENDER),
                group: None,
                timestamp: Timestamp::from_millis(42),
                content: model::Content::text("hi"),
                expire_timer: None,
            }))
        );
    }

    #[test]
    fn messages_carry_their_disappearing_timer() {
        let timed = |seconds| {
            let body = ContentBody::DataMessage(DataMessage {
                body: Some("hi".into()),
                expire_timer: Some(seconds),
                ..Default::default()
            });
            received_message(&content(body)).unwrap().expire_timer
        };
        assert_eq!(timed(3600), Some(3600));
        assert_eq!(timed(0), None);

        let body = sent(Sent {
            destination_service_id: Some(RECIPIENT.to_string()),
            timestamp: Some(77),
            message: Some(DataMessage {
                body: Some("from phone".into()),
                expire_timer: Some(60),
                ..Default::default()
            }),
            ..Default::default()
        });
        assert_eq!(sent_message(&content(body)).unwrap().expire_timer, Some(60));
    }

    #[test]
    fn falls_back_to_client_timestamp() {
        let message = received_message(&content(text("hi", None))).unwrap();
        assert_eq!(message.timestamp, Timestamp::from_millis(1_700_000_000_123));
    }

    #[test]
    fn attachments_keep_their_caption() {
        let message = DataMessage {
            body: Some("the graph".into()),
            attachments: vec![
                pointer("image/png", 0, None),
                pointer("image/gif", 0, None),
                pointer("video/mp4", 8, None),
                pointer("video/mp4", 0, None),
                pointer("audio/aac", 1, None),
                pointer("audio/mpeg", 0, None),
                pointer("application/pdf", 0, Some("report.pdf")),
            ],
            ..Default::default()
        };

        assert_eq!(
            data(message),
            Some(model::Content::Text {
                body: MessageBody::new("the graph"),
                attachments: vec![
                    Attachment::Image,
                    Attachment::Gif,
                    Attachment::Gif,
                    Attachment::Video,
                    Attachment::Voice,
                    Attachment::Audio,
                    Attachment::File {
                        name: Some("report.pdf".into())
                    },
                ],
                quote: None,
            })
        );
    }

    #[test]
    fn attachment_without_text() {
        let message = DataMessage {
            attachments: vec![pointer("image/jpeg", 0, None)],
            ..Default::default()
        };
        assert_eq!(
            data(message),
            Some(model::Content::Text {
                body: MessageBody::new(""),
                attachments: vec![Attachment::Image],
                quote: None,
            })
        );
    }

    #[test]
    fn stickers_reactions_and_unsupported_content() {
        let sticker = DataMessage {
            sticker: Some(data_message::Sticker {
                emoji: Some("🐱".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            data(sticker),
            Some(model::Content::Sticker {
                emoji: Some("🐱".into())
            })
        );

        let reaction = |remove| DataMessage {
            reaction: Some(data_message::Reaction {
                emoji: Some("👍".into()),
                remove: Some(remove),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            data(reaction(false)),
            Some(model::Content::Reaction {
                emoji: "👍".into()
            })
        );
        assert_eq!(data(reaction(true)), None);

        let view_once = DataMessage {
            is_view_once: Some(true),
            attachments: vec![pointer("image/jpeg", 0, None)],
            ..Default::default()
        };
        let contact = DataMessage {
            contact: vec![data_message::Contact::default()],
            ..Default::default()
        };
        let payment = DataMessage {
            payment: Some(data_message::Payment::default()),
            ..Default::default()
        };
        let poll = DataMessage {
            poll_create: Some(data_message::PollCreate::default()),
            ..Default::default()
        };
        let gift = DataMessage {
            gift_badge: Some(data_message::GiftBadge::default()),
            ..Default::default()
        };
        for (message, kind) in [
            (view_once, Unsupported::ViewOnce),
            (contact, Unsupported::Contact),
            (payment, Unsupported::Payment),
            (poll, Unsupported::Poll),
            (gift, Unsupported::GiftBadge),
        ] {
            assert_eq!(data(message), Some(model::Content::Unsupported(kind)));
        }
    }

    #[test]
    fn timer_updates() {
        let update = |seconds| DataMessage {
            flags: Some(data_message::Flags::ExpirationTimerUpdate as u32),
            expire_timer: Some(seconds),
            ..Default::default()
        };
        assert_eq!(
            data(update(604_800)),
            Some(model::Content::TimerChanged(Some(604_800)))
        );
        assert_eq!(data(update(0)), Some(model::Content::TimerChanged(None)));
    }

    #[test]
    fn call_offers() {
        let call = |kind: offer::Type| {
            let body = ContentBody::CallMessage(CallMessage {
                offer: Some(call_message::Offer {
                    r#type: Some(kind as i32),
                    ..Default::default()
                }),
                ..Default::default()
            });
            received_message(&content(body)).map(|message| message.content)
        };
        assert_eq!(
            call(offer::Type::OfferAudioCall),
            Some(model::Content::Call { video: false })
        );
        assert_eq!(
            call(offer::Type::OfferVideoCall),
            Some(model::Content::Call { video: true })
        );
        let hangup = ContentBody::CallMessage(CallMessage {
            hangup: Some(call_message::Hangup::default()),
            ..Default::default()
        });
        assert_eq!(received_message(&content(hangup)), None);
    }

    #[test]
    fn skips_messages_that_show_nothing() {
        let empty = ContentBody::DataMessage(DataMessage::default());
        assert_eq!(received_message(&content(empty)), None);
        let delete = DataMessage {
            delete: Some(data_message::Delete::default()),
            ..Default::default()
        };
        assert_eq!(data(delete), None);
        let typing = ContentBody::TypingMessage(TypingMessage::default());
        assert_eq!(received_message(&content(typing)), None);
    }

    fn group_context(key: Option<Vec<u8>>) -> Option<GroupContextV2> {
        Some(GroupContextV2 {
            master_key: key,
            revision: Some(3),
            ..Default::default()
        })
    }

    #[test]
    fn maps_group_text() {
        let body = ContentBody::DataMessage(DataMessage {
            body: Some("hi all".into()),
            group_v2: group_context(Some(KEY.to_vec())),
            ..Default::default()
        });

        let message = received_message(&content(body)).unwrap();

        assert_eq!(message.sender, ServiceId::new(SENDER));
        assert_eq!(
            message.group,
            Some(GroupRef {
                key: GroupMasterKey::new(KEY),
                revision: 3,
            })
        );
    }

    #[test]
    fn skips_group_messages_with_a_bad_master_key() {
        for key in [None, Some(vec![9; 31])] {
            let body = ContentBody::DataMessage(DataMessage {
                body: Some("hi all".into()),
                group_v2: group_context(key),
                ..Default::default()
            });
            assert_eq!(received_message(&content(body)), None);
        }
    }

    /// An encrypted group change by `editor` renaming the group and
    /// removing [`OTHER`].
    fn encrypted_change(editor: Uuid, title: &str) -> Vec<u8> {
        let params = GroupSecretParams::derive_from_master_key(
            presage::libsignal_service::prelude::GroupMasterKey::new(KEY),
        );
        let encrypt = |uuid: Uuid| {
            let id = SignalServiceId::from(Aci::from(uuid));
            presage::libsignal_service::zkgroup::serialize(&params.encrypt_service_id(id))
        };
        let actions = group_change::Actions {
            group_id: params.get_group_identifier().to_vec(),
            source_user_id: encrypt(editor),
            version: 4,
            modify_title: Some(group_change::actions::ModifyTitleAction {
                title: GroupOperations::new(params).encrypt_title(title, &mut rand::rng()),
            }),
            delete_members: vec![group_change::actions::DeleteMemberAction {
                deleted_user_id: encrypt(OTHER),
            }],
            ..Default::default()
        };
        proto::GroupChange {
            actions: actions.encode_to_vec(),
            ..Default::default()
        }
        .encode_to_vec()
    }

    #[test]
    fn group_changes_become_updates() {
        let message = DataMessage {
            group_v2: Some(GroupContextV2 {
                master_key: Some(KEY.to_vec()),
                revision: Some(4),
                group_change: Some(encrypted_change(SENDER, "Dev team")),
            }),
            ..Default::default()
        };

        assert_eq!(
            data(message),
            Some(model::Content::GroupChanged(vec![
                GroupUpdate::Removed(ServiceId::new(OTHER)),
                GroupUpdate::Title("Dev team".into()),
            ]))
        );
    }

    #[test]
    fn unreadable_group_changes_are_skipped() {
        let message = DataMessage {
            group_v2: Some(GroupContextV2 {
                master_key: Some(KEY.to_vec()),
                revision: Some(4),
                group_change: Some(vec![1, 2, 3]),
            }),
            ..Default::default()
        };
        assert_eq!(data(message), None);
    }

    #[test]
    fn group_update_mapping() {
        let editor = Aci::from(SENDER);
        let other = Aci::from(OTHER);
        let member = |aci| presage::libsignal_service::groups_v2::Member {
            aci,
            role: Role::Default,
            profile_key: presage::libsignal_service::prelude::ProfileKey::create([0; 32]),
            joined_at_version: 0,
            label: None,
            label_emoji: None,
        };
        let cases = [
            (
                GroupChange::NewMember(member(editor)),
                Some(GroupUpdate::Joined),
            ),
            (
                GroupChange::NewMember(member(other)),
                Some(GroupUpdate::Added(ServiceId::new(OTHER))),
            ),
            (GroupChange::DeleteMember(editor), Some(GroupUpdate::Left)),
            (
                GroupChange::ModifyMemberRole {
                    aci: other,
                    role: Role::Administrator,
                },
                Some(GroupUpdate::Admin {
                    member: ServiceId::new(OTHER),
                    admin: true,
                }),
            ),
            (
                GroupChange::Timer(Some(Timer { duration: 3600 })),
                Some(GroupUpdate::Timer(Some(3600))),
            ),
            (GroupChange::Timer(None), Some(GroupUpdate::Timer(None))),
            (
                GroupChange::Description(None),
                Some(GroupUpdate::Description),
            ),
            (
                GroupChange::AnnouncementOnly(true),
                Some(GroupUpdate::Settings),
            ),
            (
                GroupChange::ModifyMemberProfileKey {
                    aci: other,
                    profile_key: presage::libsignal_service::prelude::ProfileKey::create([0; 32]),
                },
                None,
            ),
        ];
        for (change, expected) in cases {
            assert_eq!(group_update(change, editor), expected);
        }
    }

    fn received(body: ContentBody) -> Option<Incoming> {
        incoming(Received::Content(Box::new(content(body))))
    }

    #[test]
    fn maps_receipts() {
        let receipt = |kind: receipt_message::Type| {
            received(ContentBody::ReceiptMessage(ReceiptMessage {
                r#type: Some(kind.into()),
                timestamp: vec![10, 11],
            }))
        };
        let expected = |kind| {
            Some(Incoming::Receipt {
                sender: ServiceId::new(SENDER),
                kind,
                timestamps: vec![Timestamp::from_millis(10), Timestamp::from_millis(11)],
            })
        };
        assert_eq!(
            receipt(receipt_message::Type::Delivery),
            expected(ReceiptKind::Delivered)
        );
        assert_eq!(
            receipt(receipt_message::Type::Read),
            expected(ReceiptKind::Read)
        );
        assert_eq!(
            receipt(receipt_message::Type::Viewed),
            expected(ReceiptKind::Read)
        );
    }

    #[test]
    fn maps_typing() {
        let typing = |action: typing_message::Action, group_id: Option<Vec<u8>>| {
            received(ContentBody::TypingMessage(TypingMessage {
                timestamp: Some(1),
                action: Some(action.into()),
                group_id,
            }))
        };
        assert_eq!(
            typing(typing_message::Action::Started, None),
            Some(Incoming::Typing {
                sender: ServiceId::new(SENDER),
                group: None,
                typing: true,
            })
        );
        assert_eq!(
            typing(typing_message::Action::Stopped, Some(vec![4; 32])),
            Some(Incoming::Typing {
                sender: ServiceId::new(SENDER),
                group: Some(GroupId::new([4; 32])),
                typing: false,
            })
        );
        assert_eq!(
            typing(typing_message::Action::Started, Some(vec![4; 3])),
            None
        );
    }

    #[test]
    fn maps_reads_and_settings_from_other_devices() {
        let read = received(ContentBody::SynchronizeMessage(SyncMessage {
            read: vec![
                sync_message::Read {
                    sender_aci: Some(OTHER.to_string()),
                    timestamp: Some(5),
                    ..Default::default()
                },
                sync_message::Read {
                    sender_aci_binary: Some(SENDER.as_bytes().to_vec()),
                    timestamp: Some(6),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }));
        assert_eq!(
            read,
            Some(Incoming::ReadElsewhere {
                read: vec![
                    (ServiceId::new(OTHER), Timestamp::from_millis(5)),
                    (ServiceId::new(SENDER), Timestamp::from_millis(6)),
                ],
                at: Timestamp::from_millis(1_700_000_000_123),
            })
        );

        let settings = received(ContentBody::SynchronizeMessage(SyncMessage {
            content: Some(SyncContent::Configuration(sync_message::Configuration {
                read_receipts: Some(true),
                typing_indicators: Some(false),
                ..Default::default()
            })),
            ..Default::default()
        }));
        assert_eq!(
            settings,
            Some(Incoming::Settings {
                read_receipts: Some(true),
                typing_indicators: Some(false),
            })
        );
    }

    #[test]
    fn replies_carry_their_quote() {
        let message = DataMessage {
            body: Some("agreed".into()),
            quote: Some(data_message::Quote {
                id: Some(50),
                author_aci: Some(OTHER.to_string()),
                text: Some("ship it?".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let attachment_only = data_message::Quote {
            id: Some(51),
            author_aci_binary: Some(OTHER.as_bytes().to_vec()),
            attachments: vec![data_message::quote::QuotedAttachment {
                content_type: Some("image/jpeg".into()),
                ..Default::default()
            }],
            ..Default::default()
        };

        assert_eq!(
            data(message),
            Some(model::Content::Text {
                body: MessageBody::new("agreed"),
                attachments: Vec::new(),
                quote: Some(Quote {
                    author: Some(ServiceId::new(OTHER)),
                    sent_at: Timestamp::from_millis(50),
                    text: "ship it?".into(),
                    attachment: None,
                }),
            })
        );
        let quoted = quote(&attachment_only).unwrap();
        assert_eq!(quoted.text, "");
        assert_eq!(quoted.attachment, Some(Attachment::Image));
        assert_eq!(quote(&data_message::Quote::default()), None);
    }

    #[test]
    fn maps_deletes_and_edits() {
        let delete = ContentBody::DataMessage(DataMessage {
            delete: Some(data_message::Delete {
                target_sent_timestamp: Some(40),
            }),
            group_v2: group_context(Some(KEY.to_vec())),
            ..Default::default()
        });
        assert_eq!(
            received(delete),
            Some(Incoming::Change(MessageChange {
                destination: Destination::Group(GroupRef {
                    key: GroupMasterKey::new(KEY),
                    revision: 3,
                }),
                author: Some(ServiceId::new(SENDER)),
                target: Timestamp::from_millis(40),
                change: Change::Delete,
            }))
        );

        let edit = ContentBody::EditMessage(EditMessage {
            target_sent_timestamp: Some(40),
            data_message: Some(DataMessage {
                body: Some("fixed".into()),
                timestamp: Some(45),
                ..Default::default()
            }),
        });
        assert_eq!(
            received(edit),
            Some(Incoming::Change(MessageChange {
                destination: Destination::Contact(ServiceId::new(SENDER)),
                author: Some(ServiceId::new(SENDER)),
                target: Timestamp::from_millis(40),
                change: Change::Edit {
                    content: model::Content::text("fixed"),
                    at: Timestamp::from_millis(45),
                },
            }))
        );
    }

    #[test]
    fn maps_edits_and_deletes_from_the_phone() {
        let recipient: SignalServiceId = Aci::from(RECIPIENT).into();
        let edit = sent(Sent {
            destination_service_id_binary: Some(recipient.service_id_binary()),
            timestamp: Some(90),
            edit_message: Some(EditMessage {
                target_sent_timestamp: Some(80),
                data_message: sent_data("better", false),
            }),
            ..Default::default()
        });
        assert_eq!(
            received(edit),
            Some(Incoming::Change(MessageChange {
                destination: Destination::Contact(ServiceId::new(RECIPIENT)),
                author: None,
                target: Timestamp::from_millis(80),
                change: Change::Edit {
                    content: model::Content::text("better"),
                    at: Timestamp::from_millis(90),
                },
            }))
        );

        let delete = sent(Sent {
            timestamp: Some(91),
            message: Some(DataMessage {
                delete: Some(data_message::Delete {
                    target_sent_timestamp: Some(80),
                }),
                group_v2: group_context(Some(KEY.to_vec())),
                ..Default::default()
            }),
            ..Default::default()
        });
        let Some(Incoming::Change(change)) = received(delete) else {
            panic!("expected a change");
        };
        assert_eq!(change.author, None);
        assert_eq!(change.change, Change::Delete);
        assert!(matches!(change.destination, Destination::Group(_)));
    }

    #[test]
    fn maps_deletes_for_me() {
        use presage::libsignal_service::proto::sync_message::{DeleteForMe, delete_for_me};

        let contact = ConversationIdentifier {
            identifier: Some(conversation_identifier::Identifier::ThreadServiceId(
                OTHER.to_string(),
            )),
        };
        let group = ConversationIdentifier {
            identifier: Some(conversation_identifier::Identifier::ThreadGroupId(vec![
                4;
                32
            ])),
        };
        let phone_only = ConversationIdentifier {
            identifier: Some(conversation_identifier::Identifier::ThreadE164(
                "+15550100".into(),
            )),
        };
        let message = |author: Uuid, sent_at| AddressableMessage {
            sent_timestamp: Some(sent_at),
            author: Some(addressable_message::Author::AuthorServiceIdBinary(
                SignalServiceId::from(Aci::from(author)).service_id_binary(),
            )),
        };
        let delete = DeleteForMe {
            message_deletes: vec![
                delete_for_me::MessageDeletes {
                    conversation: Some(contact.clone()),
                    messages: vec![message(OTHER, 10), message(SENDER, 11)],
                },
                delete_for_me::MessageDeletes {
                    conversation: Some(phone_only.clone()),
                    messages: vec![message(OTHER, 12)],
                },
            ],
            conversation_deletes: vec![delete_for_me::ConversationDelete {
                conversation: Some(group),
                ..Default::default()
            }],
            local_only_conversation_deletes: vec![delete_for_me::LocalOnlyConversationDelete {
                conversation: Some(phone_only),
            }],
            ..Default::default()
        };

        let chat = ChatRef::Contact(ServiceId::new(OTHER));
        assert_eq!(
            received(ContentBody::SynchronizeMessage(SyncMessage {
                content: Some(SyncContent::DeleteForMe(delete)),
                ..Default::default()
            })),
            Some(Incoming::DeletedForMe {
                messages: vec![
                    MessageRef {
                        chat,
                        author: Some(ServiceId::new(OTHER)),
                        sent_at: Timestamp::from_millis(10),
                    },
                    MessageRef {
                        chat,
                        author: Some(ServiceId::new(SENDER)),
                        sent_at: Timestamp::from_millis(11),
                    },
                ],
                conversations: vec![ChatRef::Group(GroupId::new([4; 32]))],
            })
        );
    }

    #[test]
    fn maps_queue_empty_and_contacts() {
        assert_eq!(incoming(Received::QueueEmpty), Some(Incoming::QueueEmpty));
        assert_eq!(incoming(Received::Contacts), Some(Incoming::ContactsSynced));
    }

    fn failure(content_hint: ContentHint, group_id: Option<Vec<u8>>) -> Received {
        use presage::libsignal_service::protocol::{
            CiphertextMessageType, DecryptionErrorMessage, DeviceId, SignalProtocolError,
        };

        let retry_request = DecryptionErrorMessage::for_original(
            &[],
            CiphertextMessageType::SenderKey,
            presage::libsignal_service::protocol::Timestamp::from_epoch_millis(77),
            2,
        )
        .unwrap();
        Received::DecryptionError(Box::new(DecryptionFailure {
            error: SignalProtocolError::InvalidState("decrypt", "test".into()),
            sender: Aci::from(SENDER).into(),
            sender_device: DeviceId::new(2).unwrap(),
            timestamp: 77,
            content_hint,
            group_id,
            retry_request,
        }))
    }

    #[test]
    fn maps_decryption_failures_with_their_retry_request() {
        let Some(Incoming::Undecryptable(failed)) =
            incoming(failure(ContentHint::Resendable, Some(vec![4; 32])))
        else {
            panic!("not mapped");
        };

        assert_eq!(failed.sender, ServiceId::new(SENDER));
        assert_eq!(failed.device, 2);
        assert_eq!(failed.sent_at, Timestamp::from_millis(77));
        assert_eq!(failed.group, Some(GroupId::new([4; 32])));
        assert!(failed.shown);
        assert_eq!(failed.retry.group_id(), Some(&[4; 32][..]));
        assert!(!failed.retry.message().is_empty());
    }

    #[test]
    fn decryption_failures_of_implicit_content_arent_shown() {
        let Some(Incoming::Undecryptable(failed)) =
            incoming(failure(ContentHint::Implicit, Some(vec![4; 3])))
        else {
            panic!("not mapped");
        };

        assert!(!failed.shown);
        assert_eq!(failed.group, None, "not a group ID");
    }

    #[test]
    fn maps_retry_requests() {
        use presage::libsignal_service::protocol::DeviceId;

        let request = Received::RetryRequest {
            sender: Aci::from(SENDER).into(),
            sender_device: DeviceId::new(3).unwrap(),
            timestamp: 99,
            session_archived: true,
        };

        assert_eq!(
            incoming(request),
            Some(Incoming::RetryRequested {
                sender: ServiceId::new(SENDER),
                device: 3,
                sent_at: Timestamp::from_millis(99),
                session_reset: true,
            })
        );
    }

    const RECIPIENT: Uuid = Uuid::from_u128(0x5678);

    fn sent(sent: Sent) -> ContentBody {
        ContentBody::SynchronizeMessage(SyncMessage {
            content: Some(SyncContent::Sent(sent)),
            ..Default::default()
        })
    }

    fn sent_data(body: &str, group: bool) -> Option<DataMessage> {
        Some(DataMessage {
            body: Some(body.into()),
            group_v2: if group {
                group_context(Some(KEY.to_vec()))
            } else {
                None
            },
            ..Default::default()
        })
    }

    #[test]
    fn maps_text_sent_from_the_phone() {
        let recipient: SignalServiceId = Aci::from(RECIPIENT).into();
        let body = sent(Sent {
            destination_service_id_binary: Some(recipient.service_id_binary()),
            timestamp: Some(77),
            message: sent_data("from phone", false),
            ..Default::default()
        });

        assert_eq!(
            incoming(Received::Content(Box::new(content(body)))),
            Some(Incoming::Sent(SentMessage {
                destination: Destination::Contact(ServiceId::new(RECIPIENT)),
                timestamp: Timestamp::from_millis(77),
                content: model::Content::text("from phone"),
                expire_timer: None,
            }))
        );
    }

    #[test]
    fn sent_recipient_can_be_a_string() {
        let body = sent(Sent {
            destination_service_id: Some(RECIPIENT.to_string()),
            timestamp: Some(77),
            message: sent_data("x", false),
            ..Default::default()
        });

        let sent = sent_message(&content(body)).unwrap();
        assert_eq!(
            sent.destination,
            Destination::Contact(ServiceId::new(RECIPIENT))
        );
    }

    #[test]
    fn maps_group_text_sent_from_the_phone() {
        let body = sent(Sent {
            timestamp: Some(77),
            message: sent_data("to all", true),
            ..Default::default()
        });

        let sent = sent_message(&content(body)).unwrap();

        assert_eq!(
            sent.destination,
            Destination::Group(GroupRef {
                key: GroupMasterKey::new(KEY),
                revision: 3,
            })
        );
    }

    #[test]
    fn skips_sent_messages_without_a_recipient() {
        let no_recipient = sent(Sent {
            timestamp: Some(77),
            message: sent_data("x", false),
            ..Default::default()
        });

        assert_eq!(sent_message(&content(no_recipient)), None);
    }
}
