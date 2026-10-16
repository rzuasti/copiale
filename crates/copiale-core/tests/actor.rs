//! Core actor against the fake backend, fake linker and in-memory keychain,
//! with real encrypted storage in a temp dir.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use copiale_core::storage::{KdfParams, Passphrase, Storage, StoragePaths};
use copiale_core::{
    BackendError, Change, ChatRef, Command, Contact, Content, ConversationId, ConversationSummary,
    CoreHandle, CoreOptions, Destination, Direction, Event, Events, Expiry, GroupInfo, GroupRef,
    GroupUpdate, Incoming, IncomingMessage, Member, Message, MessageBody, MessageChange,
    MessagePreview, MessageRef, MessageStatus, Outgoing, Peer, PhoneNumber, Quote, ReceiptKind,
    SentMessage, StorageFailure, Timestamp,
};
use copiale_testkit::{
    FAKE_LINK_URL, FakeNetwork, FakePhone, ManualClock, MemorySecretStore, OWN_ID, SentText,
    fake_backend, fake_linker, group_key, service_id, text,
};
use tempfile::TempDir;

const CHEAP_KDF: KdfParams = KdfParams {
    memory_kib: 64,
    iterations: 1,
    parallelism: 1,
};

const RETRY: Duration = Duration::from_millis(10);

/// Quick reconnects and connection checks.
fn fast() -> CoreOptions {
    CoreOptions {
        reconnect_initial: RETRY,
        reconnect_max: RETRY * 4,
        probe_interval: RETRY,
        probe_timeout: RETRY * 2,
        ..CoreOptions::default()
    }
}

struct Running {
    core: CoreHandle,
    events: Events,
    network: FakeNetwork,
    phone: FakePhone,
}

fn start(dir: &Path, secrets: &MemorySecretStore, linked: bool) -> Running {
    start_with(dir, secrets, linked, fast(), |_| {})
}

/// `script` prepares the network before the core starts using it.
fn start_with(
    dir: &Path,
    secrets: &MemorySecretStore,
    linked: bool,
    options: CoreOptions,
    script: impl FnOnce(&FakeNetwork),
) -> Running {
    let (backend, network) = fake_backend();
    script(&network);
    let (linker, phone) = fake_linker(backend);
    if linked {
        phone.set_linked();
    }
    let storage = Storage::new(StoragePaths::in_dir(dir)).with_kdf_params(CHEAP_KDF);
    let (core, events) = copiale_core::spawn_with_options(
        storage,
        Arc::new(secrets.clone()),
        move || linker,
        options,
    )
    .unwrap();
    Running {
        core,
        events,
        network,
        phone,
    }
}

/// A linked account with a keychain, connected.
async fn ready() -> (TempDir, Running) {
    let dir = tempfile::tempdir().unwrap();
    let mut running = start(dir.path(), &MemorySecretStore::default(), true);
    expect(&mut running.events, Event::Ready).await;
    expect(&mut running.events, Event::Connected).await;
    (dir, running)
}

fn incoming(from: u128, sent_at: u64, body: &str) -> Incoming {
    Incoming::Message(IncomingMessage {
        sender: service_id(from),
        group: None,
        timestamp: Timestamp::from_millis(sent_at),
        content: Content::text(body),
        expire_timer: None,
    })
}

async fn message_added(events: &mut Events) -> Message {
    match next_event(events).await {
        Event::MessageAdded(message) => message,
        other => panic!("expected MessageAdded, got {other:?}"),
    }
}

async fn conversation_updated(events: &mut Events) -> ConversationSummary {
    match next_event(events).await {
        Event::ConversationUpdated(summary) => summary,
        other => panic!("expected ConversationUpdated, got {other:?}"),
    }
}

async fn expect_disconnected(events: &mut Events, retry_in: Duration) {
    let Event::Disconnected {
        retry_in: actual, ..
    } = next_event(events).await
    else {
        panic!("expected Disconnected");
    };
    assert_eq!(actual, retry_in);
}

async fn next_event(events: &mut Events) -> Event {
    tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("event within 5 s")
        .expect("events still open")
}

async fn expect(events: &mut Events, expected: Event) {
    assert_eq!(next_event(events).await, expected);
}

/// Shuts down and checks storage was closed before `Stopped`.
async fn stop(running: Running) {
    let Running {
        core, mut events, ..
    } = running;
    core.shutdown().expect("core thread exits cleanly");
    loop {
        if next_event(&mut events).await == Event::Stopped {
            break;
        }
    }
    assert_eq!(events.recv().await, None);
}

async fn expect_unlinked(events: &mut Events) {
    let Event::Unlinked { device_name } = next_event(events).await else {
        panic!("expected Unlinked");
    };
    assert!(device_name.starts_with("Copiale"), "{device_name}");
}

fn send(running: &Running, command: Command) {
    running.core.send(command).unwrap();
}

// Messaging

#[tokio::test]
async fn incoming_message_is_stored_and_reported() {
    let (_dir, mut running) = ready().await;

    running
        .network
        .deliver(incoming(1, 1_700_000_000_000, "hello"));
    running.network.deliver(Incoming::QueueEmpty);

    let message = message_added(&mut running.events).await;
    assert_eq!(message.direction, Direction::Incoming);
    assert_eq!(message.sent_at, Timestamp::from_millis(1_700_000_000_000));
    assert_eq!(message.content, Content::text("hello"));
    assert!(!message.read);
    let summary = conversation_updated(&mut running.events).await;
    assert_eq!(summary.id, message.conversation);
    assert_eq!(summary.peer, Peer::Contact(service_id(1)));
    assert_eq!(summary.unread, 1);
    assert_eq!(
        summary.last_message,
        Some(MessagePreview {
            direction: Direction::Incoming,
            content: Content::text("hello"),
        })
    );
    expect(&mut running.events, Event::Synced).await;
    stop(running).await;
}

#[tokio::test]
async fn redelivered_message_is_ignored() {
    let (_dir, mut running) = ready().await;
    running.network.deliver(incoming(1, 100, "once"));
    message_added(&mut running.events).await;
    conversation_updated(&mut running.events).await;

    running.network.deliver(incoming(1, 100, "once"));
    running.network.deliver(Incoming::QueueEmpty);

    expect(&mut running.events, Event::Synced).await;
    stop(running).await;
}

#[tokio::test]
async fn message_sent_from_the_phone_is_stored_as_outgoing() {
    let (_dir, mut running) = ready().await;

    running.network.deliver(Incoming::Sent(SentMessage {
        destination: Destination::Contact(service_id(2)),
        timestamp: Timestamp::from_millis(200),
        content: Content::text("from my phone"),
        expire_timer: None,
    }));

    let message = message_added(&mut running.events).await;
    assert_eq!(message.direction, Direction::Outgoing);
    assert!(message.read);
    let summary = conversation_updated(&mut running.events).await;
    assert_eq!(summary.peer, Peer::Contact(service_id(2)));
    assert_eq!(summary.unread, 0);
    stop(running).await;
}

#[tokio::test]
async fn conversations_and_messages_can_be_loaded() {
    let (_dir, mut running) = ready().await;
    for (from, sent_at, body) in [(1, 100, "a"), (2, 100, "b"), (1, 101, "c")] {
        running.network.deliver(incoming(from, sent_at, body));
        message_added(&mut running.events).await;
        conversation_updated(&mut running.events).await;
    }

    send(&running, Command::LoadConversations);
    let Event::Conversations(conversations) = next_event(&mut running.events).await else {
        panic!("expected Conversations");
    };
    let peers: Vec<_> = conversations.iter().map(|c| c.peer).collect();
    assert_eq!(
        peers,
        [Peer::Contact(service_id(1)), Peer::Contact(service_id(2))]
    );
    let first = conversations[0].id;

    send(
        &running,
        Command::LoadMessages {
            conversation: first,
            before: None,
            limit: 10,
        },
    );
    let Event::Messages {
        conversation,
        before: None,
        messages,
    } = next_event(&mut running.events).await
    else {
        panic!("expected Messages");
    };
    assert_eq!(conversation, first);
    let bodies: Vec<_> = messages
        .iter()
        .map(|m| m.content.body().unwrap().as_str().to_string())
        .collect();
    assert_eq!(bodies, ["a", "c"]);
    stop(running).await;
}

