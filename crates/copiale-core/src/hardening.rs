//! Keeps secrets in memory (keys, passphrase, messages) from reaching disk
//! through core dumps, and keys and passphrases through swap.
//!
//! `COPIALE_UNSAFE_CORE_DUMPS=1` skips disabling core dumps, for debugging
//! crashes; core dumps can then hold the database key.

use std::io;
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};

use zeroize::Zeroize as _;

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

/// Fixed-size buffer on pages of its own, locked in RAM so it's never
/// swapped out, and left out of core dumps on Linux. Starts zeroed; zeroed
/// on drop.
///
/// Locking is best effort: the first failure (e.g. `RLIMIT_MEMLOCK`
/// reached) is logged.
pub struct LockedBytes {
    ptr: NonNull<u8>,
    len: usize,
}

// SAFETY: owns its memory exclusively, like `Box<[u8]>`.
unsafe impl Send for LockedBytes {}
// SAFETY: no interior mutability.
unsafe impl Sync for LockedBytes {}

impl LockedBytes {
    /// Panics if the memory can't be allocated.
    pub fn new(len: usize) -> Self {
        let ptr = pages::alloc(Self::size(len));
        // SAFETY: just allocated with this size.
        if let Err(error) = unsafe { pages::lock(ptr, Self::size(len)) } {
            static WARNED: AtomicBool = AtomicBool::new(false);
            if !WARNED.swap(true, Ordering::Relaxed) {
                tracing::warn!(%error, "can't lock secrets in memory; they may be swapped out");
            }
        }
        Self { ptr, len }
    }

    /// Mapped size; mappings can't be empty.
    fn size(len: usize) -> usize {
        len.max(1)
    }
}

impl Deref for LockedBytes {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        // SAFETY: `len` initialized bytes, owned by `self`.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }
}

impl DerefMut for LockedBytes {
    fn deref_mut(&mut self) -> &mut [u8] {
        // SAFETY: `len` initialized bytes, owned by `self`.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}

impl Drop for LockedBytes {
    fn drop(&mut self) {
        self.zeroize();
        // SAFETY: allocated with this size; not used again.
        unsafe { pages::free(self.ptr, Self::size(self.len)) };
    }
}

/// Page-granular allocation, so locks never cover other data.
#[cfg(unix)]
mod pages {
    use std::io;
    use std::ptr::NonNull;

    /// Zeroed pages covering `size` bytes.
    pub(super) fn alloc(size: usize) -> NonNull<u8> {
        // SAFETY: a new private anonymous mapping.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            panic!("can't map memory: {}", io::Error::last_os_error());
        }
        NonNull::new(ptr.cast()).expect("mmap succeeded")
    }

    /// # Safety
    /// `ptr` and `size` must come from [`alloc`].
    pub(super) unsafe fn lock(ptr: NonNull<u8>, size: usize) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        // SAFETY: our own mapping.
        if unsafe { libc::madvise(ptr.as_ptr().cast(), size, libc::MADV_DONTDUMP) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: our own mapping.
        if unsafe { libc::mlock(ptr.as_ptr().cast(), size) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Unmapping also unlocks.
    ///
    /// # Safety
    /// `ptr` and `size` must come from [`alloc`]; the memory isn't used
    /// again.
    pub(super) unsafe fn free(ptr: NonNull<u8>, size: usize) {
        // SAFETY: our own mapping, per the caller.
        unsafe { libc::munmap(ptr.as_ptr().cast(), size) };
    }
}

#[cfg(windows)]
mod pages {
    use std::io;
    use std::ptr::NonNull;

    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc, VirtualFree,
        VirtualLock, VirtualUnlock,
    };

    /// Zeroed pages covering `size` bytes.
    pub(super) fn alloc(size: usize) -> NonNull<u8> {
        // SAFETY: a new private allocation.
        let ptr = unsafe {
            VirtualAlloc(
                std::ptr::null(),
                size,
                MEM_COMMIT | MEM_RESERVE,
                PAGE_READWRITE,
            )
        };
        NonNull::new(ptr.cast())
            .unwrap_or_else(|| panic!("can't allocate memory: {}", io::Error::last_os_error()))
    }

    /// # Safety
    /// `ptr` and `size` must come from [`alloc`].
    pub(super) unsafe fn lock(ptr: NonNull<u8>, size: usize) -> io::Result<()> {
        // SAFETY: our own allocation.
        if unsafe { VirtualLock(ptr.as_ptr().cast(), size) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// # Safety
    /// `ptr` and `size` must come from [`alloc`]; the memory isn't used
    /// again.
    pub(super) unsafe fn free(ptr: NonNull<u8>, size: usize) {
        // SAFETY: our own allocation, per the caller.
        unsafe {
            VirtualUnlock(ptr.as_ptr().cast(), size);
            VirtualFree(ptr.as_ptr().cast(), 0, MEM_RELEASE);
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod pages {
    use std::io;
    use std::ptr::NonNull;

    pub(super) fn alloc(size: usize) -> NonNull<u8> {
        let bytes = Box::into_raw(vec![0u8; size].into_boxed_slice());
        NonNull::new(bytes.cast()).expect("Box is non-null")
    }

    pub(super) unsafe fn lock(_: NonNull<u8>, _: usize) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }

    pub(super) unsafe fn free(ptr: NonNull<u8>, size: usize) {
        // SAFETY: from `alloc`, per the caller.
        drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr.as_ptr(), size)) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locked_bytes_start_zeroed_and_are_writable() {
        for len in [0, 1, 32, 5000] {
            let mut bytes = LockedBytes::new(len);
            assert_eq!(bytes.len(), len);
            assert!(bytes.iter().all(|&byte| byte == 0));
            bytes.fill(7);
            assert!(bytes.iter().all(|&byte| byte == 7));
        }
    }

    /// The mapping's flags: `lo` locked, `dd` left out of core dumps.
    #[cfg(target_os = "linux")]
    #[test]
    fn locked_bytes_are_locked_and_not_dumped() {
        let bytes = LockedBytes::new(32);
        let address = bytes.as_ptr() as usize;
        let contains = |line: &str| -> Option<bool> {
            let (start, end) = line.split_once(' ')?.0.split_once('-')?;
            let range =
                usize::from_str_radix(start, 16).ok()?..usize::from_str_radix(end, 16).ok()?;
            Some(range.contains(&address))
        };
        let smaps = std::fs::read_to_string("/proc/self/smaps").unwrap();
        let mapping: Vec<_> = smaps
            .lines()
            .skip_while(|line| contains(line) != Some(true))
            .find_map(|line| line.strip_prefix("VmFlags:"))
            .expect("mapping listed")
            .split_whitespace()
            .collect();
        assert!(mapping.contains(&"dd"), "{mapping:?}");

        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: `limit` is a valid `rlimit`.
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_MEMLOCK, &mut limit) },
            0
        );
        // Parallel tests could use up a small limit.
        if limit.rlim_cur >= 1 << 20 {
            assert!(mapping.contains(&"lo"), "{mapping:?}");
        }
    }
}
