//! The tray icon: shows and hides the window, quits, and marks unread
//! chats.

#[cfg(windows)]
mod close;
mod icon;

use std::rc::Rc;

use futures::channel::mpsc::{UnboundedReceiver, unbounded};
use gpui_kit::{App, Global};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

/// Drawn at this size; hosts scale it.
const ICON_SIZE: u32 = 32;

/// What the tray shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TrayState {
    /// The window is open.
    pub shown: bool,
    /// A chat has unread messages.
    pub unread: bool,
}

/// What the user picked in the tray.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayAction {
    /// Click, or the menu's Show/Hide: hides an open window, else shows it.
    Activate,
    Quit,
}

/// Shows [`TrayState`]; a fake in tests.
pub trait Tray {
    fn set(&self, state: TrayState);
}

/// The tray in use and what it shows.
struct ActiveTray {
    tray: Rc<dyn Tray>,
    state: TrayState,
}

impl Global for ActiveTray {}

/// Shows `tray` from now on.
pub fn install(tray: Rc<dyn Tray>, cx: &mut App) {
    let state = TrayState::default();
    tray.set(state);
    cx.set_global(ActiveTray { tray, state });
}

/// Changes what the tray shows, if there is one.
pub fn update(cx: &mut App, change: impl FnOnce(&mut TrayState)) {
    let Some(active) = cx.try_global::<ActiveTray>() else {
        return;
    };
    let mut state = active.state;
    change(&mut state);
    if state != active.state {
        active.tray.set(state);
        cx.global_mut::<ActiveTray>().state = state;
    }
}

/// The system's tray icon.
pub struct SystemTray {
    icon: TrayIcon,
    window_item: MenuItem,
}

impl SystemTray {
    /// Adds the icon; its actions arrive on the receiver.
    pub fn new() -> Result<(Self, UnboundedReceiver<TrayAction>), Box<dyn std::error::Error>> {
        let window_item = MenuItem::with_id("window", "Show Copiale", true, None);
        let quit = MenuItem::with_id("quit", "Quit", true, None);
        let menu = Menu::new();
        menu.append_items(&[&window_item, &quit])?;
        let icon = TrayIconBuilder::new()
            .with_id(copiale_core::APP_ID)
            .with_title("Copiale")
            .with_tooltip("Copiale")
            .with_icon(icon(false))
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()?;

        let (actions, received) = unbounded();
        #[cfg(windows)]
        close::quit_on_close(icon.window_handle(), actions.clone());
        let clicks = actions.clone();
        TrayIconEvent::set_event_handler(Some(move |event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                clicks.unbounded_send(TrayAction::Activate).ok();
            }
        }));
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let action = match event.id.as_ref() {
                "window" => TrayAction::Activate,
                "quit" => TrayAction::Quit,
                _ => return,
            };
            actions.unbounded_send(action).ok();
        }));
        Ok((Self { icon, window_item }, received))
    }
}

impl Tray for SystemTray {
    fn set(&self, state: TrayState) {
        if let Err(error) = self.icon.set_icon(Some(icon(state.unread))) {
            tracing::warn!(%error, "can't update the tray icon");
        }
        let tooltip = if state.unread {
            "Copiale · unread messages"
        } else {
            "Copiale"
        };
        self.icon.set_tooltip(Some(tooltip)).ok();
        self.window_item.set_text(if state.shown {
            "Hide Copiale"
        } else {
            "Show Copiale"
        });
    }
}

fn icon(unread: bool) -> Icon {
    Icon::from_rgba(icon::rgba(ICON_SIZE, unread), ICON_SIZE, ICON_SIZE)
        .expect("icon pixels match its size")
}
