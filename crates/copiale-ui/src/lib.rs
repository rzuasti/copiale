//! Views and theming for Copiale.

pub mod chat;
mod drawer;
mod fuzzy;
mod kinetic;
pub mod main_window;
pub mod notifier;
pub mod onboarding;
mod qr;
pub mod settings;
mod shell;
pub mod theme;
pub mod titlebar;
pub mod tray;

use std::cell::RefCell;
use std::rc::Rc;

pub use copiale_core::APP_ID;
use copiale_core::config::ConfigFile;
use copiale_core::{Command, CoreHandle, Events};
use futures::StreamExt as _;
use futures::channel::mpsc::UnboundedReceiver;
use gpui_kit::component::TitleBar;
use gpui_kit::{
    App, AppContext as _, Bounds, KeyBinding, TitlebarOptions, WindowBounds, WindowDecorations,
    WindowOptions, actions, px, size,
};
use notifier::{DesktopNotifier, NoNotifier, Notifier};
pub use qr::QrMatrix;
pub use shell::Shell;
use tray::SystemTray;

actions!(copiale, [Quit]);

/// What another `copiale` process asks of the running one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// Show the window and focus the composer.
    Show {
        activation_token: Option<String>,
    },
    /// Show the window, or hide it if focused.
    Toggle {
        activation_token: Option<String>,
    },
    Quit,
}

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
/// shutting down `core`. Closing the window keeps running; `requests` come
/// from other `copiale` processes.
pub fn run(core: CoreHandle, events: Events, mut requests: UnboundedReceiver<Request>) {
    let core = Rc::new(RunningCore(RefCell::new(Some(core))));
    let app = gpui_kit::application().with_assets(gpui_kit::assets::Assets);
    // The Dock icon on macOS.
    app.on_reopen(|cx| main_window::show(None, cx));
    app.run(move |cx| {
        init(cx);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_app_quit({
            let core = core.clone();
            move |_| {
                core.shutdown();
                async {}
            }
        })
        .detach();

        match ConfigFile::platform() {
            Some(file) => settings::load(file, cx),
            None => tracing::warn!("no config directory; using default settings"),
        }
        let follows_system = theme::follow_system(cx);
        let (notifier, clicks): (Rc<dyn Notifier>, _) = match DesktopNotifier::new() {
            Ok((notifier, clicks)) => (Rc::new(notifier), Some(clicks)),
            Err(error) => {
                tracing::warn!(%error, "notifications unavailable");
                (Rc::new(NoNotifier), None)
            }
        };
        let sink: Rc<dyn CoreSink> = core;
        let shell = cx.new(|cx| {
            let mut shell = Shell::new(sink, notifier);
            shell.listen(events, cx);
            if let Some(clicks) = clicks {
                shell.listen_to_notifications(clicks, cx);
            }
            shell
        });
        let options = main_window::Options {
            switch_displays: cfg!(target_os = "linux"),
            follow_window_appearance: !follows_system,
            raise_by_reopening: cx.compositor_name() == "Wayland",
        };
        main_window::install(shell, options, cx);
        match SystemTray::new() {
            Ok((system_tray, mut actions)) => {
                tray::install(Rc::new(system_tray), cx);
                cx.spawn(async move |cx| {
                    while let Some(action) = actions.next().await {
                        cx.update(|cx| main_window::handle_tray(action, cx));
                    }
                })
                .detach();
            }
            Err(error) => tracing::warn!(%error, "no tray icon"),
        }
        main_window::show(None, cx);
        cx.spawn(async move |cx| {
            while let Some(request) = requests.next().await {
                cx.update(|cx| main_window::handle_request(request, cx));
            }
        })
        .detach();
    });
}

/// Initializes GPUI Kit and registers key bindings. [`Quit`] is left for
/// the caller to handle.
pub fn init(cx: &mut App) {
    gpui_kit::init(cx);
    cx.bind_keys([KeyBinding::new("secondary-q", Quit, None)]);
    chat::init(cx);
    theme::watch_settings(cx);
}

/// Options for the main window: centered, titled, tagged with [`APP_ID`],
/// with room for [`titlebar`].
pub fn main_window_options(cx: &App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(1100.), px(720.)),
            cx,
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some("Copiale".into()),
            ..TitleBar::title_bar_options()
        }),
        app_owns_titlebar_drag: true,
        window_decorations: Some(WindowDecorations::Server),
        app_id: Some(APP_ID.into()),
        ..Default::default()
    }
}
