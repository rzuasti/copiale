//! The main window, shown and hidden while the app keeps running.
//!
//! GPUI can't hide a window, so hiding closes it; the [`Shell`] outlives
//! it and attaches to the next one. With `switch_displays`, the app also
//! goes headless while hidden, and reconnects with the requesting
//! process's activation token so the new window may take focus on Wayland.

use gpui_kit::{
    AnyWindowHandle, App, Entity, Global, GraphicalEnvironment, WindowId, WindowingRequest,
};

use crate::tray::{self, TrayAction};
use crate::{Request, Shell, main_window_options, theme};

pub struct MainWindow {
    shell: Entity<Shell>,
    window: Option<AnyWindowHandle>,
    switch_displays: bool,
    /// Windows report the system appearance; see [`theme::follow_system`].
    follow_window_appearance: bool,
    /// The display to reconnect to, while headless.
    display: Option<GraphicalEnvironment>,
    /// A display switch is under way.
    switching: bool,
    /// Show once the display switch is done, with this activation token.
    show_after_switch: Option<Option<String>>,
    raise_by_reopening: bool,
    /// Open a new window once this one closes.
    reopen: bool,
}

impl Global for MainWindow {}

/// How the main window comes and goes.
#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    /// Go headless while hidden (Linux).
    pub switch_displays: bool,
    pub follow_window_appearance: bool,
    /// Raise by opening a new window: Wayland compositors may ignore
    /// activation requests (Hyprland by default) but focus new windows.
    pub raise_by_reopening: bool,
}

/// Manages `shell`'s window from now on; call [`show`] to open it, or
/// [`start_hidden`] to start without it.
pub fn install(shell: Entity<Shell>, options: Options, cx: &mut App) {
    cx.set_global(MainWindow {
        shell,
        window: None,
        switch_displays: options.switch_displays,
        follow_window_appearance: options.follow_window_appearance,
        display: None,
        switching: false,
        show_after_switch: None,
        raise_by_reopening: options.raise_by_reopening,
        reopen: false,
    });
    cx.on_window_closed(closed).detach();
}

/// The open main window.
pub fn handle(cx: &App) -> Option<AnyWindowHandle> {
    cx.try_global::<MainWindow>()?.window
}

/// Whether the main window is open and focused.
pub fn is_focused(cx: &mut App) -> bool {
    handle(cx).is_some_and(|handle| {
        handle
            .update(cx, |_, window, _| window.is_window_active())
            .unwrap_or(false)
    })
}

pub fn handle_request(request: Request, cx: &mut App) {
    match request {
        Request::Show { activation_token } => show(activation_token, cx),
        Request::Toggle { activation_token } => toggle(activation_token, cx),
        Request::Quit => cx.quit(),
    }
}

/// Tray clicks hide an open window, focused or not: clicking the tray
/// takes focus from it.
pub fn handle_tray(action: TrayAction, cx: &mut App) {
    match action {
        TrayAction::Activate if handle(cx).is_some() => hide(cx),
        TrayAction::Activate => show(None, cx),
        TrayAction::Quit => cx.quit(),
    }
}

/// Hides the window if focused, else shows it.
pub fn toggle(activation_token: Option<String>, cx: &mut App) {
    if is_focused(cx) {
        hide(cx);
    } else {
        show(activation_token, cx);
    }
}

/// Opens or raises the window, with the composer focused.
pub fn show(activation_token: Option<String>, cx: &mut App) {
    let Some(main) = cx.try_global::<MainWindow>() else {
        return;
    };
    let (shell, window, display) = (main.shell.clone(), main.window, main.display.clone());
    let (switching, switch_displays) = (main.switching, main.switch_displays);
    let raise_by_reopening = main.raise_by_reopening;
    if switching {
        cx.global_mut::<MainWindow>().show_after_switch = Some(activation_token);
        return;
    }
    match window {
        // A token goes with a new connection; without one, reopen in place.
        Some(_) if raise_by_reopening && !is_focused(cx) => {
            let main = cx.global_mut::<MainWindow>();
            if switch_displays && activation_token.is_some() {
                main.show_after_switch = Some(activation_token);
            } else {
                main.reopen = true;
            }
            hide(cx);
        }
        Some(handle) => {
            handle
                .update(cx, |_, window, _| window.activate_window())
                .ok();
            shell.update(cx, |shell, cx| shell.focus_composer(cx));
        }
        None => match display {
            Some(display) => reconnect(display, activation_token, cx),
            None => open(cx),
        },
    }
}

