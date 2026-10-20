//! Core dumps are disabled. A separate test binary: the settings are
//! process-wide.

#[cfg(target_os = "linux")]
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

#[cfg(windows)]
#[test]
fn crashes_are_not_reported() {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::ErrorReporting::WerRemoveExcludedApplication;
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_ANY, RegGetValueW};

    copiale_core::hardening::disable_core_dumps().unwrap();

    let key: Vec<u16> = r"Software\Microsoft\Windows\Windows Error Reporting\ExcludedApplications"
        .encode_utf16()
        .chain([0])
        .collect();
    let exe = std::env::current_exe().unwrap();
    let name: Vec<u16> = exe.file_name().unwrap().encode_wide().chain([0]).collect();
    // SAFETY: NUL-terminated strings; no data is read.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_ANY,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    // SAFETY: `name` is NUL-terminated.
    unsafe { WerRemoveExcludedApplication(name.as_ptr(), 0) };
    assert_eq!(status, 0);
}
