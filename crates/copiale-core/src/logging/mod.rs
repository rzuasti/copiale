//! Logging to stderr with `tracing`.
//!
//! Copiale writes no log files, but stderr may still be kept: the systemd
//! journal stores it when Copiale starts from a desktop session, often
//! readable by `adm` or `wheel` members. So logs must not identify contacts.
//!
//! - `COPIALE_LOG` sets the filter (`EnvFilter` syntax), default
//!   [`DEFAULT_FILTER`].
//! - Dependencies are capped at `warn` whatever the filter says: presage and
//!   libsignal log message contents at `debug`. libsignal-protocol is off:
//!   it logs several multi-line errors per message that fails to decrypt,
//!   which presage and Copiale report too.
//! - Panics are logged with their location only: their messages can hold
//!   message text (string slicing panics print the string). Messages that
//!   are string literals are kept.
//! - `COPIALE_UNSAFE_LOG=1` lifts the dependency cap and logs panic
//!   messages; use it with test accounts only.
//!
//! Copiale's own logs carry identifiers only through redacting types
//! ([`ServiceId`](crate::ServiceId), [`PhoneNumber`](crate::PhoneNumber),
//! [`GroupId`](crate::GroupId)), which show a [`Tag`]: a keyed hash, with a
//! key random per run, so lines correlate within a run but not across runs
//! or with a known identifier. As a backstop, all output is scrubbed of
//! UUIDs, long hex runs and phone numbers, which dependencies do print,
//! replacing them with their tags.

mod scrub;

use std::fmt;
use std::hash::{BuildHasher as _, Hasher as _, RandomState};
use std::panic::PanicHookInfo;
use std::sync::OnceLock;

use tracing::field::{Field, Visit};
use tracing::{Level, Metadata, Subscriber};
use tracing_subscriber::filter::{EnvFilter, filter_fn};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::{self, Filter, SubscriberExt as _};
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{Layer as _, Registry};

pub const FILTER_ENV: &str = "COPIALE_LOG";
pub const UNSAFE_ENV: &str = "COPIALE_UNSAFE_LOG";
/// `zbus` warns about missing D-Bus services (no keyring, no portal), which
/// is expected on minimal desktops.
pub const DEFAULT_FILTER: &str = "warn,zbus=error,copiale=info,copiale_core=info,copiale_ui=info";

/// Installs the global logger from the environment, including `log` records
/// from dependencies. Does nothing if a logger is already installed.
pub fn init() {
    let filter = std::env::var(FILTER_ENV).unwrap_or_else(|_| DEFAULT_FILTER.into());
    let options = Options {
        filter,
        unsafe_log: std::env::var(UNSAFE_ENV).is_ok_and(|value| value == "1"),
    };
    // Also routes `log` records into tracing.
    let installed = subscriber(&options, std::io::stderr).try_init().is_ok();
    if !installed {
        return;
    }
    install_panic_hook(options.unsafe_log);
    if options.unsafe_log {
        tracing::warn!("{UNSAFE_ENV} is set: logs may contain message contents");
    }
}

/// Sends panics through the logger, and its scrubbing, instead of raw
/// stderr; with `unsafe_log`, including their messages. [`init`] calls this.
pub fn install_panic_hook(unsafe_log: bool) {
    std::panic::set_hook(Box::new(move |info| log_panic(info, unsafe_log)));
}

/// The backtrace is included when `RUST_BACKTRACE` asks for one.
fn log_panic(info: &PanicHookInfo<'_>, unsafe_log: bool) {
    let thread = std::thread::current();
    let thread = thread.name().unwrap_or("unnamed");
    let summary = panic_summary(info, unsafe_log);
    let backtrace = std::backtrace::Backtrace::capture();
    if backtrace.status() == std::backtrace::BacktraceStatus::Captured {
        tracing::error!(thread, "{summary}\n{backtrace}");
    } else {
        tracing::error!(thread, "{summary}");
    }
}

/// A `&'static str` payload is a string literal; formatted messages are
/// `String`s and may hold data.
fn panic_summary(info: &PanicHookInfo<'_>, unsafe_log: bool) -> String {
    if unsafe_log {
        return format!("panic: {info}");
    }
    let location = info
        .location()
        .map_or_else(|| "unknown location".into(), ToString::to_string);
    match info.payload().downcast_ref::<&'static str>() {
        Some(message) => format!("panic at {location}: {message}"),
        None => format!("panic at {location} (message hidden; see {UNSAFE_ENV})"),
    }
}

