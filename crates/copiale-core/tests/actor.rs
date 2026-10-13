//! Core actor round trips against the fake backend.

use std::time::Duration;

use copiale_core::{
    BackendError, Command, CoreHandle, Event, Events, Incoming, IncomingMessage, MessageBody,
    Timestamp,
};
use copiale_testkit::{FakeNetwork, SentText, fake_backend, service_id};

fn start() -> (CoreHandle, Events, FakeNetwork) {
    let (backend, network) = fake_backend();
    let (core, events) = copiale_core::spawn(move || backend).expect("spawn core");
    (core, events, network)
}

async fn next_event(events: &mut Events) -> Event {
    tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("event within 5 s")
        .expect("events still open")
}

#[tokio::test]
async fn forwards_incoming_messages_and_queue_empty() {
    let (_core, mut events, network) = start();
    let message = IncomingMessage {
        sender: service_id(1),
        timestamp: Timestamp::from_millis(1_700_000_000_000),
        body: MessageBody::new("hello"),
    };

    network.deliver(Incoming::Message(message.clone()));
    network.deliver(Incoming::QueueEmpty);

    assert_eq!(
        next_event(&mut events).await,
        Event::MessageReceived(message)
    );
    assert_eq!(next_event(&mut events).await, Event::Synced);
}

#[tokio::test]
async fn sends_text_and_reports_sent() {
    let (core, mut events, network) = start();
    let body = MessageBody::new("hi there");

    core.send(Command::SendText {
        recipient: service_id(2),
        body: body.clone(),
    })
    .unwrap();

    let Event::MessageSent {
        recipient,
        timestamp,
    } = next_event(&mut events).await
    else {
        panic!("expected MessageSent");
    };
    assert_eq!(recipient, service_id(2));
    assert_eq!(
        network.sent(),
        vec![SentText {
            recipient,
            body,
            timestamp,
        }]
    );
}

#[tokio::test]
async fn reports_send_failure() {
    let (core, mut events, network) = start();
    network.fail_next_send(BackendError::new("rate limited"));

    core.send(Command::SendText {
        recipient: service_id(3),
        body: MessageBody::new("nope"),
    })
    .unwrap();

    let Event::SendFailed {
        recipient, error, ..
    } = next_event(&mut events).await
    else {
        panic!("expected SendFailed");
    };
    assert_eq!(recipient, service_id(3));
    assert_eq!(error, BackendError::new("rate limited"));
    assert!(network.sent().is_empty());
}

#[tokio::test]
async fn reports_receive_failure_and_keeps_serving_commands() {
    let (backend, network) = fake_backend();
    network.fail_receive(BackendError::new("unauthorized"));
    let (core, mut events) = copiale_core::spawn(move || backend).unwrap();

    assert_eq!(
        next_event(&mut events).await,
        Event::Disconnected {
            error: Some(BackendError::new("unauthorized"))
        }
    );

    core.send(Command::SendText {
        recipient: service_id(4),
        body: MessageBody::new("still here"),
    })
    .unwrap();
    assert!(matches!(
        next_event(&mut events).await,
        Event::MessageSent { .. }
    ));
}

#[tokio::test]
async fn reports_disconnect() {
    let (_core, mut events, mut network) = start();

    network.disconnect();

    assert_eq!(
        next_event(&mut events).await,
        Event::Disconnected { error: None }
    );
}

#[tokio::test]
async fn shutdown_stops_core_and_ends_events() {
    let (core, mut events, _network) = start();

    core.shutdown().expect("core thread exits cleanly");

    assert_eq!(next_event(&mut events).await, Event::Stopped);
    assert_eq!(events.recv().await, None);
}

#[tokio::test]
async fn dropping_handle_stops_core() {
    let (core, mut events, _network) = start();

    drop(core);

    assert_eq!(next_event(&mut events).await, Event::Stopped);
    assert_eq!(events.recv().await, None);
}
