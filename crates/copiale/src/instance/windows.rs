//! Named pipes share one machine-wide namespace any local user can create
//! names in: the pipe is named after this user's SID and the instance dir,
//! only they may open it, and requests go only to a server running as them.

use std::io;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::path::Path;
use std::ptr;

use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{GenericNamespaced, ListenerOptions, Name, Stream};
use interprocess::os::windows::local_socket::ListenerOptionsExt as _;
use interprocess::os::windows::security_descriptor::SecurityDescriptor;
use sha2::{Digest as _, Sha256};
use widestring::{U16CStr, U16CString};
use windows_sys::Win32::Foundation::{HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::{
    EqualSid, GetTokenInformation, PSID, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// The pipe's name, one per user and instance dir.
pub fn name(dir: &Path) -> io::Result<Name<'static>> {
    pipe_name(&own_sid()?, dir)
        .to_ns_name::<GenericNamespaced>()
        .map(Name::into_owned)
}

fn pipe_name(sid: &str, dir: &Path) -> String {
    let path: Vec<u8> = dir
        .as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect();
    let hash: String = Sha256::digest(path)[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("{}-{sid}-{hash}", copiale_core::APP_ID)
}

/// Makes this user the pipe's owner and the only one allowed to open it.
pub fn private(options: ListenerOptions<'_>) -> io::Result<ListenerOptions<'_>> {
    let sid = own_sid()?;
    // Protected DACL: one ACE granting the user all access, nothing inherited.
    let sddl =
        U16CString::from_str(format!("O:{sid}D:P(A;;GA;;;{sid})")).map_err(io::Error::other)?;
    Ok(options.security_descriptor(SecurityDescriptor::deserialize(&sddl)?))
}

/// Fails unless the pipe's server runs as this user.
pub fn check_server(connection: &Stream) -> io::Result<()> {
    let not_ours = || {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the running instance belongs to another user",
        )
    };
    let pid = connection.peer_creds()?.pid().ok_or_else(not_ours)?;
    // SAFETY: plain call; a null handle is checked below.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return Err(not_ours());
    }
    // SAFETY: a fresh handle owned by nothing else.
    let process = unsafe { OwnedHandle::from_raw_handle(process) };
    let theirs = TokenUserInfo::of(process.as_raw_handle()).map_err(|_| not_ours())?;
    // SAFETY: the pseudo handle needs no closing.
    let ours = TokenUserInfo::of(unsafe { GetCurrentProcess() })?;
    // SAFETY: both SIDs are valid while their buffers live.
    if unsafe { EqualSid(ours.sid(), theirs.sid()) } == 0 {
        return Err(not_ours());
    }
    Ok(())
}

/// This user's SID, as a string.
fn own_sid() -> io::Result<String> {
    // SAFETY: the pseudo handle needs no closing.
    let user = TokenUserInfo::of(unsafe { GetCurrentProcess() })?;
    let mut string = ptr::null_mut();
    // SAFETY: the SID is valid; on success `string` is ours to free.
    if unsafe { ConvertSidToStringSidW(user.sid(), &mut string) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a NUL-terminated string from the call above, freed once.
    let sid = unsafe { U16CStr::from_ptr_str(string) }.to_string_lossy();
    unsafe { LocalFree(string.cast()) };
    Ok(sid)
}

/// A process token's `TOKEN_USER`, in a buffer aligned for it.
struct TokenUserInfo(Vec<u64>);

impl TokenUserInfo {
    fn of(process: HANDLE) -> io::Result<Self> {
        let mut token = ptr::null_mut();
        // SAFETY: `process` is a valid handle; on success `token` is ours.
        if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a fresh handle owned by nothing else.
        let token = unsafe { OwnedHandle::from_raw_handle(token) };
        let mut len = 0;
        // Fails, giving the size needed.
        // SAFETY: a null buffer of length 0.
        unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                ptr::null_mut(),
                0,
                &mut len,
            )
        };
        let mut buffer = vec![0u64; (len as usize).div_ceil(size_of::<u64>())];
        // SAFETY: the buffer holds at least `len` bytes.
        let read = unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                buffer.as_mut_ptr().cast(),
                len,
                &mut len,
            )
        };
        if read == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(buffer))
    }

    fn sid(&self) -> PSID {
        // SAFETY: the buffer holds a `TOKEN_USER`, its SID pointing inside it.
        unsafe { (*self.0.as_ptr().cast::<TOKEN_USER>()).User.Sid }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::pipe_name;

    #[test]
    fn the_pipe_is_named_after_the_user() {
        assert!(super::own_sid().unwrap().starts_with("S-1-5-"));
    }

    #[test]
    fn each_dir_has_its_own_pipe() {
        let a = Path::new(r"C:\Users\a\AppData\Local\Copiale");
        let b = Path::new(r"C:\Users\a\AppData\Local\Other");
        assert_eq!(pipe_name("S-1-5-21", a), pipe_name("S-1-5-21", a));
        assert_ne!(pipe_name("S-1-5-21", a), pipe_name("S-1-5-21", b));
        assert_ne!(pipe_name("S-1-5-21", a), pipe_name("S-1-5-22", a));
    }
}
