//! Windows: the helper can't confine itself, so it's created confined.
//!
//! - An AppContainer profile of its own ([`PROFILE`]), without
//!   capabilities: no network, and no files, registry or other objects
//!   beyond what's open to all apps.
//! - A job: one process, 2 GiB of memory, limited CPU time, no access to
//!   others' windows, the clipboard or system settings; killed with its last
//!   handle.
//! - Mitigations: no dynamic code or child processes; only
//!   Microsoft-signed DLLs, System32's first, none remote or low-integrity;
//!   no extension point DLLs or non-system fonts.
//! - Inherits only its pipes, and of the environment only the paths
//!   AppContainers are given their own of; runs in System32.

use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::marker::PhantomData;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_NOT_FOUND, HANDLE,
    HANDLE_FLAG_INHERIT, LocalFree, SetHandleInformation, WAIT_FAILED, WIN32_ERROR,
};
use windows_sys::Win32::Security::Authorization::{
    EXPLICIT_ACCESS_W, GRANT_ACCESS, GetNamedSecurityInfoW, NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT,
    SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
};
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
use windows_sys::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, FreeSid, NO_INHERITANCE, PSID, SECURITY_CAPABILITIES,
};
use windows_sys::Win32::Storage::FileSystem::{FILE_GENERIC_EXECUTE, FILE_GENERIC_READ};
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
    JOB_OBJECT_LIMIT_PROCESS_TIME, JOB_OBJECT_UILIMIT_DESKTOP, JOB_OBJECT_UILIMIT_DISPLAYSETTINGS,
    JOB_OBJECT_UILIMIT_EXITWINDOWS, JOB_OBJECT_UILIMIT_GLOBALATOMS, JOB_OBJECT_UILIMIT_HANDLES,
    JOB_OBJECT_UILIMIT_READCLIPBOARD, JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS,
    JOB_OBJECT_UILIMIT_WRITECLIPBOARD, JOBOBJECT_BASIC_UI_RESTRICTIONS,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOBOBJECTINFOCLASS, JobObjectBasicUIRestrictions,
    JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
use windows_sys::Win32::System::Threading::{
    CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DETACHED_PROCESS, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_JOB_LIST,
    PROC_THREAD_ATTRIBUTE_MITIGATION_POLICY, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
    PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOEXW, UpdateProcThreadAttribute,
    WaitForSingleObject,
};
use windows_sys::Win32::System::WindowsProgramming::PROCESS_CREATION_CHILD_PROCESS_RESTRICTED;
use windows_sys::core::{BOOL, HRESULT};

/// The helper's AppContainer profile: `APP_ID` plus `.Video`.
pub const PROFILE: &str = "page.codeberg.korba.Copiale.Video";

/// CPU time for a first frame, and for playing.
pub const FRAME_CPU: Duration = Duration::from_secs(20);
pub const PLAY_CPU: Duration = Duration::from_secs(60 * 60);
/// Memory the helper may commit.
const MAX_MEMORY: usize = 2 * 1024 * 1024 * 1024;
/// Frames stream through the pipes in large writes.
const PIPE_BUFFER: u32 = 1024 * 1024;

// `PROCESS_CREATION_MITIGATION_POLICY_*_ALWAYS_ON` (winbase.h).
const HEAP_TERMINATE: u64 = 1 << 12;
const STRICT_HANDLE_CHECKS: u64 = 1 << 24;
const EXTENSION_POINT_DISABLE: u64 = 1 << 32;
const PROHIBIT_DYNAMIC_CODE: u64 = 1 << 36;
const MICROSOFT_SIGNED_ONLY: u64 = 1 << 44;
const FONT_DISABLE: u64 = 1 << 48;
const IMAGE_LOAD_NO_REMOTE: u64 = 1 << 52;
const IMAGE_LOAD_NO_LOW_LABEL: u64 = 1 << 56;
const IMAGE_LOAD_PREFER_SYSTEM32: u64 = 1 << 60;
const MITIGATIONS: u64 = HEAP_TERMINATE
    | STRICT_HANDLE_CHECKS
    | EXTENSION_POINT_DISABLE
    | PROHIBIT_DYNAMIC_CODE
    | MICROSOFT_SIGNED_ONLY
    | FONT_DISABLE
    | IMAGE_LOAD_NO_REMOTE
    | IMAGE_LOAD_NO_LOW_LABEL
    | IMAGE_LOAD_PREFER_SYSTEM32;

