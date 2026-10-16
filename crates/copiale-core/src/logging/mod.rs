//! Logging to stderr with `tracing`; nothing is written to disk.
//!
//! - `COPIALE_LOG` sets the filter (`EnvFilter` syntax), default
//!   [`DEFAULT_FILTER`].
//! - Dependencies are capped at `warn` whatever the filter says: presage and
//!   libsignal log message contents at `debug`. `COPIALE_UNSAFE_LOG=1` lifts
//!   the cap; use it with test accounts only.
//!
//! Copiale's own logs carry identifiers only through redacting types
//! ([`ServiceId`](crate::ServiceId), [`PhoneNumber`](crate::PhoneNumber),
//! [`GroupId`](crate::GroupId)). As a backstop, all output is scrubbed of
//! UUIDs, long hex runs and phone numbers, which dependencies do print.

mod scrub;

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
        unsafe_dependencies: std::env::var(UNSAFE_ENV).is_ok_and(|value| value == "1"),
    };
    // Also routes `log` records into tracing.
    let installed = subscriber(&options, std::io::stderr).try_init().is_ok();
    if !installed {
        return;
    }
    install_panic_hook();
    if options.unsafe_dependencies {
        tracing::warn!("{UNSAFE_ENV} is set: dependency logs may contain message contents");
    }
}

/// Sends panics through the logger, and its scrubbing, instead of raw
/// stderr. [`init`] calls this.
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(log_panic));
}

/// The backtrace is included when `RUST_BACKTRACE` asks for one.
fn log_panic(info: &std::panic::PanicHookInfo<'_>) {
    let thread = std::thread::current();
    let thread = thread.name().unwrap_or("unnamed");
    let backtrace = std::backtrace::Backtrace::capture();
    if backtrace.status() == std::backtrace::BacktraceStatus::Captured {
        tracing::error!(thread, "panic: {info}\n{backtrace}");
    } else {
        tracing::error!(thread, "panic: {info}");
    }
}

/// How to log; see the module docs.
#[derive(Clone, Debug)]
pub struct Options {
    pub filter: String,
    pub unsafe_dependencies: bool,
}

/// The subscriber [`init`] installs, writing to `writer`. An invalid filter
/// falls back to [`DEFAULT_FILTER`].
pub fn subscriber<W>(options: &Options, writer: W) -> impl Subscriber + Send + Sync + use<W>
where
    W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
    let filter =
        EnvFilter::try_new(&options.filter).unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    let unsafe_dependencies = options.unsafe_dependencies;
    let cap = filter_fn(move |metadata| {
        unsafe_dependencies || is_copiale(metadata.target()) || *metadata.level() <= Level::WARN
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
}
