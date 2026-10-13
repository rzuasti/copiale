//! Views and theming for Copiale.

mod shell;

pub use shell::Shell;

use gpui_kit::{
    App, AppContext as _, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size,
};

pub use copiale_core::APP_ID;

/// Runs the UI event loop with the main window open; returns on quit.
pub fn run() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            gpui_kit::open_window(main_window_options(cx), cx, |_, cx| cx.new(|_| Shell))
                .expect("open main window");
        });
}

/// Options for the main window: centered, titled, tagged with [`APP_ID`].
pub fn main_window_options(cx: &App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(1100.), px(720.)),
            cx,
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some("Copiale".into()),
            ..Default::default()
        }),
        app_id: Some(APP_ID.into()),
        ..Default::default()
    }
}