/// Held while a helper's pipe ends are inheritable, so no other helper
/// starting meanwhile gets them.
static SPAWNING: Mutex<()> = Mutex::new(());
/// Held while making a profile: made at once, they fail.
static PROFILES: Mutex<()> = Mutex::new(());
/// The program was made readable from the AppContainer.
static GRANTED: AtomicBool = AtomicBool::new(false);
/// Passed on: process creation points the app data and temporary
/// directories into the AppContainer's own, failing without them. Sorted.
const ENVIRONMENT: &[&str] = &["LOCALAPPDATA", "SystemRoot", "TEMP", "TMP", "USERPROFILE"];

/// A sandboxed helper; killed when dropped.
pub struct Process {
    process: OwnedHandle,
    job: OwnedHandle,
}

impl Process {
    pub fn kill(&self) -> io::Result<()> {
        // SAFETY: a job handle we own.
        check(unsafe { TerminateJobObject(self.job.as_raw_handle(), 1) })
    }

    /// Waits for it to exit; its exit code.
    pub fn wait(&self) -> io::Result<u32> {
        // SAFETY: a process handle we own.
        if unsafe { WaitForSingleObject(self.process.as_raw_handle(), INFINITE) } == WAIT_FAILED {
            return Err(io::Error::last_os_error());
        }
        let mut code = 0;
        // SAFETY: as above, and a valid out-pointer.
        check(unsafe { GetExitCodeProcess(self.process.as_raw_handle(), &mut code) })?;
        Ok(code)
    }
}

/// Starts `program` with `args` in `profile`'s AppContainer (made if
/// missing), with `cpu` of CPU time; with its stdin and stdout.
pub fn spawn(
    profile: &str,
    program: &Path,
    args: &[&OsStr],
    cpu: Duration,
) -> io::Result<(Process, File, File)> {
    let sid = AppContainer::create(profile)?;
    if !GRANTED.load(Ordering::Relaxed) {
        match grant(&sid, program) {
            Ok(()) => GRANTED.store(true, Ordering::Relaxed),
            // Already readable where installed for all apps.
            Err(error) => tracing::debug!(%error, "can't let the video helper read its program"),
        }
    }
    let job = job(cpu)?;
    let (child_stdin, stdin) = pipe()?;
    let (stdout, child_stdout) = pipe()?;
    let process = {
        let _spawning = SPAWNING.lock().unwrap_or_else(PoisonError::into_inner);
        inheritable(&child_stdin)?;
        inheritable(&child_stdout)?;
        let stdio = [child_stdin.as_raw_handle(), child_stdout.as_raw_handle()];
        let process = create(&sid, &job, program, args, stdio);
        // The child has its own.
        drop((child_stdin, child_stdout));
        process?
    };
    Ok((
        Process { process, job },
        File::from(stdin),
        File::from(stdout),
    ))
}

/// Deletes `profile`'s AppContainer profile, if there is one.
pub fn remove(profile: &str) -> io::Result<()> {
    let name = wide(profile);
    // SAFETY: a NUL-terminated name.
    let result = unsafe { DeleteAppContainerProfile(name.as_ptr()) };
    if result == from_win32(ERROR_NOT_FOUND) || result == from_win32(ERROR_FILE_NOT_FOUND) {
        return Ok(());
    }
    hresult(result)
}

