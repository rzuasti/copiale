//! The titlebar the app draws where it owns the window decorations.
//!
//! macOS and Windows hide the system titlebar for ours (macOS keeps its
//! traffic lights). Linux asks for server-side decorations; ours shows only
//! when the compositor leaves them to the client (GNOME). Tiling
//! compositors grant server-side and draw nothing.

use gpui_kit::component::TitleBar;
use gpui_kit::{
    AnyElement, App, Decorations, IntoElement, ParentElement as _, Styled as _, Window, div,
};

use crate::theme::ActivePalette as _;

/// Whether the app draws the titlebar.
pub fn shown(window: &Window) -> bool {
    draws_titlebar(cfg!(target_os = "linux"), window.window_decorations())
}

fn draws_titlebar(linux: bool, decorations: Decorations) -> bool {
    !linux || matches!(decorations, Decorations::Client { .. })
}

/// The titlebar, if [`shown`].
pub fn render(window: &Window, cx: &App) -> Option<AnyElement> {
    shown(window).then(|| titlebar(cx))
}

/// Drags and double-click maximizes natively; window controls follow the
/// platform's (none on macOS, where the system draws them).
pub fn titlebar(cx: &App) -> AnyElement {
    let palette = cx.palette();
    TitleBar::new()
        .bg(palette.sidebar)
        .border_color(palette.border)
        // Closing keeps the app running; see `main_window`.
        .on_close_window(|_, window, _| window.remove_window())
        .child(
            div()
                .flex_1()
                .flex()
                .justify_center()
                .text_sm()
                .text_color(palette.text_muted)
                .child("Copiale"),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use gpui_kit::Tiling;

    use super::*;

    #[test]
    fn drawn_unless_linux_grants_server_decorations() {
        let client = Decorations::Client {
            tiling: Tiling::default(),
        };
        assert!(draws_titlebar(false, Decorations::Server));
        assert!(draws_titlebar(true, client));
        assert!(!draws_titlebar(true, Decorations::Server));
    }
}
