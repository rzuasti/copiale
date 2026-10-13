//! Log filtering and redaction, checked on the formatted output.

use std::io::Write;
use std::sync::{Arc, Mutex};

use copiale_core::logging::{DEFAULT_FILTER, Options, subscriber};
use copiale_core::storage::{DatabaseKey, Passphrase};
use copiale_core::{
    Command, ConversationId, ConversationSummary, Direction, Event, GroupId, IncomingMessage,
    Message, MessageBody, MessageId, Peer, PhoneNumber, ServiceId, Timestamp,
};
use tracing_subscriber::fmt::MakeWriter;
use uuid::Uuid;

/// Collects formatted log output.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Capture {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Capture {
    type Writer = Capture;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Runs `emit` under a capturing subscriber and returns the output.
fn logged(filter: &str, unsafe_dependencies: bool, emit: impl FnOnce()) -> String {
    let capture = Capture::default();
    let options = Options {
        filter: filter.into(),
        unsafe_dependencies,
    };
    tracing::subscriber::with_default(subscriber(&options, capture.clone()), emit);
    capture.text()
}

#[test]
fn default_filter_shows_copiale_info_and_dependency_warnings() {
    let out = logged(DEFAULT_FILTER, false, || {
        tracing::info!(target: "copiale_core::actor", "core-info");
        tracing::debug!(target: "copiale_core::actor", "core-debug");
        tracing::warn!(target: "presage::manager", "dep-warn");
        tracing::info!(target: "presage::manager", "dep-info");
    });

    assert!(out.contains("core-info"));
    assert!(!out.contains("core-debug"));
    assert!(out.contains("dep-warn"));
    assert!(!out.contains("dep-info"));
}

#[test]
fn default_filter_hides_zbus_warnings() {
    let out = logged(DEFAULT_FILTER, false, || {
        tracing::warn!(target: "zbus::proxy", "zbus-warn");
        tracing::error!(target: "zbus::proxy", "zbus-error");
    });

    assert!(!out.contains("zbus-warn"));
    assert!(out.contains("zbus-error"));
}

#[test]
fn dependencies_stay_capped_at_warn_even_with_trace_filter() {
    let out = logged("trace", false, || {
        tracing::debug!(target: "copiale_core::actor", "core-debug");
        tracing::debug!(target: "presage::manager", "dep-debug");
        tracing::trace!(target: "libsignal_service::receiver", "dep-trace");
        tracing::warn!(target: "sqlx::query", "dep-warn");
    });

    assert!(out.contains("core-debug"));
    assert!(!out.contains("dep-debug"));
    assert!(!out.contains("dep-trace"));
    assert!(out.contains("dep-warn"));
}

#[test]
fn unsafe_flag_lifts_the_dependency_cap() {
    let out = logged("trace", true, || {
        tracing::debug!(target: "presage::manager", "dep-debug");
    });

    assert!(out.contains("dep-debug"));
}

#[test]
fn log_crate_records_are_capped_too() {
    // Global and idempotent; records then reach the thread's subscriber.
    tracing_log::LogTracer::init().ok();

    let out = logged("trace", false, || {
        log::debug!(target: "libsignal_protocol::session", "log-debug");
        log::warn!(target: "libsignal_protocol::session", "log-warn");
    });

    assert!(!out.contains("log-debug"));
    assert!(out.contains("log-warn"));
}

#[test]
fn invalid_filter_falls_back_to_default() {
    let out = logged("not a [valid filter", false, || {
        tracing::info!(target: "copiale_core::actor", "core-info");
        tracing::debug!(target: "copiale_core::actor", "core-debug");
    });

    assert!(out.contains("core-info"));
    assert!(!out.contains("core-debug"));
}

#[test]
fn identifiers_and_secrets_are_redacted_in_logs() {
    let service_id =
        ServiceId::new(Uuid::parse_str("0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0").unwrap());
    let phone = PhoneNumber::new("+14155550123");
    let group = GroupId::new([0x5a; 32]);
    let body = MessageBody::new("meet at the old mill");
    let key = DatabaseKey::from_slice(&[0xab; 32]).unwrap();
    let passphrase = Passphrase::new("hunter2-but-longer");
    let incoming = IncomingMessage {
        sender: service_id,
        timestamp: Timestamp::from_millis(1),
        body: body.clone(),
    };
    let event = Event::MessageAdded(Message {
        id: MessageId::new(1),
        conversation: ConversationId::new(1),
        direction: Direction::Incoming,
        sent_at: Timestamp::from_millis(1),
        received_at: Timestamp::from_millis(2),
        body: body.clone(),
        read: false,
    });
    let summary = Event::ConversationUpdated(ConversationSummary {
        id: ConversationId::new(1),
        peer: Peer::Contact(service_id),
        title: Some("Alice Liddell".into()),
        last_activity: None,
        unread: 1,
        last_message: None,
    });
    let command = Command::SendText {
        recipient: service_id,
        body: body.clone(),
    };

    let out = logged("trace", false, || {
        tracing::info!(
            target: "copiale_core::test",
            ?service_id,
            ?phone,
            ?group,
            ?body,
            ?key,
            ?passphrase,
            ?incoming,
            ?event,
            ?summary,
            ?command,
            "everything"
        );
    });

    assert!(out.contains("everything"));
    for secret in [
        "0f1e2d3c", "+1415555", "5a5a5a5a", "old mill", "abababab", "hunter2", "Alice",
    ] {
        assert!(!out.contains(secret), "{secret:?} leaked in: {out}");
    }
}

#[test]
fn dependency_output_is_scrubbed() {
    let out = logged(DEFAULT_FILTER, false, || {
        tracing::warn!(
            target: "presage::manager::registered",
            thread = "Thread(contact=0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0)",
            ts = 1_700_000_000_123u64,
            "could not find message to delete in thread"
        );
        tracing::warn!(target: "libsignal_service::models", "bad number +14155550123");
    });

    assert!(out.contains("could not find message to delete"));
    assert!(out.contains("…e1f0"));
    assert!(out.contains("1700000000123"), "timestamps survive: {out}");
    assert!(out.contains("+…23"));
    assert!(!out.contains("0f1e2d3c"), "{out}");
    assert!(!out.contains("4155550"), "{out}");
}