/// `stdio` are the helper's stdin and stdout, inheritable.
fn create(
    sid: &AppContainer,
    job: &OwnedHandle,
    program: &Path,
    args: &[&OsStr],
    stdio: [HANDLE; 2],
) -> io::Result<OwnedHandle> {
    let capabilities = SECURITY_CAPABILITIES {
        AppContainerSid: sid.0,
        Capabilities: std::ptr::null_mut(),
        CapabilityCount: 0,
        Reserved: 0,
    };
    let jobs = [job.as_raw_handle()];
    let child_processes = PROCESS_CREATION_CHILD_PROCESS_RESTRICTED;
    let mut attributes = Attributes::new(5)?;
    attributes.set(PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, &capabilities)?;
    attributes.set(PROC_THREAD_ATTRIBUTE_HANDLE_LIST, &stdio)?;
    attributes.set(PROC_THREAD_ATTRIBUTE_JOB_LIST, &jobs)?;
    attributes.set(PROC_THREAD_ATTRIBUTE_MITIGATION_POLICY, &MITIGATIONS)?;
    attributes.set(PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY, &child_processes)?;

    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = stdio[0];
    startup.StartupInfo.hStdOutput = stdio[1];
    startup.lpAttributeList = attributes.as_ptr();
    let application = wide(program);
    let mut command_line = command_line(program.as_os_str(), args);
    let environment = environment();
    let directory = system_directory()?;
    let mut created = PROCESS_INFORMATION::default();
    // SAFETY: NUL-terminated strings, an empty environment block, and an
    // attribute list whose values outlive the call.
    check(unsafe {
        CreateProcessW(
            application.as_ptr(),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            // No console: sandboxed, setting one up fails.
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | DETACHED_PROCESS,
            environment.as_ptr().cast(),
            directory.as_ptr(),
            &startup.StartupInfo,
            &mut created,
        )
    })?;
    // SAFETY: handles CreateProcessW gave us to close.
    unsafe {
        CloseHandle(created.hThread);
        Ok(OwnedHandle::from_raw_handle(created.hProcess))
    }
}

/// An AppContainer profile's SID.
struct AppContainer(PSID);

impl AppContainer {
    /// Creates `profile`, without capabilities, unless it exists.
    fn create(profile: &str) -> io::Result<Self> {
        let _making = PROFILES.lock().unwrap_or_else(PoisonError::into_inner);
        let name = wide(profile);
        let display = wide("Copiale video decoder");
        let mut sid = std::ptr::null_mut();
        // SAFETY: NUL-terminated strings, no capabilities, and a valid
        // out-pointer; the SID is freed on drop.
        let created = unsafe {
            CreateAppContainerProfile(
                name.as_ptr(),
                display.as_ptr(),
                display.as_ptr(),
                std::ptr::null(),
                0,
                &mut sid,
            )
        };
        if created == from_win32(ERROR_ALREADY_EXISTS) {
            // SAFETY: as above.
            hresult(unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &mut sid) })?;
        } else {
            hresult(created)?;
        }
        Ok(Self(sid))
    }
}

impl Drop for AppContainer {
    fn drop(&mut self) {
        // SAFETY: a SID from the profile functions, freed once.
        unsafe { FreeSid(self.0) };
    }
}

/// Lets `sid` read and run `program`: the loader maps it from inside.
fn grant(sid: &AppContainer, program: &Path) -> io::Result<()> {
    let path = wide(program);
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut descriptor = std::ptr::null_mut();
    // SAFETY: a NUL-terminated path and valid out-pointers; `dacl` points
    // into `descriptor`, freed after its last use.
    win32(unsafe {
        GetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    })?;
    let _descriptor = Local(descriptor);
    let access = EXPLICIT_ACCESS_W {
        grfAccessPermissions: FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
        grfAccessMode: GRANT_ACCESS,
        grfInheritance: NO_INHERITANCE,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: sid.0.cast(),
        },
    };
    let mut granted: *mut ACL = std::ptr::null_mut();
    // SAFETY: one entry, the old DACL, and a valid out-pointer.
    win32(unsafe { SetEntriesInAclW(1, &access, dacl, &mut granted) })?;
    let granted = Local(granted.cast());
    // SAFETY: a NUL-terminated path and the new DACL.
    win32(unsafe {
        SetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            granted.0.cast::<ACL>(),
            std::ptr::null(),
        )
    })
}

