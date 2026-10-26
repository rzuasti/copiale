//! Confines the video helper before it reads untrusted input: it may read
//! stdin, write stdout and compute, nothing else; playing, on threads.
//!
//! - Linux: resource limits, then a seccomp filter allowing only those
//!   syscalls, and no executable memory; anything else fails with `ENOSYS`.
//! - macOS: resource limits, then the system's pure computation sandbox
//!   profile: no files, network or other processes.

use std::io;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// One frame, on one thread.
    Frame,
    /// A whole video, on threads.
    Play,
}

impl Mode {
    /// Seconds of CPU before the kernel ends it.
    fn max_cpu_seconds(self) -> u64 {
        match self {
            Self::Frame => 20,
            Self::Play => 60 * 60,
        }
    }
}

/// Confines this process for good.
pub fn enter(mode: Mode) -> io::Result<()> {
    imp::enter(mode)
}

/// Limits CPU time and open files; on Linux, memory too.
fn limit_resources(mode: Mode) -> io::Result<()> {
    let limit = |resource, value: u64| -> io::Result<()> {
        let limit = libc::rlimit {
            rlim_cur: value as libc::rlim_t,
            rlim_max: value as libc::rlim_t,
        };
        // SAFETY: a valid resource and limit.
        match unsafe { libc::setrlimit(resource, &limit) } {
            0 => Ok(()),
            _ => Err(io::Error::last_os_error()),
        }
    };
    limit(libc::RLIMIT_CPU, mode.max_cpu_seconds())?;
    limit(libc::RLIMIT_NOFILE, 3)?;
    // macOS doesn't enforce address space limits.
    #[cfg(target_os = "linux")]
    limit(libc::RLIMIT_AS, 2 * 1024 * 1024 * 1024)?;
    Ok(())
}

#[cfg(target_os = "linux")]
mod imp {
    use std::collections::BTreeMap;
    use std::io;

    use super::Mode;
    use seccompiler::{
        BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
        SeccompRule,
    };

    /// Syscalls reading, writing, allocating memory and ending need.
    const ALLOWED: &[libc::c_long] = &[
        libc::SYS_read,
        libc::SYS_write,
        libc::SYS_close,
        libc::SYS_brk,
        libc::SYS_munmap,
        libc::SYS_mremap,
        libc::SYS_madvise,
        libc::SYS_futex,
        libc::SYS_getrandom,
        libc::SYS_sched_getaffinity,
        libc::SYS_sched_yield,
        libc::SYS_clock_gettime,
        libc::SYS_rt_sigreturn,
        libc::SYS_rt_sigprocmask,
        libc::SYS_rt_sigaction,
        libc::SYS_sigaltstack,
        libc::SYS_getpid,
        libc::SYS_gettid,
        libc::SYS_tgkill,
        libc::SYS_exit,
        libc::SYS_exit_group,
    ];

    /// What threads need besides `clone`, when playing.
    const THREADS: &[libc::c_long] = &[libc::SYS_set_robust_list, libc::SYS_rseq];

    /// Mapped only without execution, so injected code can't run.
    const NOT_EXECUTABLE: &[libc::c_long] = &[libc::SYS_mmap, libc::SYS_mprotect];

