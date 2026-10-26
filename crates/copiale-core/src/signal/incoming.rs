//! Received presage content as [`Incoming`] core events.

use presage::libsignal_service::cipher::DecryptionFailure;
use presage::libsignal_service::content::{Content, ContentBody};
use presage::libsignal_service::groups_v2::{GroupChange, GroupOperations, Role};
use presage::libsignal_service::prelude::{GroupSecretParams, ProtobufMessage as _};
use presage::libsignal_service::proto;
use presage::libsignal_service::proto::call_message::offer;
use presage::libsignal_service::proto::sync_message::{
    Content as SyncContent, Sent, fetch_latest, message_request_response,
};
use presage::libsignal_service::proto::{
    AddressableMessage, AttachmentPointer, CallMessage, ConversationIdentifier, DataMessage,
    EditMessage, GroupContextV2, ReceiptMessage, SyncMessage, TypingMessage, addressable_message,
    attachment_pointer, conversation_identifier, data_message, receipt_message, typing_message,
};
use presage::libsignal_service::protocol::{
    Aci, ContentHint, IdentityKey as SignalIdentityKey, ServiceId as SignalServiceId,
};
use presage::libsignal_service::zkgroup::{NotarySignatureBytes, ServerPublicParams};
use presage::model::messages::Received;

use crate::backend::{Incoming, RetryRequest, Undecryptable};
use crate::model::{
    self, Attachment, AttachmentKind, AttachmentState, BlockedList, Change, ChatRef, Destination,
    GroupId, GroupMasterKey, GroupRef, GroupUpdate, IdentityKey, IncomingMessage, MessageBody,
    MessageChange, MessageRef, PhoneNumber, Quote, ReceiptKind, RemoteAttachment, RequestResponse,
    SentMessage, ServiceId, Timestamp, Unsupported,
};
use crate::text::{clean_name, clip_body, clip_file_name};

/// What the core cares about so far; everything else is dropped, as are
/// sync messages from anyone but `me`. Group changes must be signed by
/// `server`.
pub(super) fn incoming(
    received: Received,
    me: Aci,
    server: &ServerPublicParams,
) -> Option<Incoming> {
    match received {
        Received::QueueEmpty => Some(Incoming::QueueEmpty),
        Received::Content(content)
            if matches!(content.body, ContentBody::SynchronizeMessage(_))
                && content.metadata.sender != me =>
        {
            tracing::warn!("dropped a sync message not sent by this account");
            None
        }
        Received::Content(content) => signal(&content)
            .or_else(|| message_change(&content).map(Incoming::Change))
            .or_else(|| received_message(&content, server).map(Incoming::Message))
            .or_else(|| sent_message(&content, server).map(Incoming::Sent)),
        Received::Contacts { complete, saved } => Some(Incoming::ContactsSynced {
            all: saved
                .filter(|_| complete)
                .map(|saved| saved.into_iter().map(ServiceId::new).collect()),
        }),
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

/// Receipts, typing, and reads, settings, verifications, message request
/// answers and blocks synced from other devices.
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
        ContentBody::SynchronizeMessage(SyncMessage {
            content: Some(SyncContent::Verified(verified)),
            ..
        }) => {
            let peer = service_id(
                verified.destination_aci_binary.as_deref(),
                verified.destination_aci.as_deref(),
            )?;
            let key = identity_key(verified.identity_key.as_deref()?)?;
            let verified = match proto::verified::State::try_from(verified.state.unwrap_or(0)) {
                Ok(proto::verified::State::Verified) => true,
                Ok(proto::verified::State::Default) => false,
                // The phone's own mark of a verified key that changed; the
                // change shows here as well.
                Ok(proto::verified::State::Unverified) | Err(_) => return None,
            };
            Some(Incoming::Verified {
                peer,
                key,
                verified,
            })
        }
        ContentBody::SynchronizeMessage(SyncMessage {
            content: Some(SyncContent::MessageRequestResponse(answer)),
            ..
        }) => {
            let chat = match &answer.group_id {
                Some(id) => ChatRef::Group(group_id(id)?),
                None => ChatRef::Contact(service_id(
                    answer.thread_aci_binary.as_deref(),
                    answer.thread_aci.as_deref(),
                )?),
            };
            use message_request_response::Type;
            let response = match Type::try_from(answer.r#type?).ok()? {
                Type::Accept => RequestResponse::Accept,
                Type::Delete => RequestResponse::Delete,
                Type::Block | Type::BlockAndSpam => RequestResponse::Block,
                Type::BlockAndDelete => RequestResponse::BlockAndDelete,
                Type::Spam | Type::Unknown => return None,
            };
            Some(Incoming::RequestResponse { chat, response })
        }
        ContentBody::SynchronizeMessage(SyncMessage {
            content: Some(SyncContent::Blocked(blocked)),
            ..
        }) => {
            let binary = blocked
                .acis_binary
                .iter()
                .filter_map(|id| service_id(Some(id), None));
            let strings = blocked
                .acis
                .iter()
                .filter_map(|id| service_id(None, Some(id)));
            let mut accounts = Vec::new();
            for account in binary.chain(strings) {
                if !accounts.contains(&account) {
                    accounts.push(account);
                }
            }
            Some(Incoming::Blocked(BlockedList {
                accounts,
                groups: blocked
                    .group_ids
                    .iter()
                    .filter_map(|id| group_id(id))
                    .collect(),
                numbers: blocked
                    .numbers
                    .iter()
                    .filter(|number| !number.is_empty())
                    .map(PhoneNumber::new)
                    .collect(),
            }))
        }
        ContentBody::SynchronizeMessage(SyncMessage {
            content: Some(SyncContent::FetchLatest(fetch)),
            ..
        }) if fetch.r#type == Some(fetch_latest::Type::StorageManifest.into()) => {
            Some(Incoming::StorageChanged)
        }
        _ => None,
    }
}

