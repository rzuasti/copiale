//! Screen capture protection: hides the window from screenshots and screen
//! shares where the OS lets an app ask for it.

use gpui_kit::Window;

/// Whether this platform can apply it. Wayland and X11 have no way for an
/// app to ask; some compositors offer a window rule.
pub const SUPPORTED: bool = cfg!(any(windows, target_os = "macos"));

/// Why the setting can't change here.
pub fn unsupported_hint() -> &'static str {
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() {
        "use Hyprland's no_screen_share window rule"
    } else {
        "not supported on this desktop"
    }
}

/// Protects `window` from capture, or stops; logs a failure.
pub fn protect(window: &Window, on: bool) {
    if let Err(error) = platform::protect(window, on) {
        tracing::warn!("screen capture protection not applied: {error}");
    }
}

#[cfg(windows)]
mod platform {
    use gpui_kit::Window;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE, WDA_NONE,
    };

    pub fn protect(window: &Window, on: bool) -> Result<(), String> {
        let handle = HasWindowHandle::window_handle(window).map_err(|e| e.to_string())?;
        let RawWindowHandle::Win32(handle) = handle.as_raw() else {
            return Err("not a Win32 window".into());
        };
        let affinity = if on { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE };
        // Before Windows 10 2004, `WDA_EXCLUDEFROMCAPTURE` fails.
        if unsafe { SetWindowDisplayAffinity(handle.hwnd.get() as _, affinity) } == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use gpui_kit::Window;
    use objc2_app_kit::{NSView, NSWindowSharingType};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    pub fn protect(window: &Window, on: bool) -> Result<(), String> {
        let handle = HasWindowHandle::window_handle(window).map_err(|e| e.to_string())?;
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            return Err("not an AppKit window".into());
        };
        // The handle's view lives as long as `window`, on this (main) thread.
        let view = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
        let ns_window = view.window().ok_or("view has no window")?;
        ns_window.setSharingType(if on {
            NSWindowSharingType::None
        } else {
            NSWindowSharingType::ReadOnly
        });
        Ok(())
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod platform {
    use gpui_kit::Window;

    pub fn protect(_: &Window, _: bool) -> Result<(), String> {
        Ok(())
    }
}