/// How to log; see the module docs.
#[derive(Clone, Debug)]
pub struct Options {
    pub filter: String,
    /// Lifts the dependency cap and logs panic messages.
    pub unsafe_log: bool,
}

/// A loggable stand-in for an identifier: 16 bits of a keyed hash, keyed
/// per run. Shown as `#` and 4 hex digits.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Tag(u16);

impl Tag {
    pub fn new(bytes: &[u8]) -> Self {
        static KEY: OnceLock<RandomState> = OnceLock::new();
        Self::with_key(KEY.get_or_init(RandomState::new), bytes)
    }

    /// `RandomState` is SipHash with a random key.
    fn with_key(key: &RandomState, bytes: &[u8]) -> Self {
        let mut hasher = key.build_hasher();
        hasher.write(bytes);
        Self(hasher.finish() as u16)
    }
}

impl fmt::Display for Tag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:04x}", self.0)
    }
}

impl fmt::Debug for Tag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// The subscriber [`init`] installs, writing to `writer`. An invalid filter
/// falls back to [`DEFAULT_FILTER`].
pub fn subscriber<W>(options: &Options, writer: W) -> impl Subscriber + Send + Sync + use<W>
where
    W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
    let filter =
        EnvFilter::try_new(&options.filter).unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    let unsafe_log = options.unsafe_log;
    let cap = filter_fn(move |metadata| {
        let target = metadata.target();
        unsafe_log
            || is_copiale(target)
            || (*metadata.level() <= Level::WARN && !target.starts_with("libsignal_protocol"))
    });
    let layer = tracing_subscriber::fmt::layer()
        .with_writer(scrub::Scrubbing(writer))
        .with_filter(filter)
        .with_filter(cap)
        .with_filter(QuietEmptyStore);
    Registry::default().with(layer)
}

/// presage warnings about messages missing from its store, which keeps none
/// by design (see `signal::store`).
const EMPTY_STORE_WARNINGS: &[&str] = &[
    "could not find message to delete in thread",
    "Message referenced by delete-for-self message not found",
    "Could not delete message referenced by delete-for-self message",
];

/// Drops [`EMPTY_STORE_WARNINGS`].
struct QuietEmptyStore;

impl<S> Filter<S> for QuietEmptyStore {
    fn enabled(&self, _: &Metadata<'_>, _: &layer::Context<'_, S>) -> bool {
        true
    }

    fn event_enabled(&self, event: &tracing::Event<'_>, _: &layer::Context<'_, S>) -> bool {
        if !event.metadata().target().starts_with("presage") {
            return true;
        }
        let mut message = MessageText(None);
        event.record(&mut message);
        !message
            .0
            .is_some_and(|text| EMPTY_STORE_WARNINGS.iter().any(|w| text.starts_with(w)))
    }
}

/// An event's message text.
struct MessageText(Option<String>);

impl Visit for MessageText {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = Some(format!("{value:?}"));
        }
    }
}

fn is_copiale(target: &str) -> bool {
    target == "copiale" || target.starts_with("copiale_") || target.starts_with("copiale::")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copiale_targets_are_recognized() {
        for target in [
            "copiale",
            "copiale::main",
            "copiale_core::actor",
            "copiale_ui",
        ] {
            assert!(is_copiale(target), "{target}");
        }
        for target in [
            "presage::manager",
            "libsignal_protocol",
            "copialex",
            "sqlx::query",
        ] {
            assert!(!is_copiale(target), "{target}");
        }
    }

    #[test]
    fn tags_are_stable_within_a_run() {
        assert_eq!(Tag::new(b"alice"), Tag::new(b"alice"));
        assert_eq!(Tag::new(&[0xab; 16]).to_string().len(), 5);
        assert!(Tag::new(b"alice").to_string().starts_with('#'));
    }

    #[test]
    fn tags_depend_on_the_key() {
        let (first, second) = (RandomState::new(), RandomState::new());
        let inputs: [&[u8]; 4] = [b"alice", b"bob", b"carol", &[0xab; 16]];
        assert!(
            inputs
                .iter()
                .any(|input| Tag::with_key(&first, input) != Tag::with_key(&second, input))
        );
    }
}
