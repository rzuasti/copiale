//! Views and theming for Copiale.

pub mod chat;
mod drawer;
mod fuzzy;
mod kinetic;
pub mod onboarding;
mod qr;
mod shell;
pub mod theme;

use std::cell::RefCell;
use std::rc::Rc;

pub use copiale_core::APP_ID;
use copiale_core::{Command, CoreHandle, Events};
use gpui_kit::{
    App, AppContext as _, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size,
};
pub use qr::QrMatrix;
pub use shell::Shell;

/// Where the UI sends commands: the running core, or a fake in tests.
pub trait CoreSink {
    fn send(&self, command: Command);
}

/// The core, until the app quits.
struct RunningCore(RefCell<Option<CoreHandle>>);

impl CoreSink for RunningCore {
    fn send(&self, command: Command) {
        if let Some(core) = &*self.0.borrow()
            && core.send(command).is_err()
        {
            tracing::warn!("command dropped: core has stopped");
        }
    }
}

impl RunningCore {
    /// Waits for the core to close storage; see `Database::close`.
    fn shutdown(&self) {
        if let Some(core) = self.0.borrow_mut().take()
            && core.shutdown().is_err()
        {
            tracing::error!("core thread panicked");
        }
    }
}

/// Runs the UI event loop with the main window open; returns on quit, after
/// shutting down `core`.
pub fn run(core: CoreHandle, events: Events) {
    let core = Rc::new(RunningCore(RefCell::new(Some(core))));
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            init(cx);
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            cx.on_app_quit({
                let core = core.clone();
                move |_| {
                    core.shutdown();
                    async {}
                }
            })
            .detach();

            let sink: Rc<dyn CoreSink> = core;
            gpui_kit::open_window(main_window_options(cx), cx, |window, cx| {
                theme::init(window, cx);
                cx.new(|cx| {
                    let mut shell = Shell::new(sink, window, cx);
                    shell.listen(events, window, cx);
                    shell
                })
            })
            .expect("open main window");
        });
}

/// Initializes GPUI Kit and registers key bindings.
pub fn init(cx: &mut App) {
    gpui_kit::init(cx);
    chat::init(cx);
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
