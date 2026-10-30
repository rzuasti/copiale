//! Copiale is a GUI program on Windows, without a console of its own: it
//! borrows the one it was started from, if any, for command-line output.

use windows_sys::Win32::Storage::FileSystem::{FILE_TYPE_CHAR, GetFileType};
use windows_sys::Win32::System::Console::{
    ATTACH_PARENT_PROCESS, AttachConsole, FreeConsole, GetStdHandle, STD_ERROR_HANDLE, STD_HANDLE,
    STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
};

const STD_HANDLES: [STD_HANDLE; 3] = [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE];

/// Sends stdout and stderr to the console Copiale was started from.
pub fn attach() {
    // SAFETY: plain call; fails harmlessly without a parent console.
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

/// Lets go of the console: closing it would end this process too. Standard
/// handles on it are cleared: files opened later could reuse their values.
pub fn detach() {
    // SAFETY: plain calls; an invalid handle is `FILE_TYPE_UNKNOWN`.
    let on_console =
        STD_HANDLES.map(|std| unsafe { GetFileType(GetStdHandle(std)) } == FILE_TYPE_CHAR);
    // SAFETY: plain call; fails harmlessly without a console.
    unsafe { FreeConsole() };
    for (std, on_console) in STD_HANDLES.into_iter().zip(on_console) {
        if on_console {
            // SAFETY: a null handle is no handle, which std writes skip.
            unsafe { SetStdHandle(std, std::ptr::null_mut()) };
        }
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use windows_sys::Win32::System::Console::AllocConsole;

    use super::*;

    /// Consoles are per process, so this runs in a process of its own.
    #[test]
    #[ignore = "run by detaching_clears_the_console_handles"]
    fn detaching_process() {
        unsafe {
            FreeConsole();
            for std in STD_HANDLES {
                SetStdHandle(std, std::ptr::null_mut());
            }
            assert_ne!(AllocConsole(), 0);
        }
        assert!(
            STD_HANDLES
                .iter()
                .all(|&std| !unsafe { GetStdHandle(std) }.is_null())
        );

        detach();
        assert!(
            STD_HANDLES
                .iter()
                .all(|&std| unsafe { GetStdHandle(std) }.is_null())
        );
        eprintln!("written nowhere");
    }

    #[test]
    fn detaching_clears_the_console_handles() {
        let output = {
            let _spawning = crate::spawn_guard();
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "console::tests::detaching_process", "--ignored"])
                .output()
                .unwrap()
        };
        assert!(output.status.success(), "{output:?}");
    }
}
