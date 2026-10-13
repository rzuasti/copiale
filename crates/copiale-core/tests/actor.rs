//! Core actor against the fake backend, fake linker and in-memory keychain,
//! with real encrypted storage in a temp dir.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use copiale_core::storage::{KdfParams, Passphrase, Storage, StoragePaths};
use copiale_core::{
    BackendError, Command, ConversationSummary, CoreHandle, CoreOptions, Direction, Event, Events,
    Incoming, IncomingMessage, Message, MessageBody, MessagePreview, Peer, SentMessage,
    StorageFailure, Timestamp,
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

fn fast_reconnect() -> CoreOptions {
    CoreOptions {
        reconnect_initial: RETRY,
        reconnect_max: RETRY * 4,
    }
}

struct Running {
    core: CoreHandle,
    events: Events,
    network: FakeNetwork,
    phone: FakePhone,
}

fn start(dir: &Path, secrets: &MemorySecretStore, linked: bool) -> Running {
    start_with(dir, secrets, linked, fast_reconnect(), |_| {})
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

#[tokio::test]
async fn sends_text_and_reports_sent() {
    let (_dir, mut running) = ready().await;
    let body = MessageBody::new("hi there");

    send(
        &running,
        Command::SendText {
            recipient: service_id(2),
            body: body.clone(),
        },
    );

    let Event::MessageSent {
        recipient,
        timestamp,
    } = next_event(&mut running.events).await
    else {
        panic!("expected MessageSent");
    };
    assert_eq!(recipient, service_id(2));
    assert_eq!(
        running.network.sent(),
        vec![SentText {
            recipient,
            body,
            timestamp,
        }]
    );
    stop(running).await;
}

#[tokio::test]
async fn reports_send_failure() {
    let (_dir, mut running) = ready().await;
    running
        .network
        .fail_next_send(BackendError::new("rate limited"));

    send(
        &running,
        Command::SendText {
            recipient: service_id(3),
            body: MessageBody::new("nope"),
        },
    );

    let Event::SendFailed {
        recipient, error, ..
    } = next_event(&mut running.events).await
    else {
        panic!("expected SendFailed");
    };
    assert_eq!(recipient, service_id(3));
    assert_eq!(error, BackendError::new("rate limited"));
    assert!(running.network.sent().is_empty());
    stop(running).await;
}

#[tokio::test]
async fn reports_receive_failure_and_keeps_serving_commands() {
    let dir = tempfile::tempdir().unwrap();
    let mut running = start_with(
        dir.path(),
        &MemorySecretStore::default(),
        true,
        fast_reconnect(),
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

    send(
        &running,
        Command::SendText {
            recipient: service_id(4),
            body: MessageBody::new("still here"),
        },
    );
    assert!(matches!(
        next_event(&mut running.events).await,
        Event::MessageSent { .. }
    ));
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
        fast_reconnect(),
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