#[tokio::test]
async fn mark_read_updates_the_conversation_once() {
    let (_dir, mut running) = ready().await;
    running.network.deliver(incoming(1, 100, "unread"));
    let message = message_added(&mut running.events).await;
    conversation_updated(&mut running.events).await;

    send(&running, Command::MarkRead(message.conversation));
    assert_eq!(conversation_updated(&mut running.events).await.unread, 0);

    // Nothing left unread: no update, so the next event answers this load.
    send(&running, Command::MarkRead(message.conversation));
    send(&running, Command::LoadConversations);
    assert!(matches!(
        next_event(&mut running.events).await,
        Event::Conversations(_)
    ));
    stop(running).await;
}

#[tokio::test]
async fn messages_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = MemorySecretStore::default();
    let mut running = start(dir.path(), &secrets, true);
    expect(&mut running.events, Event::Ready).await;
    expect(&mut running.events, Event::Connected).await;
    running.network.deliver(incoming(1, 100, "kept"));
    let message = message_added(&mut running.events).await;
    stop(running).await;

    let mut running = start(dir.path(), &secrets, true);
    expect(&mut running.events, Event::Ready).await;
    send(
        &running,
        Command::LoadMessages {
            conversation: message.conversation,
            before: None,
            limit: 10,
        },
    );
    let messages = loop {
        if let Event::Messages { messages, .. } = next_event(&mut running.events).await {
            break messages;
        }
    };
    assert_eq!(messages, [message]);
    stop(running).await;
}

#[tokio::test]
async fn contacts_are_requested_and_refresh_conversations() {
    let (_dir, mut running) = ready().await;
    // No contacts stored yet, so the core asked for them on start.
    assert_eq!(running.network.contact_sync_requests(), 1);

    running.network.deliver(Incoming::ContactsSynced);

    assert!(matches!(
        next_event(&mut running.events).await,
        Event::Conversations(_)
    ));
    expect(&mut running.events, Event::Contacts(Vec::new())).await;
    stop(running).await;
}

fn contact(n: u128, name: Option<&str>, phone: Option<&str>) -> Contact {
    Contact {
        id: service_id(n),
        name: name.map(Into::into),
        phone: phone.map(PhoneNumber::new),
    }
}

async fn contacts(events: &mut Events) -> Vec<Contact> {
    loop {
        if let Event::Contacts(contacts) = next_event(events).await {
            break contacts;
        }
    }
}

#[tokio::test]
async fn synced_contacts_are_listed_and_title_chats() {
    let (_dir, mut running) = ready().await;
    running.network.deliver(incoming(1, 100, "hi"));
    message_added(&mut running.events).await;
    conversation_updated(&mut running.events).await;

    let alice = contact(1, Some("Alice"), Some("+14155550123"));
    let bob = contact(2, Some("Bob"), None);
    running
        .network
        .sync_contacts(&[bob.clone(), alice.clone()])
        .await;

    let Event::Conversations(conversations) = next_event(&mut running.events).await else {
        panic!("expected Conversations");
    };
    assert_eq!(conversations[0].title.as_deref(), Some("Alice"));
    assert_eq!(
        contacts(&mut running.events).await,
        [alice.clone(), bob.clone()]
    );

    send(&running, Command::LoadContacts);
    assert_eq!(contacts(&mut running.events).await, [alice, bob]);
    stop(running).await;
}

#[tokio::test]
async fn contacts_are_asked_for_daily() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = MemorySecretStore::default();
    let mut running = start(dir.path(), &secrets, true);
    expect(&mut running.events, Event::Ready).await;
    expect(&mut running.events, Event::Connected).await;
    running
        .network
        .sync_contacts(&[contact(1, Some("Alice"), None)])
        .await;
    contacts(&mut running.events).await;
    assert_eq!(running.network.contact_sync_requests(), 1);
    stop(running).await;

    // Asked less than a day ago, with contacts stored.
    let mut running = start(dir.path(), &secrets, true);
    expect(&mut running.events, Event::Ready).await;
    expect(&mut running.events, Event::Connected).await;
    assert_eq!(running.network.contact_sync_requests(), 0);
    stop(running).await;
}

fn send_text(running: &Running, conversation: ConversationId, body: &str) {
    send(
        running,
        Command::SendText {
            conversation,
            body: MessageBody::new(body),
            reply_to: None,
        },
    );
}

/// The 1:1 conversation with `peer`, created if new; skips connection
/// events on the way.
async fn open_chat(running: &mut Running, peer: u128) -> ConversationId {
    send(running, Command::OpenConversation(service_id(peer)));
    loop {
        match next_event(&mut running.events).await {
            Event::ConversationUpdated(summary) => {
                assert_eq!(summary.peer, Peer::Contact(service_id(peer)));
                break summary.id;
            }
            Event::Disconnected { .. } | Event::Connected => {}
            other => panic!("expected ConversationUpdated, got {other:?}"),
        }
    }
}

/// The pending message stored for a send.
async fn pending_added(events: &mut Events) -> Message {
    let message = message_added(events).await;
    assert_eq!(message.direction, Direction::Outgoing);
    assert_eq!(message.status, Some(MessageStatus::Pending));
    conversation_updated(events).await;
    message
}

async fn message_updated(events: &mut Events) -> Message {
    match next_event(events).await {
        Event::MessageUpdated(message) => message,
        other => panic!("expected MessageUpdated, got {other:?}"),
    }
}

async fn send_failed(events: &mut Events) -> (Message, BackendError) {
    match next_event(events).await {
        Event::SendFailed { message, error } => (message, error),
        other => panic!("expected SendFailed, got {other:?}"),
    }
}

async fn load_messages(running: &mut Running, conversation: ConversationId) -> Vec<Message> {
    send(
        running,
        Command::LoadMessages {
            conversation,
            before: None,
            limit: 10,
        },
    );
    loop {
        if let Event::Messages { messages, .. } = next_event(&mut running.events).await {
            break messages;
        }
    }
}

#[tokio::test]
async fn sent_text_goes_from_pending_to_sent() {
    let (_dir, mut running) = ready().await;

    let chat = open_chat(&mut running, 2).await;
    send_text(&running, chat, "hi there");

    let pending = message_added(&mut running.events).await;
    assert_eq!(pending.direction, Direction::Outgoing);
    assert_eq!(pending.status, Some(MessageStatus::Pending));
    assert_eq!(pending.content, Content::text("hi there"));
    assert!(pending.read);
    let summary = conversation_updated(&mut running.events).await;
    assert_eq!(summary.peer, Peer::Contact(service_id(2)));
    assert_eq!(summary.unread, 0);
    let sent = message_updated(&mut running.events).await;
    assert_eq!(
        sent,
        Message {
            status: Some(MessageStatus::Sent),
            ..pending.clone()
        }
    );
    assert_eq!(
        running.network.sent(),
        [SentText {
            destination: Destination::Contact(service_id(2)),
            outgoing: text("hi there"),
            timestamp: pending.sent_at,
        }]
    );
    assert_eq!(
        load_messages(&mut running, pending.conversation).await,
        [sent]
    );
    stop(running).await;
}

#[tokio::test]
async fn failed_send_can_be_retried() {
    let (_dir, mut running) = ready().await;
    running
        .network
        .fail_next_send(BackendError::new("rate limited"));

    let chat = open_chat(&mut running, 3).await;
    send_text(&running, chat, "try again");
    let pending = pending_added(&mut running.events).await;
    let (failed, error) = send_failed(&mut running.events).await;
    assert_eq!(failed.id, pending.id);
    assert_eq!(failed.status, Some(MessageStatus::Failed));
    assert_eq!(error, BackendError::new("rate limited"));
    assert!(running.network.sent().is_empty());

    send(&running, Command::RetrySend(pending.id));

    let retried = message_updated(&mut running.events).await;
    assert_eq!(retried.status, Some(MessageStatus::Pending));
    let sent = message_updated(&mut running.events).await;
    assert_eq!(sent.status, Some(MessageStatus::Sent));
    // Same timestamp, so the recipient can drop a copy that got through.
    assert_eq!(running.network.sent()[0].timestamp, pending.sent_at);
    stop(running).await;
}

#[tokio::test]
async fn only_failed_messages_are_retried() {
    let (_dir, mut running) = ready().await;
    let chat = open_chat(&mut running, 3).await;
    send_text(&running, chat, "fine");
    let pending = pending_added(&mut running.events).await;
    message_updated(&mut running.events).await;

    send(&running, Command::RetrySend(pending.id));
    send(&running, Command::LoadConversations);

    assert!(matches!(
        next_event(&mut running.events).await,
        Event::Conversations(_)
    ));
    assert_eq!(running.network.sends_started(), 1);
    stop(running).await;
}

