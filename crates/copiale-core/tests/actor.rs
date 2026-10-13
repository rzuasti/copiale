//! Core actor against the fake backend, fake linker and in-memory keychain,
//! with real encrypted storage in a temp dir.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use copiale_core::storage::{KdfParams, Passphrase, Storage, StoragePaths};
use copiale_core::{
    BackendError, Command, CoreHandle, Event, Events, Incoming, IncomingMessage, MessageBody,
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

struct Running {
    core: CoreHandle,
    events: Events,
    network: FakeNetwork,
    phone: FakePhone,
}

fn start(dir: &Path, secrets: &MemorySecretStore, linked: bool) -> Running {
    let (backend, network) = fake_backend();
    let (linker, phone) = fake_linker(backend);
    if linked {
        phone.set_linked();
    }
    let storage = Storage::new(StoragePaths::in_dir(dir)).with_kdf_params(CHEAP_KDF);
    let (core, events) =
        copiale_core::spawn(storage, Arc::new(secrets.clone()), move || linker).unwrap();
    Running {
        core,
        events,
        network,
        phone,
    }
}

/// A linked account with a keychain, past [`Event::Ready`].
async fn ready() -> (TempDir, Running) {
    let dir = tempfile::tempdir().unwrap();
    let mut running = start(dir.path(), &MemorySecretStore::default(), true);
    expect(&mut running.events, Event::Ready).await;
    (dir, running)
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
async fn forwards_incoming_messages_and_queue_empty() {
    let (_dir, mut running) = ready().await;
    let message = IncomingMessage {
        sender: service_id(1),
        timestamp: Timestamp::from_millis(1_700_000_000_000),
        body: MessageBody::new("hello"),
    };

    running.network.deliver(Incoming::Message(message.clone()));
    running.network.deliver(Incoming::QueueEmpty);

    expect(&mut running.events, Event::MessageReceived(message)).await;
    expect(&mut running.events, Event::Synced).await;
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
    let mut running = start(dir.path(), &MemorySecretStore::default(), true);
    running
        .network
        .fail_receive(BackendError::new("unauthorized"));

    expect(&mut running.events, Event::Ready).await;
    expect(
        &mut running.events,
        Event::Disconnected {
            error: Some(BackendError::new("unauthorized")),
        },
    )
    .await;

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

    expect(&mut running.events, Event::Disconnected { error: None }).await;
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
    assert!(running.phone.is_linked());
    assert_eq!(
        running.phone.device_name().as_deref(),
        Some("Copiale on test")
    );
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
