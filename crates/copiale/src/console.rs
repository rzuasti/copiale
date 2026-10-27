//! Copiale is a GUI program on Windows, without a console of its own: it
//! borrows the one it was started from, if any, for command-line output.

use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole, FreeConsole};

/// Sends stdout and stderr to the console Copiale was started from.
pub fn attach() {
    // SAFETY: plain call; fails harmlessly without a parent console.
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

/// Lets go of the console: closing it would end this process too.
pub fn detach() {
    // SAFETY: plain call; fails harmlessly without a console.
    unsafe { FreeConsole() };
}