#[tokio::test]
async fn slow_send_does_not_delay_incoming_messages() {
    let (_dir, mut running) = ready().await;
    running.network.pause_sends();

    let chat = open_chat(&mut running, 2).await;
    send_text(&running, chat, "slow");
    let pending = pending_added(&mut running.events).await;
    running.network.deliver(incoming(1, 100, "meanwhile"));

    let received = message_added(&mut running.events).await;
    assert_eq!(received.content, Content::text("meanwhile"));
    conversation_updated(&mut running.events).await;
    send(&running, Command::LoadConversations);
    assert!(matches!(
        next_event(&mut running.events).await,
        Event::Conversations(_)
    ));

    running.network.resume_sends();
    let sent = message_updated(&mut running.events).await;
    assert_eq!(sent.id, pending.id);
    assert_eq!(sent.status, Some(MessageStatus::Sent));
    stop(running).await;
}

#[tokio::test]
async fn sends_go_out_one_at_a_time_in_order() {
    let (_dir, mut running) = ready().await;
    running.network.pause_sends();

    let chat = open_chat(&mut running, 2).await;
    for body in ["first", "second", "third"] {
        send_text(&running, chat, body);
    }
    let mut pending = Vec::new();
    for _ in 0..3 {
        pending.push(pending_added(&mut running.events).await);
    }
    assert_eq!(running.network.sends_started(), 1);
    // Unique timestamps even when sent within the same millisecond.
    assert!(pending.windows(2).all(|w| w[0].sent_at < w[1].sent_at));

    running.network.resume_sends();
    for expected in &pending {
        assert_eq!(message_updated(&mut running.events).await.id, expected.id);
    }
    let bodies: Vec<_> = running
        .network
        .sent()
        .into_iter()
        .map(|sent| match sent.outgoing {
            Outgoing::Text { body, .. } => body.as_str().to_string(),
            other => panic!("expected text, got {other:?}"),
        })
        .collect();
    assert_eq!(bodies, ["first", "second", "third"]);
    stop(running).await;
}

#[tokio::test]
async fn one_failure_does_not_stop_later_sends() {
    let (_dir, mut running) = ready().await;
    running
        .network
        .fail_next_send(BackendError::new("rejected"));
    running.network.pause_sends();

    let chat = open_chat(&mut running, 2).await;
    send_text(&running, chat, "fails");
    send_text(&running, chat, "works");
    let failing = pending_added(&mut running.events).await;
    let working = pending_added(&mut running.events).await;
    running.network.resume_sends();

    assert_eq!(send_failed(&mut running.events).await.0.id, failing.id);
    assert_eq!(message_updated(&mut running.events).await.id, working.id);
    stop(running).await;
}

#[tokio::test]
async fn empty_text_is_not_sent() {
    let (_dir, mut running) = ready().await;

    let chat = open_chat(&mut running, 2).await;
    send_text(&running, chat, "  \n ");
    send(&running, Command::LoadConversations);

    let Event::Conversations(conversations) = next_event(&mut running.events).await else {
        panic!("expected Conversations");
    };
    assert_eq!(conversations.len(), 1);
    assert_eq!(conversations[0].last_message, None);
    assert_eq!(running.network.sends_started(), 0);
    stop(running).await;
}

#[tokio::test]
async fn sends_interrupted_by_quitting_fail_on_restart() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = MemorySecretStore::default();
    let mut running = start(dir.path(), &secrets, true);
    expect(&mut running.events, Event::Ready).await;
    expect(&mut running.events, Event::Connected).await;
    running.network.pause_sends();
    let chat = open_chat(&mut running, 2).await;
    send_text(&running, chat, "interrupted");
    let pending = pending_added(&mut running.events).await;
    stop(running).await;

    let mut running = start(dir.path(), &secrets, true);
    expect(&mut running.events, Event::Ready).await;

    let messages = load_messages(&mut running, pending.conversation).await;
    assert_eq!(messages[0].status, Some(MessageStatus::Failed));
    stop(running).await;
}

/// The next event other than reconnect attempts.
async fn skip_disconnects(events: &mut Events) -> Event {
    loop {
        match next_event(events).await {
            Event::Disconnected { .. } => {}
            event => break event,
        }
    }
}

/// Waits for queued commands to be handled.
async fn sync(running: &mut Running) {
    send(running, Command::LoadConversations);
    loop {
        if let Event::Conversations(_) = skip_disconnects(&mut running.events).await {
            break;
        }
    }
}

#[tokio::test]
async fn messages_wait_for_the_connection() {
    let (_dir, mut running) = ready().await;
    running.network.go_offline();
    expect_disconnected(&mut running.events, RETRY).await;

    let chat = open_chat(&mut running, 2).await;
    send_text(&running, chat, "later");
    let Event::MessageAdded(pending) = skip_disconnects(&mut running.events).await else {
        panic!("expected MessageAdded");
    };
    sync(&mut running).await;
    assert_eq!(running.network.sends_started(), 0);

    running.network.go_online();
    assert_eq!(
        skip_disconnects(&mut running.events).await,
        Event::Connected
    );
    let sent = message_updated(&mut running.events).await;
    assert_eq!(sent.id, pending.id);
    assert_eq!(sent.status, Some(MessageStatus::Sent));
    stop(running).await;
}

#[tokio::test]
async fn send_cut_off_by_a_dead_connection_is_resent() {
    let (_dir, mut running) = ready().await;
    running.network.pause_sends();
    let chat = open_chat(&mut running, 2).await;
    send_text(&running, chat, "stuck");
    let pending = pending_added(&mut running.events).await;
    sync(&mut running).await;
    assert_eq!(running.network.sends_started(), 1);

    running.network.go_offline();
    expect(
        &mut running.events,
        Event::Disconnected {
            error: Some(BackendError::network("no answer from the server")),
            retry_in: RETRY,
        },
    )
    .await;
    running.network.resume_sends();
    running.network.go_online();

    assert_eq!(
        skip_disconnects(&mut running.events).await,
        Event::Connected
    );
    assert_eq!(message_updated(&mut running.events).await.id, pending.id);
    assert_eq!(running.network.sends_started(), 2);
    assert_eq!(
        running.network.sent(),
        [SentText {
            destination: Destination::Contact(service_id(2)),
            outgoing: text("stuck"),
            timestamp: pending.sent_at,
        }]
    );
    stop(running).await;
}

#[tokio::test]
async fn network_error_while_sending_reconnects_and_resends() {
    let (_dir, mut running) = ready().await;
    let error = BackendError::network("connection reset");
    running.network.fail_next_send(error.clone());

    let chat = open_chat(&mut running, 2).await;
    send_text(&running, chat, "again");
    let pending = pending_added(&mut running.events).await;

    expect(
        &mut running.events,
        Event::Disconnected {
            error: Some(error),
            retry_in: RETRY,
        },
    )
    .await;
    expect(&mut running.events, Event::Connected).await;
    let sent = message_updated(&mut running.events).await;
    assert_eq!(sent.id, pending.id);
    assert_eq!(sent.status, Some(MessageStatus::Sent));
    assert_eq!(running.network.sends_started(), 2);
    stop(running).await;
}

#[tokio::test]
async fn messages_waiting_too_long_fail() {
    let dir = tempfile::tempdir().unwrap();
    let options = CoreOptions {
        send_timeout: Duration::from_millis(100),
        ..fast()
    };
    let mut running = start_with(
        dir.path(),
        &MemorySecretStore::default(),
        true,
        options,
        |_| {},
    );
    expect(&mut running.events, Event::Ready).await;
    expect(&mut running.events, Event::Connected).await;
    running.network.go_offline();

    let chat = open_chat(&mut running, 2).await;
    send_text(&running, chat, "too late");
    let Event::MessageAdded(pending) = skip_disconnects(&mut running.events).await else {
        panic!("expected MessageAdded");
    };
    assert!(matches!(
        skip_disconnects(&mut running.events).await,
        Event::ConversationUpdated(_)
    ));
    let Event::SendFailed { message, error } = skip_disconnects(&mut running.events).await else {
        panic!("expected SendFailed");
    };
    assert_eq!(message.id, pending.id);
    assert_eq!(message.status, Some(MessageStatus::Failed));
    assert_eq!(error, BackendError::new("timed out"));
    assert!(running.network.sent().is_empty());
    stop(running).await;
}

