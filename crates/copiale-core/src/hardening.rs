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

/// Excludes this executable from Windows Error Reporting, which can keep
/// a dump locally and upload it; reports still made leave out the heap.
/// Dumps an administrator enabled through `LocalDumps` are still written.
#[cfg(windows)]
pub fn disable_core_dumps() -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::ErrorReporting::{
        WER_FAULT_REPORTING_FLAG_NOHEAP, WerAddExcludedApplication, WerSetFlags,
    };

    // SAFETY: takes no pointers.
    let no_heap = check(unsafe { WerSetFlags(WER_FAULT_REPORTING_FLAG_NOHEAP) });
    let exe = std::env::current_exe()?;
    let name = exe
        .file_name()
        .ok_or_else(|| io::Error::other("no executable name"))?;
    let name: Vec<u16> = name.encode_wide().chain([0]).collect();
    // SAFETY: `name` is NUL-terminated; per user, no elevation needed.
    let excluded = check(unsafe { WerAddExcludedApplication(name.as_ptr(), 0) });
    no_heap.and(excluded)
}

#[cfg(windows)]
fn check(result: windows_sys::core::HRESULT) -> io::Result<()> {
    if result < 0 {
        return Err(io::Error::from_raw_os_error(result));
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
pub fn disable_core_dumps() -> io::Result<()> {
    Ok(())
}