/// A group's public ID; `None` if malformed.
fn group_id(id: &[u8]) -> Option<GroupId> {
    Some(GroupId::new(<[u8; 32]>::try_from(id).ok()?))
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
        conversation_identifier::Identifier::ThreadGroupId(id) => {
            Some(ChatRef::Group(group_id(id)?))
        }
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
fn received_message(content: &Content, server: &ServerPublicParams) -> Option<IncomingMessage> {
    let (group, timestamp, message, expire_timer) = match &content.body {
        ContentBody::DataMessage(message) => {
            let group = match &message.group_v2 {
                Some(context) => Some(group_ref(context)?),
                None => None,
            };
            let timestamp = message
                .timestamp
                .unwrap_or_else(|| client_timestamp(content));
            let content = data_content(message, group, Some(server))?;
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
        server_at: server_timestamp(content),
        content: message,
        expire_timer,
    })
}

/// A message this account sent from another device, 1:1 or in a group.
fn sent_message(content: &Content, server: &ServerPublicParams) -> Option<SentMessage> {
    let sent = sync_sent(content)?;
    let message = sent.message.as_ref()?;
    let destination = sent_destination(sent, message)?;
    Some(SentMessage {
        destination,
        timestamp: Timestamp::from_millis(sent.timestamp?),
        content: data_content(message, group_of(destination), Some(server))?,
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
                content: edited_content(message)?,
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
                    content: edited_content(message)?,
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
        server_at: server_timestamp(content),
        change,
    })
}

/// What an edit changes a message to: text only, never a system line.
fn edited_content(message: &DataMessage) -> Option<model::Content> {
    match data_content(message, None, None)? {
        text @ model::Content::Text { .. } => Some(text),
        _ => None,
    }
}

/// What a data message shows; `None` for ones that show nothing, such as
/// receipts of edits, deletes and votes, which later issues handle. Group
/// changes are read only with `group` and `server`.
fn data_content(
    message: &DataMessage,
    group: Option<GroupRef>,
    server: Option<&ServerPublicParams>,
) -> Option<model::Content> {
    if let (Some(group), Some(server), Some(change)) = (
        group,
        server,
        message
            .group_v2
            .as_ref()
            .and_then(|context| context.group_change.as_deref()),
    ) {
        let updates = group_updates(group, change, server)?;
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
    let body = clip_body(message.body.as_deref().unwrap_or_default()).into_owned();
    let attachments: Vec<_> = message
        .attachments
        .iter()
        .take(model::MAX_ATTACHMENTS)
        .map(attachment)
        .collect();
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

/// An attachment as its pointer describes it, to download unless it can't
/// be: too large, or without its size, key or digest.
fn attachment(pointer: &AttachmentPointer) -> Attachment {
    let short = |text: &Option<String>| text.clone().filter(|text| text.len() <= MAX_FIELD_BYTES);
    let downloadable = pointer.attachment_identifier.is_some()
        && pointer.key.is_some()
        && pointer.digest.is_some()
        && pointer
            .size
            .is_some_and(|size| size <= model::MAX_ATTACHMENT_SIZE);
    Attachment {
        content_type: short(&pointer.content_type),
        name: pointer
            .file_name
            .as_deref()
            .map(|name| clip_file_name(name).into_owned()),
        size: pointer.size,
        width: pointer.width.filter(|width| *width > 0),
        height: pointer.height.filter(|height| *height > 0),
        blurhash: short(&pointer.blur_hash),
        state: if downloadable {
            AttachmentState::Remote
        } else {
            AttachmentState::Unavailable
        },
        remote: downloadable.then(|| RemoteAttachment::new(pointer.encode_to_vec())),
        ..Attachment::new(attachment_kind(
            pointer.content_type.as_deref().unwrap_or(""),
            pointer.flags.unwrap_or(0),
        ))
    }
}

/// Longest content type or blurhash kept, in bytes.
const MAX_FIELD_BYTES: usize = 255;

fn attachment_kind(kind: &str, flags: u32) -> AttachmentKind {
    let has = |flag: attachment_pointer::Flags| flags & flag as u32 != 0;
    if has(attachment_pointer::Flags::Gif) || kind == "image/gif" {
        AttachmentKind::Gif
    } else if kind.starts_with("image/") {
        AttachmentKind::Image
    } else if kind.starts_with("video/") {
        AttachmentKind::Video
    } else if kind.starts_with("audio/") && has(attachment_pointer::Flags::VoiceMessage) {
        AttachmentKind::Voice
    } else if kind.starts_with("audio/") {
        AttachmentKind::Audio
    } else {
        AttachmentKind::File
    }
}

/// The message a reply quotes; `None` without its author or time. What the
/// reply says it said is ignored: it's shown as stored.
pub(super) fn quote(quote: &data_message::Quote) -> Option<Quote> {
    let author = service_id(
        quote.author_aci_binary.as_deref(),
        quote.author_aci.as_deref(),
    )?;
    Some(Quote {
        author: Some(author),
        sent_at: Timestamp::from_millis(quote.id?),
        original: None,
    })
}

/// The changes in an encrypted group change; `None` if it can't be read,
/// isn't signed by `server`, or isn't `group`'s at its revision.
fn group_updates(
    group: GroupRef,
    change: &[u8],
    server: &ServerPublicParams,
) -> Option<Vec<GroupUpdate>> {
    let change = proto::GroupChange::decode(change).ok()?;
    let signed = NotarySignatureBytes::try_from(change.server_signature.as_slice())
        .is_ok_and(|signature| server.verify_signature(&change.actions, signature).is_ok());
    if !signed {
        tracing::warn!("dropped a group change not signed by the server");
        return None;
    }
    let params = GroupSecretParams::derive_from_master_key(
        presage::libsignal_service::prelude::GroupMasterKey::new(*group.key.as_bytes()),
    );
    let changes = match GroupOperations::new(params).decrypt_group_change(change) {
        Ok(changes) => changes,
        Err(error) => {
            tracing::warn!(?error, "a group change could not be decrypted");
            return None;
        }
    };
    if changes.group_id != *group.key.id().as_bytes() || changes.version != group.revision {
        tracing::warn!("dropped a group change of another group or revision");
        return None;
    }
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
        GroupChange::Title(title) => GroupUpdate::Title(clean_name(&title).unwrap_or_default()),
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
        GroupChange::TerminateGroup => GroupUpdate::Ended,
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
pub(super) fn service_id(binary: Option<&[u8]>, string: Option<&str>) -> Option<ServiceId> {
    let id = binary
        .and_then(SignalServiceId::parse_from_service_id_binary)
        .or_else(|| string.and_then(SignalServiceId::parse_from_service_id_string))?;
    Some(ServiceId::new(id.raw_uuid()))
}

/// An identity key as Signal encodes it; `None` if invalid.
pub(super) fn identity_key(bytes: &[u8]) -> Option<IdentityKey> {
    let key = SignalIdentityKey::decode(bytes).ok()?;
    Some(IdentityKey::new(key.serialize().into_vec()))
}

/// When the sender's device says it sent the message.
fn client_timestamp(content: &Content) -> u64 {
    content.metadata.client_timestamp.timestamp_millis().max(0) as u64
}

fn server_timestamp(content: &Content) -> Timestamp {
    Timestamp::from_millis(content.metadata.server_timestamp.timestamp_millis().max(0) as u64)
}

#[cfg(test)]
pub(super) mod tests {
    use chrono::{TimeZone as _, Utc};
    use presage::libsignal_service::groups_v2::Timer;
    use presage::libsignal_service::proto::{call_message, group_change, sync_message};
    use std::sync::LazyLock;

    use presage::libsignal_service::protocol::DeviceId;
    use presage::libsignal_service::zkgroup::ServerSecretParams;
    use uuid::Uuid;

    use super::*;

    /// The test server, which signs group changes.
    static SERVER: LazyLock<ServerSecretParams> =
        LazyLock::new(|| ServerSecretParams::generate([7; 32]));
    static SERVER_PUBLIC: LazyLock<ServerPublicParams> =
        LazyLock::new(|| SERVER.get_public_params());

    pub(in crate::signal) fn server() -> &'static ServerPublicParams {
        &SERVER_PUBLIC
    }

    pub(in crate::signal) const SENDER: Uuid = Uuid::from_u128(0x1234);
    pub(in crate::signal) const OTHER: Uuid = Uuid::from_u128(0x4321);
    /// This account.
    const ME: Uuid = Uuid::from_u128(0x99);
    const KEY: [u8; 32] = [9; 32];
    /// When the server got [`content`]'s.
    const SERVER_AT: u64 = 1_700_000_001_000;

    pub(in crate::signal) fn content(body: ContentBody) -> Content {
        let at = Utc.timestamp_millis_opt(1_700_000_000_123).unwrap();
        let server_at = Utc.timestamp_millis_opt(SERVER_AT as i64).unwrap();
        Content {
            metadata: presage::libsignal_service::content::Metadata {
                sender: Aci::from(SENDER).into(),
                destination: Aci::from(ME).into(),
                sender_device: DeviceId::try_from(1u32).unwrap(),
                pni_verified: None,
                client_timestamp: at,
                server_timestamp: server_at,
                needs_receipt: false,
                unidentified_sender: true,
                was_plaintext: false,
                server_guid: None,
            },
            body,
        }
    }

    fn data(message: DataMessage) -> Option<model::Content> {
        received_message(&content(ContentBody::DataMessage(message)), server()).map(|m| m.content)
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
        assert_eq!(
            received(text("hi", Some(42))),
            Some(Incoming::Message(IncomingMessage {
                sender: ServiceId::new(SENDER),
                group: None,
                timestamp: Timestamp::from_millis(42),
                server_at: Timestamp::from_millis(SERVER_AT),
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
            received_message(&content(body), server())
                .unwrap()
                .expire_timer
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
        assert_eq!(
            sent_message(&content(body), server()).unwrap().expire_timer,
            Some(60)
        );
    }

    #[test]
    fn falls_back_to_client_timestamp() {
        let message = received_message(&content(text("hi", None)), server()).unwrap();
        assert_eq!(message.timestamp, Timestamp::from_millis(1_700_000_000_123));
    }

    /// The kinds and names of `content`'s attachments.
    fn kinds(content: Option<model::Content>) -> Vec<(AttachmentKind, Option<String>)> {
        let Some(model::Content::Text { attachments, .. }) = content else {
            panic!("not text");
        };
        attachments
            .into_iter()
            .map(|attachment| (attachment.kind, attachment.name))
            .collect()
    }

    /// A pointer to a downloadable attachment of `size` bytes.
    fn downloadable(content_type: &str, size: u32) -> AttachmentPointer {
        AttachmentPointer {
            attachment_identifier: Some(attachment_pointer::AttachmentIdentifier::CdnKey(
                "key".into(),
            )),
            key: Some(vec![1; 64]),
            digest: Some(vec![2; 32]),
            size: Some(size),
            ..pointer(content_type, 0, Some("cat.jpg"))
        }
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

        let content = data(message);
        assert_eq!(
            content.as_ref().and_then(|c| c.body()).unwrap().as_str(),
            "the graph"
        );
        assert_eq!(
            kinds(content),
            [
                (AttachmentKind::Image, None),
                (AttachmentKind::Gif, None),
                (AttachmentKind::Gif, None),
                (AttachmentKind::Video, None),
                (AttachmentKind::Voice, None),
                (AttachmentKind::Audio, None),
                (AttachmentKind::File, Some("report.pdf".into())),
            ]
        );
    }

    #[test]
    fn downloadable_attachments_keep_their_pointer() {
        let pointer = AttachmentPointer {
            width: Some(640),
            height: Some(480),
            blur_hash: Some("LEHV6nWB2yk8".into()),
            ..downloadable("image/jpeg", 1000)
        };
        let message = DataMessage {
            attachments: vec![pointer.clone()],
            ..Default::default()
        };

        let Some(model::Content::Text { attachments, .. }) = data(message) else {
            panic!("not text");
        };
        let expected = Attachment {
            content_type: Some("image/jpeg".into()),
            name: Some("cat.jpg".into()),
            size: Some(1000),
            width: Some(640),
            height: Some(480),
            blurhash: Some("LEHV6nWB2yk8".into()),
            state: AttachmentState::Remote,
            remote: Some(RemoteAttachment::new(pointer.encode_to_vec())),
            ..Attachment::new(AttachmentKind::Image)
        };
        assert_eq!(attachments, [expected]);
    }

    #[test]
    fn attachments_too_large_or_incomplete_cant_be_downloaded() {
        let too_large = downloadable("image/jpeg", model::MAX_ATTACHMENT_SIZE + 1);
        let unsized_ = AttachmentPointer {
            size: None,
            ..downloadable("image/jpeg", 1)
        };
        let keyless = AttachmentPointer {
            key: None,
            ..downloadable("image/jpeg", 1)
        };
        let undigested = AttachmentPointer {
            digest: None,
            ..downloadable("image/jpeg", 1)
        };
        let nowhere = AttachmentPointer {
            attachment_identifier: None,
            ..downloadable("image/jpeg", 1)
        };
        for pointer in [too_large, unsized_, keyless, undigested, nowhere] {
            let message = DataMessage {
                attachments: vec![pointer],
                ..Default::default()
            };
            let Some(model::Content::Text { attachments, .. }) = data(message) else {
                panic!("not text");
            };
            assert_eq!(attachments[0].state, AttachmentState::Unavailable);
            assert_eq!(attachments[0].remote, None);
        }
    }

    #[test]
    fn attachments_are_bounded() {
        use crate::text::MAX_FILE_NAME_BYTES;

        let long = AttachmentPointer {
            file_name: Some("a".repeat(MAX_FILE_NAME_BYTES * 2)),
            content_type: Some(format!("image/{}", "x".repeat(300))),
            blur_hash: Some("b".repeat(300)),
            ..downloadable("", 1)
        };
        let message = DataMessage {
            attachments: vec![long; model::MAX_ATTACHMENTS + 1],
            ..Default::default()
        };

        let Some(model::Content::Text { attachments, .. }) = data(message) else {
            panic!("not text");
        };
        assert_eq!(attachments.len(), model::MAX_ATTACHMENTS);
        let name = attachments[0].name.as_deref().unwrap();
        assert!(name.len() <= MAX_FILE_NAME_BYTES && name.ends_with('…'));
        assert_eq!(attachments[0].content_type, None);
        assert_eq!(attachments[0].blurhash, None);
    }

    #[test]
    fn long_bodies_are_clipped() {
        use crate::text::MAX_BODY_BYTES;

        let message = DataMessage {
            body: Some("a".repeat(MAX_BODY_BYTES + 1)),
            ..Default::default()
        };
        let Some(model::Content::Text { body, .. }) = data(message) else {
            panic!("not text");
        };
        assert!(body.as_str().len() <= MAX_BODY_BYTES);
        assert!(body.as_str().ends_with("a…"));
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
                attachments: vec![Attachment {
                    content_type: Some("image/jpeg".into()),
                    ..Attachment::new(AttachmentKind::Image)
                }],
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
            received_message(&content(body), server()).map(|message| message.content)
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
        assert_eq!(received_message(&content(hangup), server()), None);
    }

    #[test]
    fn skips_messages_that_show_nothing() {
        let empty = ContentBody::DataMessage(DataMessage::default());
        assert_eq!(received_message(&content(empty), server()), None);
        let delete = DataMessage {
            delete: Some(data_message::Delete::default()),
            ..Default::default()
        };
        assert_eq!(data(delete), None);
        let typing = ContentBody::TypingMessage(TypingMessage::default());
        assert_eq!(received_message(&content(typing), server()), None);
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

        let message = received_message(&content(body), server()).unwrap();

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
            assert_eq!(received_message(&content(body), server()), None);
        }
    }

    /// The actions of a group change by `editor` at revision 4, renaming
    /// the group and removing [`OTHER`].
    fn change_actions(editor: Uuid, title: &str) -> group_change::Actions {
        let params = GroupSecretParams::derive_from_master_key(
            presage::libsignal_service::prelude::GroupMasterKey::new(KEY),
        );
        let encrypt = |uuid: Uuid| {
            let id = SignalServiceId::from(Aci::from(uuid));
            presage::libsignal_service::zkgroup::serialize(&params.encrypt_service_id(id))
        };
        group_change::Actions {
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
        }
    }

    /// `actions` as a group change signed by the test server.
    fn signed(actions: &group_change::Actions) -> Vec<u8> {
        let actions = actions.encode_to_vec();
        proto::GroupChange {
            server_signature: SERVER.sign([3; 32], &actions).to_vec(),
            actions,
            ..Default::default()
        }
        .encode_to_vec()
    }

    /// A group message at `revision` carrying `change`.
    fn with_change(revision: u32, change: Vec<u8>) -> DataMessage {
        DataMessage {
            group_v2: Some(GroupContextV2 {
                master_key: Some(KEY.to_vec()),
                revision: Some(revision),
                group_change: Some(change),
            }),
            ..Default::default()
        }
    }

    #[test]
    fn group_changes_become_updates() {
        let change = signed(&change_actions(SENDER, "Dev team"));

        assert_eq!(
            data(with_change(4, change)),
            Some(model::Content::GroupChanged(vec![
                GroupUpdate::Removed(ServiceId::new(OTHER)),
                GroupUpdate::Title("Dev team".into()),
            ]))
        );
    }

    #[test]
    fn ending_a_group_becomes_an_update() {
        let actions = group_change::Actions {
            modify_title: None,
            delete_members: Vec::new(),
            terminate_group: Some(group_change::actions::TerminateGroupAction {}),
            ..change_actions(SENDER, "")
        };

        assert_eq!(
            data(with_change(4, signed(&actions))),
            Some(model::Content::GroupChanged(vec![GroupUpdate::Ended]))
        );
    }

    #[test]
    fn group_titles_lose_bidi_controls() {
        let change = signed(&change_actions(SENDER, "\u{2067}Dev\u{202e} team"));

        assert_eq!(
            data(with_change(4, change)),
            Some(model::Content::GroupChanged(vec![
                GroupUpdate::Removed(ServiceId::new(OTHER)),
                GroupUpdate::Title("Dev team".into()),
            ]))
        );
    }

    #[test]
    fn unreadable_group_changes_are_skipped() {
        assert_eq!(data(with_change(4, vec![1, 2, 3])), None);
    }

    #[test]
    fn group_changes_not_signed_by_the_server_are_skipped() {
        let actions = change_actions(SENDER, "Pwned");
        let unsigned = proto::GroupChange {
            actions: actions.encode_to_vec(),
            ..Default::default()
        };
        let forged = proto::GroupChange {
            server_signature: vec![7; 64],
            ..unsigned.clone()
        };
        let other_server = ServerSecretParams::generate([8; 32]);
        let foreign = proto::GroupChange {
            server_signature: other_server.sign([3; 32], &unsigned.actions).to_vec(),
            ..unsigned.clone()
        };
        let tampered = proto::GroupChange {
            actions: change_actions(SENDER, "Pwned!").encode_to_vec(),
            ..proto::GroupChange::decode(signed(&actions).as_slice()).unwrap()
        };

        for change in [unsigned, forged, foreign, tampered] {
            assert_eq!(data(with_change(4, change.encode_to_vec())), None);
        }
    }

    #[test]
    fn group_changes_of_another_revision_or_group_are_skipped() {
        let change = signed(&change_actions(SENDER, "Dev team"));
        assert_eq!(data(with_change(5, change)), None);

        let elsewhere = group_change::Actions {
            group_id: GroupMasterKey::new([8; 32]).id().as_bytes().to_vec(),
            ..change_actions(SENDER, "Dev team")
        };
        assert_eq!(data(with_change(4, signed(&elsewhere))), None);
    }

    #[test]
    fn edits_are_text_only() {
        let edit = |message: DataMessage| {
            received(ContentBody::EditMessage(EditMessage {
                target_sent_timestamp: Some(40),
                data_message: Some(DataMessage {
                    timestamp: Some(45),
                    ..message
                }),
            }))
        };
        let change = DataMessage {
            body: Some("hi".into()),
            ..with_change(4, signed(&change_actions(SENDER, "Dev team")))
        };
        let timer = DataMessage {
            flags: Some(data_message::Flags::ExpirationTimerUpdate as u32),
            expire_timer: Some(60),
            ..Default::default()
        };
        let reaction = DataMessage {
            reaction: Some(data_message::Reaction {
                emoji: Some("👍".into()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let Some(Incoming::Change(edited)) = edit(change) else {
            panic!("a text edit in a group");
        };
        assert_eq!(
            edited.change,
            Change::Edit {
                content: model::Content::text("hi"),
                at: Timestamp::from_millis(45),
            }
        );
        assert_eq!(edit(timer), None);
        assert_eq!(edit(reaction), None);
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
        incoming(
            Received::Content(Box::new(content(body))),
            Aci::from(ME),
            server(),
        )
    }

    /// `body` as sent by another of this account's devices.
    fn synced(body: ContentBody) -> Option<Incoming> {
        let mut content = content(body);
        content.metadata.sender = Aci::from(ME).into();
        incoming(
            Received::Content(Box::new(content)),
            Aci::from(ME),
            server(),
        )
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
        let read = synced(ContentBody::SynchronizeMessage(SyncMessage {
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

        let settings = synced(ContentBody::SynchronizeMessage(SyncMessage {
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

    fn verified(state: proto::verified::State, key: Option<Vec<u8>>) -> Option<Incoming> {
        synced(ContentBody::SynchronizeMessage(SyncMessage {
            content: Some(SyncContent::Verified(proto::Verified {
                destination_aci_binary: Some(OTHER.as_bytes().to_vec()),
                identity_key: key,
                state: Some(state.into()),
                ..Default::default()
            })),
            ..Default::default()
        }))
    }

    #[test]
    fn maps_verifications_from_the_phone() {
        use presage::libsignal_service::protocol::IdentityKeyPair;

        let key = IdentityKeyPair::generate(&mut rand::rng())
            .identity_key()
            .serialize();
        let with_key = |state| verified(state, Some(key.to_vec()));

        let expected = |verified| Incoming::Verified {
            peer: ServiceId::new(OTHER),
            key: IdentityKey::new(key.to_vec()),
            verified,
        };
        assert_eq!(
            with_key(proto::verified::State::Verified),
            Some(expected(true))
        );
        assert_eq!(
            with_key(proto::verified::State::Default),
            Some(expected(false))
        );
        assert_eq!(with_key(proto::verified::State::Unverified), None);
        assert_eq!(verified(proto::verified::State::Verified, None), None);
        assert_eq!(
            verified(proto::verified::State::Verified, Some(vec![5, 1])),
            None,
            "not a key"
        );
    }

    fn sync(content: SyncContent) -> Option<Incoming> {
        synced(ContentBody::SynchronizeMessage(SyncMessage {
            content: Some(content),
            ..Default::default()
        }))
    }

    #[test]
    fn maps_request_answers_from_the_phone() {
        use message_request_response::Type;
        let answer = |thread: Option<Uuid>, group: Option<Vec<u8>>, kind: Type| {
            sync(SyncContent::MessageRequestResponse(
                sync_message::MessageRequestResponse {
                    thread_aci_binary: thread.map(|id| id.as_bytes().to_vec()),
                    group_id: group,
                    r#type: Some(kind.into()),
                    ..Default::default()
                },
            ))
        };
        let contact = ChatRef::Contact(ServiceId::new(OTHER));
        let cases = [
            (Type::Accept, Some(RequestResponse::Accept)),
            (Type::Delete, Some(RequestResponse::Delete)),
            (Type::Block, Some(RequestResponse::Block)),
            (Type::BlockAndSpam, Some(RequestResponse::Block)),
            (Type::BlockAndDelete, Some(RequestResponse::BlockAndDelete)),
            (Type::Spam, None),
            (Type::Unknown, None),
        ];
        for (kind, expected) in cases {
            let expected = expected.map(|response| Incoming::RequestResponse {
                chat: contact,
                response,
            });
            assert_eq!(answer(Some(OTHER), None, kind), expected, "{kind:?}");
        }
        assert_eq!(
            answer(None, Some(vec![3; 32]), Type::Accept),
            Some(Incoming::RequestResponse {
                chat: ChatRef::Group(GroupId::new([3; 32])),
                response: RequestResponse::Accept,
            })
        );
        assert_eq!(answer(None, Some(vec![3; 5]), Type::Accept), None);
        assert_eq!(answer(None, None, Type::Accept), None);
    }

    #[test]
    fn maps_the_blocked_list_from_the_phone() {
        let blocked = sync(SyncContent::Blocked(sync_message::Blocked {
            numbers: vec!["+14155550123".into(), String::new()],
            acis: vec![OTHER.to_string(), SENDER.to_string(), "nonsense".into()],
            acis_binary: vec![OTHER.as_bytes().to_vec()],
            group_ids: vec![vec![3; 32], vec![4; 2]],
        }));
        assert_eq!(
            blocked,
            Some(Incoming::Blocked(BlockedList {
                accounts: vec![ServiceId::new(OTHER), ServiceId::new(SENDER)],
                groups: vec![GroupId::new([3; 32])],
                numbers: vec![PhoneNumber::new("+14155550123")],
            }))
        );
    }

    #[test]
    fn storage_service_changes_are_noticed() {
        let fetch = |kind: fetch_latest::Type| {
            sync(SyncContent::FetchLatest(sync_message::FetchLatest {
                r#type: Some(kind.into()),
            }))
        };
        assert_eq!(
            fetch(fetch_latest::Type::StorageManifest),
            Some(Incoming::StorageChanged)
        );
        assert_eq!(fetch(fetch_latest::Type::LocalProfile), None);
    }

    #[test]
    fn requests_and_blocks_only_come_from_this_account() {
        let blocked = ContentBody::SynchronizeMessage(SyncMessage {
            content: Some(SyncContent::Blocked(sync_message::Blocked::default())),
            ..Default::default()
        });
        assert_eq!(received(blocked), None);
    }

    #[test]
    fn replies_name_the_quoted_message_only() {
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
                    original: None,
                }),
            })
        );
        assert_eq!(
            quote(&attachment_only),
            Some(Quote {
                author: Some(ServiceId::new(OTHER)),
                sent_at: Timestamp::from_millis(51),
                original: None,
            })
        );
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
                server_at: Timestamp::from_millis(SERVER_AT),
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
                server_at: Timestamp::from_millis(SERVER_AT),
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
            synced(edit),
            Some(Incoming::Change(MessageChange {
                destination: Destination::Contact(ServiceId::new(RECIPIENT)),
                author: None,
                target: Timestamp::from_millis(80),
                server_at: Timestamp::from_millis(SERVER_AT),
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
        let Some(Incoming::Change(change)) = synced(delete) else {
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
            synced(ContentBody::SynchronizeMessage(SyncMessage {
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
    fn ignores_syncs_from_other_accounts() {
        use presage::libsignal_service::proto::sync_message::DeleteForMe;
        use presage::libsignal_service::protocol::Pni;

        let recipient = Some(RECIPIENT.to_string());
        let syncs = [
            SyncMessage {
                read: vec![sync_message::Read {
                    sender_aci: Some(OTHER.to_string()),
                    timestamp: Some(5),
                    ..Default::default()
                }],
                ..Default::default()
            },
            SyncMessage {
                content: Some(SyncContent::DeleteForMe(DeleteForMe {
                    conversation_deletes: vec![sync_message::delete_for_me::ConversationDelete {
                        conversation: Some(ConversationIdentifier {
                            identifier: Some(conversation_identifier::Identifier::ThreadGroupId(
                                vec![4; 32],
                            )),
                        }),
                        ..Default::default()
                    }],
                    ..Default::default()
                })),
                ..Default::default()
            },
            SyncMessage {
                content: Some(SyncContent::Configuration(sync_message::Configuration {
                    read_receipts: Some(true),
                    ..Default::default()
                })),
                ..Default::default()
            },
            SyncMessage {
                content: Some(SyncContent::Sent(Sent {
                    destination_service_id: recipient.clone(),
                    timestamp: Some(77),
                    message: sent_data("planted", false),
                    ..Default::default()
                })),
                ..Default::default()
            },
            SyncMessage {
                content: Some(SyncContent::Sent(Sent {
                    destination_service_id: recipient.clone(),
                    timestamp: Some(90),
                    edit_message: Some(EditMessage {
                        target_sent_timestamp: Some(80),
                        data_message: sent_data("edited", false),
                    }),
                    ..Default::default()
                })),
                ..Default::default()
            },
            SyncMessage {
                content: Some(SyncContent::Sent(Sent {
                    destination_service_id: recipient,
                    timestamp: Some(91),
                    message: Some(DataMessage {
                        delete: Some(data_message::Delete {
                            target_sent_timestamp: Some(80),
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                })),
                ..Default::default()
            },
        ];

        for sync in syncs {
            let body = ContentBody::SynchronizeMessage(sync);
            assert!(synced(body.clone()).is_some(), "{body:?}");
            assert_eq!(received(body.clone()), None, "{body:?}");
            let mut own_pni = content(body.clone());
            own_pni.metadata.sender = Pni::from(ME).into();
            assert_eq!(
                incoming(
                    Received::Content(Box::new(own_pni)),
                    Aci::from(ME),
                    server()
                ),
                None,
                "{body:?}"
            );
        }
    }

    #[test]
    fn maps_queue_empty_and_contacts() {
        let me = Aci::from(ME);
        assert_eq!(
            incoming(Received::QueueEmpty, me, server()),
            Some(Incoming::QueueEmpty)
        );
        let contacts = |complete, saved| Received::Contacts { complete, saved };
        let synced = |all| Some(Incoming::ContactsSynced { all });
        let ids = vec![SENDER, OTHER];
        assert_eq!(
            incoming(contacts(true, Some(ids.clone())), me, server()),
            synced(Some(vec![ServiceId::new(SENDER), ServiceId::new(OTHER)]))
        );
        assert_eq!(
            incoming(contacts(false, Some(ids)), me, server()),
            synced(None)
        );
        assert_eq!(incoming(contacts(true, None), me, server()), synced(None));
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
        let Some(Incoming::Undecryptable(failed)) = incoming(
            failure(ContentHint::Resendable, Some(vec![4; 32])),
            Aci::from(ME),
            server(),
        ) else {
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
        let Some(Incoming::Undecryptable(failed)) = incoming(
            failure(ContentHint::Implicit, Some(vec![4; 3])),
            Aci::from(ME),
            server(),
        ) else {
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
            incoming(request, Aci::from(ME), server()),
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
            synced(body),
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

        let sent = sent_message(&content(body), server()).unwrap();
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

        let sent = sent_message(&content(body), server()).unwrap();

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

        assert_eq!(sent_message(&content(no_recipient), server()), None);
    }
}