#[tokio::test]
async fn stalled_connection_times_out_while_commands_are_served() {
    let dir = tempfile::tempdir().unwrap();
    let options = CoreOptions {
        connect_timeout: Duration::from_millis(100),
        ..fast()
    };
    let mut running = start_with(
        dir.path(),
        &MemorySecretStore::default(),
        true,
        options,
        |network| network.stall_connections(true),
    );
    expect(&mut running.events, Event::Ready).await;

    send(&running, Command::LoadConversations);
    assert_eq!(
        next_event(&mut running.events).await,
        Event::Conversations(Vec::new())
    );
    expect(
        &mut running.events,
        Event::Disconnected {
            error: Some(BackendError::network("timed out connecting")),
            retry_in: RETRY,
        },
    )
    .await;

    running.network.stall_connections(false);
    assert_eq!(
        skip_disconnects(&mut running.events).await,
        Event::Connected
    );
    stop(running).await;
}

#[tokio::test]
async fn reports_receive_failure_and_keeps_serving_commands() {
    let dir = tempfile::tempdir().unwrap();
    let mut running = start_with(
        dir.path(),
        &MemorySecretStore::default(),
        true,
        fast(),
        |network| network.fail_receive(BackendError::new("unauthorized")),
    );

    expect(&mut running.events, Event::Ready).await;
    expect(
        &mut running.events,
        Event::Disconnected {
            error: Some(BackendError::new("unauthorized")),
            retry_in: RETRY,
        },
    )
    .await;
    expect(&mut running.events, Event::Connected).await;

    let chat = open_chat(&mut running, 4).await;
    send_text(&running, chat, "still here");
    pending_added(&mut running.events).await;
    assert_eq!(
        message_updated(&mut running.events).await.status,
        Some(MessageStatus::Sent)
    );
    stop(running).await;
}

#[tokio::test]
async fn reports_disconnect() {
    let (_dir, mut running) = ready().await;

    running.network.disconnect();

    expect(
        &mut running.events,
        Event::Disconnected {
            error: None,
            retry_in: RETRY,
        },
    )
    .await;
    expect(&mut running.events, Event::Connected).await;
    assert_eq!(running.network.connections(), 2);
    stop(running).await;
}

#[tokio::test]
async fn reconnect_backs_off_and_resets_once_synced() {
    let dir = tempfile::tempdir().unwrap();
    let mut running = start_with(
        dir.path(),
        &MemorySecretStore::default(),
        true,
        fast(),
        |network| {
            for _ in 0..4 {
                network.fail_receive(BackendError::new("offline"));
            }
        },
    );
    expect(&mut running.events, Event::Ready).await;

    for retry_in in [RETRY, RETRY * 2, RETRY * 4, RETRY * 4] {
        expect_disconnected(&mut running.events, retry_in).await;
    }
    expect(&mut running.events, Event::Connected).await;

    // Still backed off until the queue drains.
    running.network.disconnect();
    expect_disconnected(&mut running.events, RETRY * 4).await;
    expect(&mut running.events, Event::Connected).await;
    running.network.deliver(Incoming::QueueEmpty);
    expect(&mut running.events, Event::Synced).await;
    running.network.disconnect();
    expect_disconnected(&mut running.events, RETRY).await;
    stop(running).await;
}

#[tokio::test]
async fn reconnect_command_skips_the_wait() {
    let dir = tempfile::tempdir().unwrap();
    let slow = CoreOptions {
        reconnect_initial: Duration::from_secs(600),
        reconnect_max: Duration::from_secs(600),
        ..CoreOptions::default()
    };
    let mut running = start_with(
        dir.path(),
        &MemorySecretStore::default(),
        true,
        slow,
        |network| network.fail_receive(BackendError::new("offline")),
    );
    expect(&mut running.events, Event::Ready).await;
    expect_disconnected(&mut running.events, Duration::from_secs(600)).await;

    send(&running, Command::Reconnect);

    expect(&mut running.events, Event::Connected).await;
    stop(running).await;
}

#[tokio::test]
async fn dropping_handle_stops_core() {
    let (_dir, running) = ready().await;
    let Running {
        core, mut events, ..
    } = running;

    drop(core);

    expect(&mut events, Event::Stopped).await;
    assert_eq!(events.recv().await, None);
}

// Linking

#[tokio::test]
async fn links_after_the_phone_approves() {
    let dir = tempfile::tempdir().unwrap();
    let mut running = start(dir.path(), &MemorySecretStore::default(), false);
    expect_unlinked(&mut running.events).await;

    send(
        &running,
        Command::Link {
            device_name: "Copiale on test".into(),
        },
    );
    expect(&mut running.events, Event::LinkUrl(FAKE_LINK_URL.into())).await;
    running.phone.approve();

    expect(&mut running.events, Event::Ready).await;
    expect(&mut running.events, Event::Connected).await;
    assert!(running.phone.is_linked());
    assert_eq!(
        running.phone.device_name().as_deref(),
        Some("Copiale on test")
    );
    assert_eq!(running.network.contact_sync_requests(), 1);
    running.network.deliver(Incoming::QueueEmpty);
    expect(&mut running.events, Event::Synced).await;
    stop(running).await;
}

#[tokio::test]
async fn rejected_link_can_be_retried() {
    let dir = tempfile::tempdir().unwrap();
    let mut running = start(dir.path(), &MemorySecretStore::default(), false);
    expect_unlinked(&mut running.events).await;
    let link = || Command::Link {
        device_name: "x".into(),
    };

    send(&running, link());
    expect(&mut running.events, Event::LinkUrl(FAKE_LINK_URL.into())).await;
    running.phone.reject(BackendError::new("timed out"));
    expect(
        &mut running.events,
        Event::LinkFailed(BackendError::new("timed out")),
    )
    .await;
    expect_unlinked(&mut running.events).await;

    send(&running, link());
    expect(&mut running.events, Event::LinkUrl(FAKE_LINK_URL.into())).await;
    running.phone.approve();
    expect(&mut running.events, Event::Ready).await;
    stop(running).await;
}

#[tokio::test]
async fn cancelled_link_returns_to_unlinked() {
    let dir = tempfile::tempdir().unwrap();
    let mut running = start(dir.path(), &MemorySecretStore::default(), false);
    expect_unlinked(&mut running.events).await;

    send(
        &running,
        Command::Link {
            device_name: "x".into(),
        },
    );
    expect(&mut running.events, Event::LinkUrl(FAKE_LINK_URL.into())).await;
    send(&running, Command::CancelLink);

    expect_unlinked(&mut running.events).await;
    assert!(!running.phone.is_linked());
    stop(running).await;
}

#[tokio::test]
async fn failed_account_load_offers_linking() {
    let dir = tempfile::tempdir().unwrap();
    let (backend, _network) = fake_backend();
    let (linker, phone) = fake_linker(backend);
    phone.fail_load(BackendError::new("corrupt registration"));
    let storage = Storage::new(StoragePaths::in_dir(dir.path())).with_kdf_params(CHEAP_KDF);
    let (core, mut events) =
        copiale_core::spawn(storage, Arc::new(MemorySecretStore::default()), move || {
            linker
        })
        .unwrap();

    expect(
        &mut events,
        Event::LinkFailed(BackendError::new("corrupt registration")),
    )
    .await;
    expect_unlinked(&mut events).await;
    core.shutdown().unwrap();
}

// Storage

#[tokio::test]
async fn passphrase_database_unlocks_on_restart() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = MemorySecretStore::unavailable();

    let mut running = start(dir.path(), &secrets, true);
    expect(
        &mut running.events,
        Event::Locked {
            new_passphrase: true,
        },
    )
    .await;
    send(&running, Command::SetPassphrase(Passphrase::new("right")));
    expect(&mut running.events, Event::Ready).await;
    stop(running).await;

    let mut running = start(dir.path(), &secrets, true);
    expect(
        &mut running.events,
        Event::Locked {
            new_passphrase: false,
        },
    )
    .await;
    send(&running, Command::Unlock(Passphrase::new("wrong")));
    expect(&mut running.events, Event::WrongPassphrase).await;
    send(&running, Command::Unlock(Passphrase::new("right")));
    expect(&mut running.events, Event::Ready).await;
    stop(running).await;
}

#[tokio::test]
async fn missing_key_can_start_over() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = MemorySecretStore::default();
    let running = start(dir.path(), &secrets, true);
    stop(running).await;
    secrets.clear_secret();

    let mut running = start(dir.path(), &secrets, false);
    expect(
        &mut running.events,
        Event::StorageFailed(StorageFailure::KeyMissing),
    )
    .await;
    send(&running, Command::StartOver);

    // Fresh storage with a new key; the account must be linked again.
    expect_unlinked(&mut running.events).await;
    assert!(secrets.secret().is_some());
    stop(running).await;
}

