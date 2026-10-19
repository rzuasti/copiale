//! Keeps secrets in memory (keys, passphrase, messages) from reaching disk
//! through core dumps.
//!
//! `COPIALE_UNSAFE_CORE_DUMPS=1` skips this, for debugging crashes; core
//! dumps can then hold the database key.

use std::io;

pub const UNSAFE_ENV: &str = "COPIALE_UNSAFE_CORE_DUMPS";

/// Disables core dumps unless [`UNSAFE_ENV`] is set. Call first thing in
/// `main`, after logging.
pub fn init() {
    if std::env::var(UNSAFE_ENV).is_ok_and(|value| value == "1") {
        tracing::warn!("{UNSAFE_ENV} is set: core dumps may contain keys");
    } else if let Err(error) = disable_core_dumps() {
        tracing::warn!(%error, "can't disable core dumps");
    }
}

/// Sets the core size limit to zero; dump handlers such as systemd-coredump
/// honor it too. Not `PR_SET_DUMPABLE`: it hides `/proc/<pid>` from
/// xdg-desktop-portal, failing every portal request.
#[cfg(unix)]
pub fn disable_core_dumps() -> io::Result<()> {
    let none = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `none` is a valid `rlimit`.
    if unsafe { libc::setrlimit(libc::RLIMIT_CORE, &none) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Does nothing yet on Windows.
#[cfg(not(unix))]
pub fn disable_core_dumps() -> io::Result<()> {
    Ok(())
}