    pub fn enter(mode: Mode) -> io::Result<()> {
        super::limit_resources(mode)?;
        let mut rules: BTreeMap<i64, Vec<SeccompRule>> = ALLOWED
            .iter()
            .chain(if mode == Mode::Play { THREADS } else { &[] })
            .map(|syscall| (*syscall, Vec::new()))
            .collect();
        let no_exec = SeccompCondition::new(
            2,
            SeccompCmpArgLen::Dword,
            SeccompCmpOp::MaskedEq(libc::PROT_EXEC as u64),
            0,
        )
        .map_err(io::Error::other)?;
        let rule = SeccompRule::new(vec![no_exec]).map_err(io::Error::other)?;
        for syscall in NOT_EXECUTABLE {
            rules.insert(*syscall, vec![rule.clone()]);
        }
        if mode == Mode::Play {
            // Threads only: no new processes. `clone3`'s flags can't be
            // checked, so it fails and the C library falls back to this.
            let thread = SeccompCondition::new(
                0,
                SeccompCmpArgLen::Dword,
                SeccompCmpOp::MaskedEq(libc::CLONE_THREAD as u64),
                libc::CLONE_THREAD as u64,
            )
            .map_err(io::Error::other)?;
            let rule = SeccompRule::new(vec![thread]).map_err(io::Error::other)?;
            rules.insert(libc::SYS_clone, vec![rule]);
        }
        let filter = SeccompFilter::new(
            rules,
            // What C libraries take as "not here", falling back where they
            // can.
            SeccompAction::Errno(libc::ENOSYS as u32),
            SeccompAction::Allow,
            std::env::consts::ARCH
                .try_into()
                .map_err(io::Error::other)?,
        )
        .map_err(io::Error::other)?;
        let program: BpfProgram = filter.try_into().map_err(io::Error::other)?;
        // Also sets no_new_privs.
        seccompiler::apply_filter(&program).map_err(io::Error::other)
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::{CStr, c_char, c_int};
    use std::io;

    use super::Mode;

    /// `kSBXProfilePureComputation`.
    const PURE_COMPUTATION: &CStr = c"pure-computation";
    /// `SANDBOX_NAMED`: the profile is a built-in one's name.
    const NAMED: u64 = 1;

    unsafe extern "C" {
        fn sandbox_init(profile: *const c_char, flags: u64, error: *mut *mut c_char) -> c_int;
        fn sandbox_free_error(error: *mut c_char);
    }

    pub fn enter(mode: Mode) -> io::Result<()> {
        super::limit_resources(mode)?;
        let mut error: *mut c_char = std::ptr::null_mut();
        // SAFETY: a valid profile name and error out-pointer, freed below.
        let result = unsafe { sandbox_init(PURE_COMPUTATION.as_ptr(), NAMED, &mut error) };
        if result == 0 {
            return Ok(());
        }
        let message = if error.is_null() {
            "sandbox_init failed".to_owned()
        } else {
            // SAFETY: a NUL-terminated message from sandbox_init.
            let message = unsafe { CStr::from_ptr(error) }
                .to_string_lossy()
                .into_owned();
            unsafe { sandbox_free_error(error) };
            message
        };
        Err(io::Error::other(message))
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::Mode;

    /// Set in the copy of the test binary that enters the sandbox, to the
    /// mode.
    const CHILD: &str = "COPIALE_SANDBOX_CHILD";

    /// Runs [`sandboxed_child`] in a copy of this test binary.
    fn run_child(mode: &str) {
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "video::sandbox::tests::sandboxed_child",
                "--test-threads=1",
            ])
            .env(CHILD, mode)
            .status()
            .unwrap();
        assert!(status.success(), "{mode}: {status}");
    }

    #[test]
    fn for_a_frame_the_sandbox_allows_nothing_but_computing() {
        run_child("frame");
    }

    #[test]
    fn playing_the_sandbox_allows_threads_too() {
        run_child("play");
    }

    /// In the child: enters the sandbox, then tries what it must refuse;
    /// exits with the number of the first thing that went wrong.
    #[test]
    fn sandboxed_child() {
        let mode = match std::env::var(CHILD).as_deref() {
            Ok("frame") => Mode::Frame,
            Ok("play") => Mode::Play,
            _ => return,
        };
        super::enter(mode).unwrap();
        let attempts: [(i32, &dyn Fn() -> bool); 6] = [
            (10, &|| std::fs::File::open("/etc/passwd").is_ok()),
            (11, &|| std::fs::write("copiale-sandbox-test", "x").is_ok()),
            (12, &|| std::net::TcpStream::connect("127.0.0.1:9").is_ok()),
            (13, &|| Command::new("true").status().is_ok()),
            (14, &executable_memory),
            (15, &forks),
        ];
        for (code, attempt) in attempts {
            if attempt() {
                std::process::exit(code);
            }
        }
        let thread = std::thread::Builder::new().spawn(|| (0..1000u64).sum::<u64>());
        let threads = thread.is_ok_and(|thread| thread.join().is_ok_and(|sum| sum == 499_500));
        if threads != (mode == Mode::Play) {
            std::process::exit(16);
        }
        // Computing still works.
        let sum: u64 = (0..1000u64).sum();
        let buffer = vec![1u8; 64 * 1024 * 1024];
        std::process::exit(if sum == 499_500 && buffer[42] == 1 {
            0
        } else {
            20
        });
    }

    /// Whether memory can be mapped executable.
    fn executable_memory() -> bool {
        // SAFETY: an anonymous mapping, unmapped if made.
        unsafe {
            let mapped = libc::mmap(
                std::ptr::null_mut(),
                4096,
                libc::PROT_READ | libc::PROT_EXEC,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            );
            if mapped == libc::MAP_FAILED {
                return false;
            }
            libc::munmap(mapped, 4096);
            true
        }
    }

    /// Whether a process can be forked; the fork exits at once.
    fn forks() -> bool {
        // SAFETY: the child only exits.
        match unsafe { libc::fork() } {
            0 => unsafe { libc::_exit(0) },
            pid => pid > 0,
        }
    }
}