#[tokio::test]
async fn unavailable_keychain_can_be_retried() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = MemorySecretStore::default();
    let running = start(dir.path(), &secrets, true);
    stop(running).await;
    secrets.set_available(false);

    let mut running = start(dir.path(), &secrets, true);
    let event = next_event(&mut running.events).await;
    assert!(
        matches!(
            event,
            Event::StorageFailed(StorageFailure::KeychainUnavailable(_))
        ),
        "{event:?}"
    );
    secrets.set_available(true);
    send(&running, Command::Retry);

    expect(&mut running.events, Event::Ready).await;
    stop(running).await;
}

#[tokio::test]
async fn wrong_keychain_key_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = MemorySecretStore::default();
    let running = start(dir.path(), &secrets, true);
    stop(running).await;
    secrets.put_secret(&[7; 32]);

    let mut running = start(dir.path(), &secrets, true);

    expect(
        &mut running.events,
        Event::StorageFailed(StorageFailure::WrongKey),
    )
    .await;
    stop(running).await;
}

// State handling

#[tokio::test]
async fn commands_for_other_states_are_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let mut running = start(dir.path(), &MemorySecretStore::unavailable(), false);
    expect(
        &mut running.events,
        Event::Locked {
            new_passphrase: true,
        },
    )
    .await;

    send(
        &running,
        Command::SendText {
            conversation: ConversationId::new(1),
            body: MessageBody::new("too early"),
            reply_to: None,
        },
    );
    send(
        &running,
        Command::Link {
            device_name: "too early".into(),
        },
    );
    send(&running, Command::Unlock(Passphrase::new("not this one")));
    send(&running, Command::SetPassphrase(Passphrase::new("x")));

    expect_unlinked(&mut running.events).await;
    assert!(running.network.sent().is_empty());
    assert_eq!(running.phone.device_name(), None);
    stop(running).await;
}

#[tokio::test]
async fn shutdown_while_locked_stops() {
    let dir = tempfile::tempdir().unwrap();
    let mut running = start(dir.path(), &MemorySecretStore::unavailable(), false);
    expect(
        &mut running.events,
        Event::Locked {
            new_passphrase: true,
        },
    )
    .await;

    stop(running).await;
}

#[tokio::test]
async fn shutdown_while_linking_stops() {
    let dir = tempfile::tempdir().unwrap();
    let mut running = start(dir.path(), &MemorySecretStore::default(), false);
    expect_unlinked(&mut running.events).await;
    send(
        &running,
        Command::Link {
            device_name: "x".into(),
        },
    );
    expect(&mut running.events, Event::LinkUrl(FAKE_LINK_URL.into())).await;

    stop(running).await;
}

// Groups

fn group_ref(n: u8, revision: u32) -> GroupRef {
    GroupRef {
        key: group_key(n),
        revision,
    }
}

fn group_incoming(group: GroupRef, from: u128, sent_at: u64, body: &str) -> Incoming {
    Incoming::Message(IncomingMessage {
        sender: service_id(from),
        group: Some(group),
        timestamp: Timestamp::from_millis(sent_at),
        content: Content::text(body),
        expire_timer: None,
    })
}

fn member(n: u128, name: Option<&str>) -> Member {
    Member {
        id: service_id(n),
        name: name.map(Into::into),
    }
}

fn dev_group(revision: u32) -> GroupInfo {
    GroupInfo {
        title: "Dev".into(),
        revision,
        members: vec![member(1, Some("Alice")), member(2, None)],
    }
}

/// Delivers a group message and returns its stored copy and conversation.
async fn receive_in_group(
    running: &mut Running,
    group: GroupRef,
    from: u128,
    sent_at: u64,
) -> (Message, ConversationSummary) {
    running
        .network
        .deliver(group_incoming(group, from, sent_at, "hi all"));
    let message = message_added(&mut running.events).await;
    let summary = conversation_updated(&mut running.events).await;
    (message, summary)
}

#[tokio::test]
async fn group_message_fetches_the_group_and_member_names() {
    let (_dir, mut running) = ready().await;
    running.network.set_group(group_key(1), dev_group(3));
    running.network.set_profile_name(service_id(2), "Bob");

    let (message, summary) = receive_in_group(&mut running, group_ref(1, 3), 2, 100).await;

    assert_eq!(message.direction, Direction::Incoming);
    assert_eq!(message.sender, Some(service_id(2)));
    assert_eq!(summary.peer, Peer::Group(group_key(1).id()));
    assert_eq!(summary.title, None);
    let known = conversation_updated(&mut running.events).await;
    assert_eq!(known.id, summary.id);
    assert_eq!(known.title.as_deref(), Some("Dev"));
    assert_eq!(known.members, [member(1, Some("Alice")), member(2, None)]);
    let named = conversation_updated(&mut running.events).await;
    assert_eq!(
        named.members,
        [member(1, Some("Alice")), member(2, Some("Bob"))]
    );
    assert_eq!(running.network.profile_fetches(), 1);
    stop(running).await;
}

#[tokio::test]
async fn group_is_fetched_again_only_for_a_newer_revision() {
    let (_dir, mut running) = ready().await;
    running.network.set_group(group_key(1), dev_group(3));
    receive_in_group(&mut running, group_ref(1, 3), 1, 100).await;
    conversation_updated(&mut running.events).await;
    // Bob has no profile name, so this ends the refresh.
    conversation_updated(&mut running.events).await;

    receive_in_group(&mut running, group_ref(1, 3), 1, 101).await;
    running.network.deliver(Incoming::QueueEmpty);
    expect(&mut running.events, Event::Synced).await;

    running.network.set_group(
        group_key(1),
        GroupInfo {
            title: "Dev team".into(),
            ..dev_group(4)
        },
    );
    receive_in_group(&mut running, group_ref(1, 4), 1, 102).await;
    let renamed = conversation_updated(&mut running.events).await;
    assert_eq!(renamed.title.as_deref(), Some("Dev team"));
    stop(running).await;
}

#[tokio::test]
async fn same_time_from_two_members_is_two_messages() {
    let (_dir, mut running) = ready().await;

    let (first, _) = receive_in_group(&mut running, group_ref(1, 0), 1, 100).await;
    let (second, _) = receive_in_group(&mut running, group_ref(1, 0), 2, 100).await;

    assert_eq!(first.conversation, second.conversation);
    assert_ne!(first.id, second.id);
    stop(running).await;
}

#[tokio::test]
async fn text_sent_to_a_group_goes_to_the_group() {
    let (_dir, mut running) = ready().await;
    running.network.set_group(group_key(1), dev_group(3));
    let (_, summary) = receive_in_group(&mut running, group_ref(1, 3), 1, 100).await;
    conversation_updated(&mut running.events).await;
    conversation_updated(&mut running.events).await;

    send_text(&running, summary.id, "hello group");

    let pending = pending_added(&mut running.events).await;
    assert_eq!(pending.conversation, summary.id);
    assert_eq!(pending.sender, None);
    message_updated(&mut running.events).await;
    assert_eq!(
        running.network.sent(),
        [SentText {
            destination: Destination::Group(group_ref(1, 3)),
            outgoing: text("hello group"),
            timestamp: pending.sent_at,
        }]
    );
    stop(running).await;
}

#[tokio::test]
async fn group_message_sent_from_the_phone_is_stored_as_outgoing() {
    let (_dir, mut running) = ready().await;

    running.network.deliver(Incoming::Sent(SentMessage {
        destination: Destination::Group(group_ref(1, 0)),
        timestamp: Timestamp::from_millis(200),
        content: Content::text("from my phone"),
        expire_timer: None,
    }));

    let message = message_added(&mut running.events).await;
    assert_eq!(message.direction, Direction::Outgoing);
    assert_eq!(message.sender, None);
    let summary = conversation_updated(&mut running.events).await;
    assert_eq!(summary.peer, Peer::Group(group_key(1).id()));
    stop(running).await;
}

#[tokio::test]
async fn unknown_group_keeps_working_without_a_title() {
    let (_dir, mut running) = ready().await;

    let (_, summary) = receive_in_group(&mut running, group_ref(1, 0), 1, 100).await;
    running.network.deliver(Incoming::QueueEmpty);

    expect(&mut running.events, Event::Synced).await;
    assert_eq!(summary.title, None);
    stop(running).await;
}

#[tokio::test]
async fn synced_contacts_refresh_group_members() {
    let (_dir, mut running) = ready().await;
    running.network.set_group(group_key(1), dev_group(3));
    receive_in_group(&mut running, group_ref(1, 3), 1, 100).await;
    conversation_updated(&mut running.events).await;
    conversation_updated(&mut running.events).await;
    running.network.set_group(
        group_key(1),
        GroupInfo {
            members: vec![member(1, Some("Alice")), member(2, Some("Bob B."))],
            ..dev_group(3)
        },
    );

    running.network.deliver(Incoming::ContactsSynced);

    assert!(matches!(
        next_event(&mut running.events).await,
        Event::Conversations(_)
    ));
    assert!(matches!(
        next_event(&mut running.events).await,
        Event::Contacts(_)
    ));
    let refreshed = conversation_updated(&mut running.events).await;
    assert_eq!(refreshed.members[1], member(2, Some("Bob B.")));
    stop(running).await;
}

