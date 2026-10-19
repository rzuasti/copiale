//! Keeps secrets in memory (keys, passphrase, messages) from reaching disk
//! or other processes through core dumps and debuggers.
//!
//! `COPIALE_UNSAFE_DEBUG=1` skips this, for debugging and profiling
//! (`gdb`, `perf -p`); core dumps can then hold the database key.

use std::io;

pub const UNSAFE_ENV: &str = "COPIALE_UNSAFE_DEBUG";

/// Disables core dumps unless [`UNSAFE_ENV`] is set. Call first thing in
/// `main`, after logging.
pub fn init() {
    if std::env::var(UNSAFE_ENV).is_ok_and(|value| value == "1") {
        tracing::warn!("{UNSAFE_ENV} is set: core dumps may contain keys");
    } else if let Err(error) = disable_core_dumps() {
        tracing::warn!(%error, "can't disable core dumps");
    }
}

/// Sets the core size limit to zero. On Linux, also makes the process
/// non-dumpable: covers dump handlers that ignore the limit, and keeps
/// other processes of the user from attaching (`ptrace`, `perf`).
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
    #[cfg(target_os = "linux")]
    // SAFETY: `PR_SET_DUMPABLE` takes one integer argument.
    if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0 as libc::c_ulong) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Does nothing yet on Windows.
#[cfg(not(unix))]
pub fn disable_core_dumps() -> io::Result<()> {
    Ok(())
}
