//! Core dumps are disabled. A separate test binary: the settings are
//! process-wide.

#![cfg(target_os = "linux")]

#[test]
fn core_dumps_are_disabled() {
    copiale_core::hardening::disable_core_dumps().unwrap();

    let mut limit = libc::rlimit {
        rlim_cur: libc::RLIM_INFINITY,
        rlim_max: libc::RLIM_INFINITY,
    };
    // SAFETY: `limit` is a valid `rlimit`.
    assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_CORE, &mut limit) }, 0);
    assert_eq!((limit.rlim_cur, limit.rlim_max), (0, 0));
}