/// Memory to free with `LocalFree`.
struct Local(*mut core::ffi::c_void);

impl Drop for Local {
    fn drop(&mut self) {
        // SAFETY: allocated by the security functions, freed once.
        unsafe { LocalFree(self.0) };
    }
}

fn job(cpu: Duration) -> io::Result<OwnedHandle> {
    // SAFETY: unnamed, default security.
    let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if job.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a new handle we own.
    let job = unsafe { OwnedHandle::from_raw_handle(job) };
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_ACTIVE_PROCESS
        | JOB_OBJECT_LIMIT_PROCESS_MEMORY
        | JOB_OBJECT_LIMIT_PROCESS_TIME
        | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
    limits.BasicLimitInformation.ActiveProcessLimit = 1;
    // In 100 ns units.
    limits.BasicLimitInformation.PerProcessUserTimeLimit = (cpu.as_nanos() / 100) as i64;
    limits.ProcessMemoryLimit = MAX_MEMORY;
    set_job(&job, JobObjectExtendedLimitInformation, &limits)?;
    let ui = JOBOBJECT_BASIC_UI_RESTRICTIONS {
        UIRestrictionsClass: JOB_OBJECT_UILIMIT_DESKTOP
            | JOB_OBJECT_UILIMIT_DISPLAYSETTINGS
            | JOB_OBJECT_UILIMIT_EXITWINDOWS
            | JOB_OBJECT_UILIMIT_GLOBALATOMS
            | JOB_OBJECT_UILIMIT_HANDLES
            | JOB_OBJECT_UILIMIT_READCLIPBOARD
            | JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS
            | JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
    };
    set_job(&job, JobObjectBasicUIRestrictions, &ui)?;
    Ok(job)
}

fn set_job<T>(job: &OwnedHandle, class: JOBOBJECTINFOCLASS, value: &T) -> io::Result<()> {
    // SAFETY: `value` is `class`'s structure.
    check(unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            class,
            (value as *const T).cast(),
            size_of::<T>() as u32,
        )
    })
}

/// A process attribute list over values borrowed for `'a`.
struct Attributes<'a> {
    list: Vec<usize>,
    values: PhantomData<&'a ()>,
}

impl<'a> Attributes<'a> {
    fn new(count: u32) -> io::Result<Self> {
        let mut size = 0;
        // SAFETY: only asks the size needed; fails saying so.
        unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), count, 0, &mut size) };
        let mut list = vec![0usize; size.div_ceil(size_of::<usize>())];
        // SAFETY: a buffer of that size, aligned.
        check(unsafe {
            InitializeProcThreadAttributeList(list.as_mut_ptr().cast(), count, 0, &mut size)
        })?;
        Ok(Self {
            list,
            values: PhantomData,
        })
    }

    fn set<T>(&mut self, attribute: u32, value: &'a T) -> io::Result<()> {
        // SAFETY: an initialized list; `value` outlives it.
        check(unsafe {
            UpdateProcThreadAttribute(
                self.as_ptr(),
                0,
                attribute as usize,
                (value as *const T).cast(),
                size_of::<T>(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        })
    }

    fn as_ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.list.as_mut_ptr().cast()
    }
}

impl Drop for Attributes<'_> {
    fn drop(&mut self) {
        // SAFETY: an initialized list, deleted once.
        unsafe { DeleteProcThreadAttributeList(self.as_ptr()) };
    }
}

/// An anonymous pipe's read and write ends, neither inheritable.
fn pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
    let (mut read, mut write) = (std::ptr::null_mut(), std::ptr::null_mut());
    // SAFETY: valid out-pointers, default security.
    check(unsafe { CreatePipe(&mut read, &mut write, std::ptr::null(), PIPE_BUFFER) })?;
    // SAFETY: new handles we own.
    unsafe {
        Ok((
            OwnedHandle::from_raw_handle(read),
            OwnedHandle::from_raw_handle(write),
        ))
    }
}

