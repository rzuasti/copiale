//! `taskkill` closes every top-level window, the tray's hidden one too,
//! which would take the icon with it: quit instead.

use futures::channel::mpsc::UnboundedSender;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::{WM_CLOSE, WM_NCDESTROY};

use super::TrayAction;

/// Sends [`TrayAction::Quit`] on `WM_CLOSE` to `hwnd`, which stays open.
pub fn quit_on_close(hwnd: HWND, actions: UnboundedSender<TrayAction>) {
    let data = Box::into_raw(Box::new(actions));
    if unsafe { SetWindowSubclass(hwnd, Some(subclass), 0, data as usize) } == 0 {
        drop(unsafe { Box::from_raw(data) });
        tracing::warn!("closing the tray window will remove its icon");
    }
}

unsafe extern "system" fn subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _: usize,
    data: usize,
) -> LRESULT {
    let actions = data as *mut UnboundedSender<TrayAction>;
    match message {
        WM_CLOSE => {
            tracing::info!("tray window closed; quitting");
            unsafe { &*actions }.unbounded_send(TrayAction::Quit).ok();
            0
        }
        WM_NCDESTROY => {
            unsafe { RemoveWindowSubclass(hwnd, Some(subclass), 0) };
            drop(unsafe { Box::from_raw(actions) });
            unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
        }
        _ => unsafe { DefSubclassProc(hwnd, message, wparam, lparam) },
    }
}

#[cfg(test)]
mod tests {
    use futures::channel::mpsc::unbounded;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, HWND_MESSAGE, IsWindow, SendMessageW,
    };
    use windows_sys::w;

    use super::*;

    #[test]
    fn closing_quits_and_leaves_the_window() {
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                w!("STATIC"),
                std::ptr::null(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        assert!(!hwnd.is_null());
        let (actions, mut received) = unbounded();
        quit_on_close(hwnd, actions);

        unsafe { SendMessageW(hwnd, WM_CLOSE, 0, 0) };
        assert_eq!(received.try_recv().ok(), Some(TrayAction::Quit));
        assert_ne!(unsafe { IsWindow(hwnd) }, 0, "still open");

        assert_ne!(unsafe { DestroyWindow(hwnd) }, 0);
        assert!(received.try_recv().is_err(), "sender dropped");
    }
}