/// Closes the window; the app keeps running.
pub fn hide(cx: &mut App) {
    if let Some(handle) = handle(cx) {
        handle
            .update(cx, |_, window, _| window.remove_window())
            .ok();
    }
}

/// Leaves the window closed, as if hidden at once.
pub fn start_hidden(cx: &mut App) {
    if handle(cx).is_none() {
        go_headless(cx);
    }
}

fn open(cx: &mut App) {
    let main = cx.global::<MainWindow>();
    let shell = main.shell.clone();
    let follow = main.follow_window_appearance;
    let opened = gpui_kit::open_window(main_window_options(cx), cx, move |window, cx| {
        if follow {
            theme::follow_window(window, cx);
        }
        shell
    });
    match opened {
        Ok((handle, _)) => {
            cx.global_mut::<MainWindow>().window = Some(handle);
            tray::update(cx, |tray| tray.shown = true);
        }
        Err(error) => tracing::error!(%error, "can't open the main window"),
    }
}

fn closed(cx: &mut App, id: WindowId) {
    let Some(main) = cx.try_global::<MainWindow>() else {
        return;
    };
    if main.window.is_none_or(|handle| handle.window_id() != id) {
        return;
    }
    let shell = main.shell.clone();
    cx.global_mut::<MainWindow>().window = None;
    tray::update(cx, |tray| tray.shown = false);
    shell.update(cx, |shell, cx| shell.detach(cx));
    if std::mem::take(&mut cx.global_mut::<MainWindow>().reopen) {
        return open(cx);
    }
    go_headless(cx);
}

/// Disconnects from the display if `switch_displays` and no window is open.
fn go_headless(cx: &mut App) {
    if !cx.global::<MainWindow>().switch_displays || !cx.windows().is_empty() {
        return after_switch(cx);
    }
    let Some(display) = cx.graphical_environment() else {
        return after_switch(cx);
    };
    switch(WindowingRequest::Headless, Some(display), cx);
}

/// Connects to `display` again, then opens the window.
fn reconnect(display: GraphicalEnvironment, activation_token: Option<String>, cx: &mut App) {
    let display = GraphicalEnvironment {
        activation_token,
        ..display
    };
    cx.global_mut::<MainWindow>().show_after_switch = Some(None);
    switch(WindowingRequest::Windowed(display), None, cx);
}

/// Switches displays; `headless_from` is the display left behind, to
/// return to.
fn switch(request: WindowingRequest, headless_from: Option<GraphicalEnvironment>, cx: &mut App) {
    cx.global_mut::<MainWindow>().switching = true;
    let reconnecting = headless_from.is_none();
    let task = cx.request_windowing(request);
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| {
            let main = cx.global_mut::<MainWindow>();
            main.switching = false;
            match result {
                Ok(()) => main.display = headless_from,
                Err(error) => {
                    tracing::warn!(%error, "can't switch displays");
                    // Stay hidden rather than retry at once; the next show
                    // tries again.
                    if reconnecting {
                        main.show_after_switch = None;
                    }
                }
            }
            after_switch(cx);
        });
    })
    .detach();
}

/// Shows the window if asked to meanwhile.
fn after_switch(cx: &mut App) {
    let main = cx.global_mut::<MainWindow>();
    let Some(activation_token) = main.show_after_switch.take() else {
        return;
    };
    let display = main.display.clone();
    let windowed = !main.switch_displays || cx.graphical_environment().is_some();
    match display {
        Some(display) if !windowed => reconnect(display, activation_token, cx),
        _ => open(cx),
    }
}