fn inheritable(handle: &OwnedHandle) -> io::Result<()> {
    // SAFETY: a handle we own.
    check(unsafe {
        SetHandleInformation(
            handle.as_raw_handle(),
            HANDLE_FLAG_INHERIT,
            HANDLE_FLAG_INHERIT,
        )
    })
}

/// [`ENVIRONMENT`]'s variables that are set, as an environment block.
fn environment() -> Vec<u16> {
    let mut block = Vec::new();
    for name in ENVIRONMENT {
        if let Some(value) = std::env::var_os(name) {
            block.extend(OsStr::new(name).encode_wide());
            block.push(u16::from(b'='));
            block.extend(value.encode_wide());
            block.push(0);
        }
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    block
}

fn system_directory() -> io::Result<Vec<u16>> {
    let mut buffer = vec![0u16; 512];
    // SAFETY: a buffer of that length.
    let length = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    if length == 0 || length >= buffer.len() {
        return Err(io::Error::last_os_error());
    }
    buffer.truncate(length);
    buffer.push(0);
    Ok(buffer)
}

/// `program` and `args` quoted as the C runtime splits them, NUL-terminated.
fn command_line(program: &OsStr, args: &[&OsStr]) -> Vec<u16> {
    let mut line = Vec::new();
    for (i, arg) in std::iter::once(program)
        .chain(args.iter().copied())
        .enumerate()
    {
        if i > 0 {
            line.push(u16::from(b' '));
        }
        quote(arg, &mut line);
    }
    line.push(0);
    line
}

fn quote(arg: &OsStr, line: &mut Vec<u16>) {
    let (quote, backslash) = (u16::from(b'"'), u16::from(b'\\'));
    let arg: Vec<u16> = arg.encode_wide().collect();
    let plain = !arg.is_empty()
        && !arg
            .iter()
            .any(|&c| c == quote || c == u16::from(b' ') || c == u16::from(b'\t') || c == 10);
    if plain {
        line.extend(arg);
        return;
    }
    line.push(quote);
    let mut backslashes = 0;
    for c in arg {
        if c == backslash {
            backslashes += 1;
            continue;
        }
        // Backslashes escape only before a quote.
        let count = if c == quote {
            backslashes * 2 + 1
        } else {
            backslashes
        };
        line.extend(std::iter::repeat_n(backslash, count));
        backslashes = 0;
        line.push(c);
    }
    line.extend(std::iter::repeat_n(backslash, backslashes * 2));
    line.push(quote);
}

fn wide(text: impl AsRef<OsStr>) -> Vec<u16> {
    text.as_ref().encode_wide().chain([0]).collect()
}

fn check(result: BOOL) -> io::Result<()> {
    match result {
        0 => Err(io::Error::last_os_error()),
        _ => Ok(()),
    }
}

fn win32(error: WIN32_ERROR) -> io::Result<()> {
    match error {
        0 => Ok(()),
        _ => Err(io::Error::from_raw_os_error(error as i32)),
    }
}

fn hresult(result: HRESULT) -> io::Result<()> {
    match result {
        0.. => Ok(()),
        _ => Err(io::Error::from_raw_os_error(result)),
    }
}

/// `HRESULT_FROM_WIN32`.
fn from_win32(error: WIN32_ERROR) -> HRESULT {
    ((error & 0xFFFF) | 0x8007_0000) as HRESULT
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;
    use std::net::{TcpListener, TcpStream};
    use std::path::PathBuf;

    use windows_sys::Win32::Security::EqualSid;
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_EXECUTE_READWRITE, VirtualAlloc, VirtualFree,
    };

    use super::*;

    /// Marks the sandboxed copy of the test binary: it gets no environment
    /// variable to tell.
    const CHILD: &str = "copiale-sandboxed-child";
    const OTHER_PROGRAM: &str = r"C:\Windows\System32\whoami.exe";

    #[test]
    fn the_profile_is_the_apps() {
        assert_eq!(PROFILE, format!("{}.Video", crate::APP_ID));
    }

    #[test]
    fn arguments_are_quoted_for_the_c_runtime() {
        let line = |args: &[&str]| {
            let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
            let mut line = command_line(OsStr::new(r"C:\a b\c.exe"), &args);
            line.pop();
            String::from_utf16(&line).unwrap()
        };
        assert_eq!(line(&["--video-frame"]), r#""C:\a b\c.exe" --video-frame"#);
        assert_eq!(
            line(&["", "a b", r#"a"b"#, r"a\", r"a b\", r#"a\"b"#]),
            r#""C:\a b\c.exe" "" "a b" "a\"b" a\ "a b\\" "a\\\"b""#
        );
    }

    #[test]
    fn profiles_are_made_once_and_removed() {
        let name = format!("copiale-test-{}", std::process::id());
        let first = AppContainer::create(&name).unwrap();
        let again = AppContainer::create(&name).unwrap();
        // SAFETY: valid SIDs.
        assert_ne!(unsafe { EqualSid(first.0, again.0) }, 0);

        remove(&name).unwrap();
        remove(&name).unwrap();
    }

    /// Readable by this user, not by the sandbox.
    fn secret(exe: &Path) -> PathBuf {
        exe.with_extension("sandbox-secret")
    }

    #[test]
    fn the_helper_may_compute_and_nothing_else() {
        let exe = std::env::current_exe().unwrap();
        std::fs::write(secret(&exe), "secret").unwrap();
        assert!(Path::new(OTHER_PROGRAM).is_file());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port().to_string();
        let args = [
            "--exact",
            "video::sandbox::tests::sandboxed_child",
            "--test-threads=1",
            CHILD,
            port.as_str(),
        ]
        .map(OsStr::new);

        let (process, stdin, mut stdout) = spawn(PROFILE, &exe, &args, FRAME_CPU).unwrap();
        drop(stdin);
        let mut output = String::new();
        stdout.read_to_string(&mut output).ok();
        let code = process.wait().unwrap();

        std::fs::remove_file(secret(&exe)).unwrap();
        assert_eq!(code, 0, "{output}");
        assert!(!exe.with_extension("sandbox-written").exists());
        listener.set_nonblocking(true).unwrap();
        assert!(listener.accept().is_err());
    }

    /// In the sandboxed child: tries what it must be refused, exits with
    /// the number of the first that wasn't.
    #[test]
    fn sandboxed_child() {
        let mut args = std::env::args().skip_while(|arg| arg != CHILD).skip(1);
        let Some(port) = args.next() else {
            return;
        };
        let port: u16 = port.parse().unwrap();
        let exe = std::env::current_exe().unwrap();
        let attempts: [(i32, &dyn Fn() -> bool); 6] = [
            (10, &|| std::fs::read(secret(&exe)).is_ok()),
            (11, &|| {
                std::fs::write(exe.with_extension("sandbox-written"), "x").is_ok()
            }),
            (12, &|| TcpStream::connect(("127.0.0.1", port)).is_ok()),
            (13, &|| {
                std::process::Command::new(OTHER_PROGRAM).status().is_ok()
            }),
            (14, &executable_memory),
            (15, &|| Vec::<u8>::new().try_reserve(3 << 30).is_ok()),
        ];
        for (code, attempt) in attempts {
            if attempt() {
                std::process::exit(code);
            }
        }
        // Computing, on threads, still works.
        let sum = std::thread::spawn(|| (0..1000u64).sum::<u64>()).join();
        let buffer = vec![1u8; 64 * 1024 * 1024];
        let works = sum.is_ok_and(|sum| sum == 499_500) && buffer[42] == 1;
        std::process::exit(if works { 0 } else { 20 });
    }

    fn executable_memory() -> bool {
        // SAFETY: a new mapping, released if made.
        unsafe {
            let mapped = VirtualAlloc(
                std::ptr::null(),
                4096,
                MEM_COMMIT | MEM_RESERVE,
                PAGE_EXECUTE_READWRITE,
            );
            if mapped.is_null() {
                return false;
            }
            VirtualFree(mapped, 0, MEM_RELEASE);
            true
        }
    }
}