#[tokio::test]
async fn system_lines_and_reactions_arrive_read_but_calls_do_not() {
    let (_dir, mut running) = ready().await;
    let deliver = |content: Content, sent_at: u64| {
        Incoming::Message(IncomingMessage {
            sender: service_id(1),
            group: Some(group_ref(1, 0)),
            timestamp: Timestamp::from_millis(sent_at),
            content,
            expire_timer: None,
        })
    };

    running.network.deliver(deliver(
        Content::GroupChanged(vec![GroupUpdate::Title("Dev".into())]),
        100,
    ));
    let update = message_added(&mut running.events).await;
    assert!(update.read);
    assert_eq!(conversation_updated(&mut running.events).await.unread, 0);

    running.network.deliver(deliver(
        Content::Reaction {
            emoji: "👍".into()
        },
        101,
    ));
    assert!(message_added(&mut running.events).await.read);
    assert_eq!(conversation_updated(&mut running.events).await.unread, 0);

    running
        .network
        .deliver(deliver(Content::Call { video: false }, 102));
    assert!(!message_added(&mut running.events).await.read);
    assert_eq!(conversation_updated(&mut running.events).await.unread, 1);
    stop(running).await;
}

// Receipts and typing

/// Delivers everything queued on the connection, then returns the events
/// it caused.
async fn flush(running: &mut Running) -> Vec<Event> {
    running.network.deliver(Incoming::QueueEmpty);
    let mut events = Vec::new();
    loop {
        match next_event(&mut running.events).await {
            Event::Synced => break events,
            event => events.push(event),
        }
    }
}

/// Connected, with the phone's settings applied.
async fn ready_with(read_receipts: bool, typing_indicators: bool) -> (TempDir, Running) {
    let (dir, mut running) = ready().await;
    running.network.deliver(Incoming::Settings {
        read_receipts: Some(read_receipts),
        typing_indicators: Some(typing_indicators),
    });
    flush(&mut running).await;
    (dir, running)
}

#[tokio::test]
async fn settings_are_requested_until_known() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = MemorySecretStore::default();
    let mut running = start(dir.path(), &secrets, true);
    expect(&mut running.events, Event::Ready).await;
    expect(&mut running.events, Event::Connected).await;
    assert_eq!(running.network.settings_sync_requests(), 1);
    running.network.deliver(Incoming::Settings {
        read_receipts: Some(true),
        typing_indicators: None,
    });
    flush(&mut running).await;
    stop(running).await;

    let mut running = start(dir.path(), &secrets, true);
    expect(&mut running.events, Event::Ready).await;
    expect(&mut running.events, Event::Connected).await;
    assert_eq!(running.network.settings_sync_requests(), 0);
    stop(running).await;
}

#[tokio::test]
async fn unknown_settings_send_and_show_nothing() {
    let (_dir, mut running) = ready().await;
    running.network.deliver(incoming(1, 100, "hi"));
    let message = message_added(&mut running.events).await;
    conversation_updated(&mut running.events).await;

    send(&running, Command::MarkRead(message.conversation));
    conversation_updated(&mut running.events).await;
    send(
        &running,
        Command::Typing {
            conversation: message.conversation,
            typing: true,
        },
    );
    running.network.deliver(Incoming::Typing {
        sender: service_id(1),
        group: None,
        typing: true,
    });

    assert!(flush(&mut running).await.is_empty());
    assert!(running.network.read_receipts().is_empty());
    assert!(running.network.typing().is_empty());
    assert_eq!(
        running.network.read_syncs(),
        [vec![(service_id(1), Timestamp::from_millis(100))]],
        "our own devices always learn what we read"
    );
    stop(running).await;
}

#[tokio::test]
async fn each_setting_combination() {
    for (read_receipts, typing_indicators) in
        [(false, false), (true, false), (false, true), (true, true)]
    {
        let (_dir, mut running) = ready_with(read_receipts, typing_indicators).await;
        let label = format!("read receipts {read_receipts}, typing {typing_indicators}");

        // Viewing sends read receipts only if enabled.
        running.network.deliver(incoming(1, 100, "hi"));
        running.network.deliver(incoming(1, 101, "there"));
        let message = message_added(&mut running.events).await;
        conversation_updated(&mut running.events).await;
        message_added(&mut running.events).await;
        conversation_updated(&mut running.events).await;
        let chat = message.conversation;
        send(&running, Command::MarkRead(chat));
        conversation_updated(&mut running.events).await;
        let expected = read_receipts.then(|| {
            (
                service_id(1),
                vec![Timestamp::from_millis(100), Timestamp::from_millis(101)],
            )
        });
        assert_eq!(
            running.network.read_receipts(),
            Vec::from_iter(expected),
            "{label}"
        );

        // Their receipts: delivered always, read only if enabled.
        send_text(&running, chat, "reply");
        let pending = pending_added(&mut running.events).await;
        message_updated(&mut running.events).await;
        for kind in [ReceiptKind::Delivered, ReceiptKind::Read] {
            running.network.deliver(Incoming::Receipt {
                sender: service_id(1),
                kind,
                timestamps: vec![pending.sent_at],
            });
        }
        let statuses: Vec<_> = flush(&mut running)
            .await
            .into_iter()
            .filter_map(|event| match event {
                Event::MessageUpdated(message) => message.status,
                _ => None,
            })
            .collect();
        let mut expected = vec![MessageStatus::Delivered];
        if read_receipts {
            expected.push(MessageStatus::Read);
        }
        assert_eq!(statuses, expected, "{label}");

        // Typing, both ways, only if enabled.
        running.network.deliver(Incoming::Typing {
            sender: service_id(1),
            group: None,
            typing: true,
        });
        let shown = flush(&mut running).await;
        let expected = typing_indicators.then_some(Event::Typing {
            conversation: chat,
            member: service_id(1),
            typing: true,
        });
        assert_eq!(shown, Vec::from_iter(expected), "{label}");
        for typing in [true, true, false, false] {
            send(
                &running,
                Command::Typing {
                    conversation: chat,
                    typing,
                },
            );
        }
        sync(&mut running).await;
        let destination = Destination::Contact(service_id(1));
        let expected = if typing_indicators {
            vec![(destination, true), (destination, false)]
        } else {
            Vec::new()
        };
        assert_eq!(running.network.typing(), expected, "{label}");
        stop(running).await;
    }
}

#[tokio::test]
async fn typing_from_strangers_is_ignored() {
    let (_dir, mut running) = ready_with(true, true).await;

    running.network.deliver(Incoming::Typing {
        sender: service_id(9),
        group: None,
        typing: true,
    });

    assert!(flush(&mut running).await.is_empty());
    stop(running).await;
}

#[tokio::test]
async fn reads_on_the_phone_clear_unread_here() {
    let (_dir, mut running) = ready().await;
    for sent_at in [100, 101] {
        running.network.deliver(incoming(1, sent_at, "hi"));
        message_added(&mut running.events).await;
        conversation_updated(&mut running.events).await;
    }

    running.network.deliver(Incoming::ReadElsewhere {
        read: vec![(service_id(1), Timestamp::from_millis(101))],
        at: Timestamp::now(),
    });

    let summary = conversation_updated(&mut running.events).await;
    assert_eq!(summary.unread, 0);
    assert!(running.network.read_syncs().is_empty(), "not echoed back");
    stop(running).await;
}

#[tokio::test]
async fn note_to_self_gets_no_typing_or_read_receipts() {
    let (_dir, mut running) = ready_with(true, true).await;
    let note = open_chat(&mut running, OWN_ID).await;
    running.network.deliver(incoming(OWN_ID, 100, "to me"));
    message_added(&mut running.events).await;
    conversation_updated(&mut running.events).await;

    send(&running, Command::MarkRead(note));
    conversation_updated(&mut running.events).await;
    send(
        &running,
        Command::Typing {
            conversation: note,
            typing: true,
        },
    );
    sync(&mut running).await;

    assert!(running.network.read_receipts().is_empty());
    assert!(running.network.typing().is_empty());
    stop(running).await;
}

// Edits, deletes and quotes

