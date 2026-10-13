//! Core actor against the fake backend, fake linker and in-memory keychain,
//! with real encrypted storage in a temp dir.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use copiale_core::storage::{KdfParams, Passphrase, Storage, StoragePaths};
use copiale_core::{
    BackendError, Command, ConversationId, ConversationSummary, CoreHandle, CoreOptions, Direction,
    Event, Events, Incoming, IncomingMessage, Message, MessageBody, MessagePreview, MessageStatus,
    Peer, SentMessage, StorageFailure, Timestamp,
};
use copiale_testkit::{
    FAKE_LINK_URL, FakeNetwork, FakePhone, MemorySecretStore, SentText, fake_backend, fake_linker,
    service_id,
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
        timestamp: Timestamp::from_millis(sent_at),
        body: MessageBody::new(body),
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
    assert_eq!(message.body, MessageBody::new("hello"));
    assert!(!message.read);
    let summary = conversation_updated(&mut running.events).await;
    assert_eq!(summary.id, message.conversation);
    assert_eq!(summary.peer, Peer::Contact(service_id(1)));
    assert_eq!(summary.unread, 1);
    assert_eq!(
        summary.last_message,
        Some(MessagePreview {
            direction: Direction::Incoming,
            body: MessageBody::new("hello"),
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
        recipient: service_id(2),
        timestamp: Timestamp::from_millis(200),
        body: MessageBody::new("from my phone"),
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
        .map(|m| m.body.as_str().to_string())
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
    stop(running).await;
}

fn send_text(running: &Running, to: u128, body: &str) {
    send(
        running,
        Command::SendText {
            recipient: service_id(to),
            body: MessageBody::new(body),
        },
    );
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

    send_text(&running, 2, "hi there");

    let pending = message_added(&mut running.events).await;
    assert_eq!(pending.direction, Direction::Outgoing);
    assert_eq!(pending.status, Some(MessageStatus::Pending));
    assert_eq!(pending.body, MessageBody::new("hi there"));
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
            recipient: service_id(2),
            body: MessageBody::new("hi there"),
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

    send_text(&running, 3, "try again");
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
    send_text(&running, 3, "fine");
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

    send_text(&running, 2, "slow");
    let pending = pending_added(&mut running.events).await;
    running.network.deliver(incoming(1, 100, "meanwhile"));

    let received = message_added(&mut running.events).await;
    assert_eq!(received.body, MessageBody::new("meanwhile"));
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

    for body in ["first", "second", "third"] {
        send_text(&running, 2, body);
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
        .map(|sent| sent.body.as_str().to_string())
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

    send_text(&running, 2, "fails");
    send_text(&running, 2, "works");
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

    send_text(&running, 2, "  \n ");
    send(&running, Command::LoadConversations);

    let Event::Conversations(conversations) = next_event(&mut running.events).await else {
        panic!("expected Conversations");
    };
    assert!(conversations.is_empty());
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
    send_text(&running, 2, "interrupted");
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

    send_text(&running, 2, "later");
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
    send_text(&running, 2, "stuck");
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
            recipient: service_id(2),
            body: MessageBody::new("stuck"),
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

    send_text(&running, 2, "again");
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

    send_text(&running, 2, "too late");
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

    send_text(&running, 4, "still here");
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
            recipient: service_id(1),
            body: MessageBody::new("too early"),
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
