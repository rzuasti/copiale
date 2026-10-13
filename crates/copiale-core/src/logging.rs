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
//! [`GroupId`](crate::GroupId)).

use tracing::{Level, Subscriber};
use tracing_subscriber::filter::{EnvFilter, filter_fn};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{Layer as _, Registry};

pub const FILTER_ENV: &str = "COPIALE_LOG";
pub const UNSAFE_ENV: &str = "COPIALE_UNSAFE_LOG";
pub const DEFAULT_FILTER: &str = "warn,copiale=info,copiale_core=info,copiale_ui=info";

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
    if installed && options.unsafe_dependencies {
        tracing::warn!("{UNSAFE_ENV} is set: dependency logs may contain message contents");
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
        .with_writer(writer)
        .with_filter(filter)
        .with_filter(cap);
    Registry::default().with(layer)
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