fn changed(from: u128, target: u64, change: Change) -> Incoming {
    Incoming::Change(MessageChange {
        destination: Destination::Contact(service_id(from)),
        author: Some(service_id(from)),
        target: Timestamp::from_millis(target),
        change,
    })
}

fn edit(body: &str, at: u64) -> Change {
    Change::Edit {
        content: Content::text(body),
        at: Timestamp::from_millis(at),
    }
}

#[tokio::test]
async fn edits_and_deletes_update_the_message() {
    let (_dir, mut running) = ready().await;
    running.network.deliver(incoming(1, 100, "teh"));
    let original = message_added(&mut running.events).await;
    conversation_updated(&mut running.events).await;

    running.network.deliver(changed(1, 100, edit("the", 150)));
    let edited = message_updated(&mut running.events).await;
    assert_eq!(edited.id, original.id);
    assert_eq!(edited.content, Content::text("the"));
    assert!(edited.edited);
    conversation_updated(&mut running.events).await;

    running.network.deliver(changed(1, 100, Change::Delete));
    let deleted = message_updated(&mut running.events).await;
    assert_eq!(deleted.content, Content::Deleted);
    stop(running).await;
}

#[tokio::test]
async fn changes_before_their_message_apply_on_arrival() {
    let (_dir, mut running) = ready().await;

    running.network.deliver(changed(1, 100, edit("final", 150)));
    assert!(flush(&mut running).await.is_empty());
    running.network.deliver(incoming(1, 100, "draft"));

    let message = message_added(&mut running.events).await;
    assert_eq!(message.content, Content::text("final"));
    assert!(message.edited);
    stop(running).await;
}

#[tokio::test]
async fn quotes_of_our_messages_name_no_author() {
    let (_dir, mut running) = ready().await;
    let reply = |author: u128| Content::Text {
        body: MessageBody::new("yes"),
        attachments: Vec::new(),
        quote: Some(Quote {
            author: Some(service_id(author)),
            sent_at: Timestamp::from_millis(50),
            text: "really?".into(),
            attachment: None,
        }),
    };

    for (sent_at, author) in [(100, OWN_ID), (101, 2)] {
        running.network.deliver(Incoming::Message(IncomingMessage {
            sender: service_id(1),
            group: None,
            timestamp: Timestamp::from_millis(sent_at),
            content: reply(author),
            expire_timer: None,
        }));
    }

    let ours = message_added(&mut running.events).await;
    let Content::Text {
        quote: Some(quote), ..
    } = ours.content
    else {
        panic!("expected a quote");
    };
    assert_eq!(quote.author, None);
    conversation_updated(&mut running.events).await;
    let theirs = message_added(&mut running.events).await;
    let Content::Text {
        quote: Some(quote), ..
    } = theirs.content
    else {
        panic!("expected a quote");
    };
    assert_eq!(quote.author, Some(service_id(2)));
    stop(running).await;
}

#[tokio::test]
async fn deletes_for_me_remove_messages_and_conversations() {
    let (_dir, mut running) = ready().await;
    running.network.deliver(incoming(1, 100, "theirs"));
    let theirs = message_added(&mut running.events).await;
    conversation_updated(&mut running.events).await;
    running.network.deliver(Incoming::Sent(SentMessage {
        destination: Destination::Contact(service_id(1)),
        timestamp: Timestamp::from_millis(101),
        content: Content::text("mine"),
        expire_timer: None,
    }));
    let mine = message_added(&mut running.events).await;
    conversation_updated(&mut running.events).await;
    let chat = ChatRef::Contact(service_id(1));

    running.network.deliver(Incoming::DeletedForMe {
        messages: vec![
            MessageRef {
                chat,
                author: Some(service_id(1)),
                sent_at: Timestamp::from_millis(100),
            },
            // Our own messages name this account.
            MessageRef {
                chat,
                author: Some(service_id(OWN_ID)),
                sent_at: Timestamp::from_millis(101),
            },
        ],
        conversations: Vec::new(),
    });

    for expected in [&theirs, &mine] {
        assert_eq!(
            next_event(&mut running.events).await,
            Event::MessageRemoved {
                conversation: expected.conversation,
                id: expected.id,
            }
        );
    }
    let summary = conversation_updated(&mut running.events).await;
    assert_eq!(summary.last_message, None);

    running.network.deliver(Incoming::DeletedForMe {
        messages: Vec::new(),
        conversations: vec![chat],
    });
    assert_eq!(
        next_event(&mut running.events).await,
        Event::ConversationRemoved(theirs.conversation)
    );
    stop(running).await;
}

// Own-message actions

/// A text of ours, sent; returns it as sent.
async fn sent_text(running: &mut Running, chat: ConversationId, body: &str) -> Message {
    send_text(running, chat, body);
    pending_added(&mut running.events).await;
    message_updated(&mut running.events).await
}

#[tokio::test]
async fn replies_quote_the_message() {
    let (_dir, mut running) = ready().await;
    running.network.deliver(incoming(1, 100, "ship it?"));
    let question = message_added(&mut running.events).await;
    conversation_updated(&mut running.events).await;

    send(
        &running,
        Command::SendText {
            conversation: question.conversation,
            body: MessageBody::new("yes"),
            reply_to: Some(question.id),
        },
    );

    let reply = pending_added(&mut running.events).await;
    let quote = Quote {
        author: Some(service_id(1)),
        sent_at: Timestamp::from_millis(100),
        text: "ship it?".into(),
        attachment: None,
    };
    assert_eq!(
        reply.content,
        Content::Text {
            body: MessageBody::new("yes"),
            attachments: Vec::new(),
            quote: Some(quote.clone()),
        }
    );
    message_updated(&mut running.events).await;
    assert_eq!(
        running.network.sent()[0].outgoing,
        Outgoing::Text {
            body: MessageBody::new("yes"),
            quote: Some(quote),
        }
    );
    stop(running).await;
}

#[tokio::test]
async fn own_messages_can_be_edited_and_deleted_for_everyone() {
    let (_dir, mut running) = ready().await;
    let chat = open_chat(&mut running, 2).await;
    let original = sent_text(&mut running, chat, "teh").await;

    send(
        &running,
        Command::Edit {
            id: original.id,
            body: MessageBody::new("the"),
        },
    );
    let edited = message_updated(&mut running.events).await;
    assert_eq!(edited.content, Content::text("the"));
    assert!(edited.edited);
    conversation_updated(&mut running.events).await;

    send(
        &running,
        Command::Delete {
            id: original.id,
            everyone: true,
        },
    );
    let deleted = message_updated(&mut running.events).await;
    assert_eq!(deleted.content, Content::Deleted);
    conversation_updated(&mut running.events).await;
    sync(&mut running).await;

    let sent: Vec<_> = running
        .network
        .sent()
        .into_iter()
        .map(|sent| sent.outgoing)
        .collect();
    assert_eq!(
        sent,
        [
            text("teh"),
            Outgoing::Edit {
                target: original.sent_at,
                body: MessageBody::new("the"),
                quote: None,
            },
            Outgoing::Delete {
                target: original.sent_at,
            },
        ]
    );
    stop(running).await;
}

#[tokio::test]
async fn only_recent_own_sent_messages_change_for_everyone() {
    let (_dir, mut running) = ready().await;
    running.network.deliver(incoming(1, 100, "theirs"));
    let theirs = message_added(&mut running.events).await;
    conversation_updated(&mut running.events).await;
    // Sent from the phone long ago.
    running.network.deliver(Incoming::Sent(SentMessage {
        destination: Destination::Contact(service_id(1)),
        timestamp: Timestamp::from_millis(1_000),
        content: Content::text("ancient"),
        expire_timer: None,
    }));
    let old = message_added(&mut running.events).await;
    conversation_updated(&mut running.events).await;

    for id in [theirs.id, old.id] {
        send(
            &running,
            Command::Edit {
                id,
                body: MessageBody::new("changed"),
            },
        );
        send(&running, Command::Delete { id, everyone: true });
    }
    sync(&mut running).await;

    assert!(running.network.sent().is_empty());
    stop(running).await;
}

#[tokio::test]
async fn deleting_for_me_removes_and_syncs() {
    let (_dir, mut running) = ready().await;
    running.network.deliver(incoming(1, 100, "theirs"));
    let theirs = message_added(&mut running.events).await;
    conversation_updated(&mut running.events).await;

    send(
        &running,
        Command::Delete {
            id: theirs.id,
            everyone: false,
        },
    );

    assert_eq!(
        next_event(&mut running.events).await,
        Event::MessageRemoved {
            conversation: theirs.conversation,
            id: theirs.id,
        }
    );
    conversation_updated(&mut running.events).await;
    assert_eq!(
        running.network.deleted_for_me(),
        [(
            Destination::Contact(service_id(1)),
            vec![(Some(service_id(1)), Timestamp::from_millis(100))]
        )]
    );
    stop(running).await;
}

#[tokio::test]
async fn failed_changes_are_reported() {
    let (_dir, mut running) = ready().await;
    let chat = open_chat(&mut running, 2).await;
    let original = sent_text(&mut running, chat, "teh").await;
    let error = BackendError::new("rejected");
    running.network.fail_next_send(error.clone());

    send(
        &running,
        Command::Edit {
            id: original.id,
            body: MessageBody::new("the"),
        },
    );

    message_updated(&mut running.events).await;
    conversation_updated(&mut running.events).await;
    assert_eq!(
        next_event(&mut running.events).await,
        Event::ChangeFailed {
            id: original.id,
            error,
        }
    );
    stop(running).await;
}

// Disappearing messages

const NOW: u64 = 1_800_000_000_000;

fn timed(options: CoreOptions, clock: &ManualClock) -> CoreOptions {
    CoreOptions {
        clock: Arc::new(clock.clone()),
        ..options
    }
}

/// Connected, on a clock at [`NOW`].
async fn ready_at(clock: &ManualClock) -> (TempDir, Running) {
    let dir = tempfile::tempdir().unwrap();
    let options = timed(fast(), clock);
    let secrets = MemorySecretStore::default();
    let mut running = start_with(dir.path(), &secrets, true, options, |_| {});
    expect(&mut running.events, Event::Ready).await;
    expect(&mut running.events, Event::Connected).await;
    (dir, running)
}

fn timed_incoming(from: u128, sent_at: u64, body: &str, seconds: u32) -> Incoming {
    Incoming::Message(IncomingMessage {
        sender: service_id(from),
        group: None,
        timestamp: Timestamp::from_millis(sent_at),
        content: Content::text(body),
        expire_timer: Some(seconds),
    })
}

fn expires_at(message: &Message) -> Option<u64> {
    message.expiry?.at.map(|at| at.as_millis())
}

async fn expect_removed(events: &mut Events, message: &Message) {
    expect(
        events,
        Event::MessageRemoved {
            conversation: message.conversation,
            id: message.id,
        },
    )
    .await;
    let summary = conversation_updated(events).await;
    assert_eq!(summary.id, message.conversation);
}

#[tokio::test]
async fn incoming_messages_disappear_after_being_read() {
    let clock = ManualClock::new(Timestamp::from_millis(NOW));
    let (_dir, mut running) = ready_at(&clock).await;
    running
        .network
        .deliver(timed_incoming(1, NOW - 1000, "soon gone", 60));
    let message = message_added(&mut running.events).await;
    conversation_updated(&mut running.events).await;
    assert_eq!(
        message.expiry,
        Some(Expiry {
            timer: 60,
            at: None
        })
    );

    // Unread messages wait.
    clock.advance(Duration::from_secs(3600));
    sync(&mut running).await;
    assert_eq!(
        load_messages(&mut running, message.conversation)
            .await
            .len(),
        1
    );

    send(&running, Command::MarkRead(message.conversation));
    let started = message_updated(&mut running.events).await;
    assert_eq!(expires_at(&started), Some(NOW + 3_660_000));
    conversation_updated(&mut running.events).await;

    clock.advance(Duration::from_secs(59));
    sync(&mut running).await;
    clock.advance(Duration::from_secs(1));
    expect_removed(&mut running.events, &message).await;
    assert!(
        load_messages(&mut running, message.conversation)
            .await
            .is_empty()
    );
    stop(running).await;
}

#[tokio::test]
async fn own_messages_disappear_after_being_sent() {
    let clock = ManualClock::new(Timestamp::from_millis(NOW));
    let (_dir, mut running) = ready_at(&clock).await;
    let peer = Destination::Contact(service_id(2));
    running.network.set_expire_timer(peer, Some(30));
    let chat = open_chat(&mut running, 2).await;
    running.network.pause_sends();

    send_text(&running, chat, "brief");
    let pending = pending_added(&mut running.events).await;
    assert_eq!(
        pending.expiry,
        Some(Expiry {
            timer: 30,
            at: None
        })
    );
    clock.advance(Duration::from_secs(10));
    running.network.resume_sends();
    let sent = message_updated(&mut running.events).await;
    assert_eq!(sent.status, Some(MessageStatus::Sent));
    assert_eq!(expires_at(&sent), Some(NOW + 40_000));

    clock.advance(Duration::from_secs(30));
    expect_removed(&mut running.events, &pending).await;

    running.network.set_expire_timer(peer, None);
    send_text(&running, chat, "stays");
    assert_eq!(pending_added(&mut running.events).await.expiry, None);
    stop(running).await;
}

#[tokio::test]
async fn messages_from_the_phone_count_down_from_their_sending() {
    let clock = ManualClock::new(Timestamp::from_millis(NOW));
    let (_dir, mut running) = ready_at(&clock).await;
    let from_phone = |sent_at| {
        Incoming::Sent(SentMessage {
            destination: Destination::Contact(service_id(2)),
            timestamp: Timestamp::from_millis(sent_at),
            content: Content::text("from phone"),
            expire_timer: Some(60),
        })
    };

    // Long gone on the phone: never shown.
    running.network.deliver(from_phone(NOW - 61_000));
    running.network.deliver(from_phone(NOW - 1_000));
    let message = message_added(&mut running.events).await;
    assert_eq!(message.sent_at, Timestamp::from_millis(NOW - 1_000));
    assert_eq!(expires_at(&message), Some(NOW + 59_000));
    conversation_updated(&mut running.events).await;

    clock.advance(Duration::from_secs(59));
    expect_removed(&mut running.events, &message).await;
    stop(running).await;
}

#[tokio::test]
async fn reads_on_the_phone_start_the_countdown_when_read_there() {
    let clock = ManualClock::new(Timestamp::from_millis(NOW));
    let (_dir, mut running) = ready_at(&clock).await;
    running
        .network
        .deliver(timed_incoming(1, NOW - 120_000, "read", 60));
    let message = message_added(&mut running.events).await;
    conversation_updated(&mut running.events).await;

    // Read on the phone 90 s ago: already overdue here.
    running.network.deliver(Incoming::ReadElsewhere {
        read: vec![(service_id(1), message.sent_at)],
        at: Timestamp::from_millis(NOW - 90_000),
    });
    let started = message_updated(&mut running.events).await;
    assert_eq!(expires_at(&started), Some(NOW - 30_000));
    conversation_updated(&mut running.events).await;
    expect_removed(&mut running.events, &message).await;
    stop(running).await;
}

#[tokio::test]
async fn overdue_messages_are_deleted_on_start() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = MemorySecretStore::default();
    let clock = ManualClock::new(Timestamp::from_millis(NOW));
    let mut running = start_with(dir.path(), &secrets, true, timed(fast(), &clock), |_| {});
    expect(&mut running.events, Event::Ready).await;
    expect(&mut running.events, Event::Connected).await;
    running.network.deliver(timed_incoming(1, NOW, "gone", 60));
    let gone = message_added(&mut running.events).await;
    conversation_updated(&mut running.events).await;
    send(&running, Command::MarkRead(gone.conversation));
    message_updated(&mut running.events).await;
    conversation_updated(&mut running.events).await;
    // Unread, so its countdown hasn't started.
    running
        .network
        .deliver(timed_incoming(1, NOW + 1, "later", 60));
    let later = message_added(&mut running.events).await;
    stop(running).await;

    clock.advance(Duration::from_secs(60));
    let mut running = start_with(dir.path(), &secrets, true, timed(fast(), &clock), |_| {});
    expect(&mut running.events, Event::Ready).await;
    expect_removed(&mut running.events, &gone).await;
    let left = load_messages(&mut running, gone.conversation).await;
    assert_eq!(left.iter().map(|m| m.id).collect::<Vec<_>>(), [later.id]);
    stop(running).await;
}

#[tokio::test]
async fn timer_changes_stay_in_the_log() {
    let clock = ManualClock::new(Timestamp::from_millis(NOW));
    let (_dir, mut running) = ready_at(&clock).await;
    running.network.deliver(Incoming::Message(IncomingMessage {
        sender: service_id(1),
        group: None,
        timestamp: Timestamp::from_millis(NOW),
        content: Content::TimerChanged(Some(60)),
        expire_timer: Some(60),
    }));
    let change = message_added(&mut running.events).await;
    assert_eq!(change.expiry, None);
    stop(running).await;
}
