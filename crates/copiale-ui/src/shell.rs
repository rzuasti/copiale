use std::rc::Rc;

use copiale_core::{Event, Events};
use futures::StreamExt as _;
use futures::channel::mpsc::UnboundedReceiver;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{Disableable as _, v_flex};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Subscription, Task, Window, WindowBackgroundAppearance, WindowId,
    div, px,
};

use crate::chat::{ChatView, NoticeKey};
use crate::notifier::Notifier;
use crate::onboarding::{Onboarding, Screen, describe};
use crate::qr::{QrMatrix, qr_code};
use crate::settings::Settings;
use crate::theme::ActivePalette as _;
use crate::{CoreSink, main_window, titlebar};

/// Root view of the main window: onboarding until the core is ready, then
/// the chats. Outlives the window: hiding closes it, and the next one shows
/// this again.
pub struct Shell {
    core: Rc<dyn CoreSink>,
    notifier: Rc<dyn Notifier>,
    onboarding: Onboarding,
    chat: Option<Entity<ChatView>>,
    inputs: Option<Inputs>,
    /// The screen's input needs focus once a window shows it.
    focus_pending: bool,
    /// QR code for the current provisioning URL.
    qr: Option<(String, Option<QrMatrix>)>,
    /// The window background last set, and for which window.
    background: Option<(WindowId, WindowBackgroundAppearance)>,
    _events: Option<Task<()>>,
    _clicks: Option<Task<()>>,
}

/// The onboarding inputs, made for the window showing them.
struct Inputs {
    window: WindowId,
    passphrase: Entity<InputState>,
    confirmation: Entity<InputState>,
    device_name: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl Shell {
    pub fn new(core: Rc<dyn CoreSink>, notifier: Rc<dyn Notifier>) -> Self {
        Self {
            core,
            notifier,
            onboarding: Onboarding::default(),
            chat: None,
            inputs: None,
            focus_pending: true,
            qr: None,
            background: None,
            _events: None,
            _clicks: None,
        }
    }

    /// Makes the inputs for `window`, unless it already has them.
    fn attach(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = window.window_handle().window_id();
        if self
            .inputs
            .as_ref()
            .is_some_and(|inputs| inputs.window == id)
        {
            return;
        }
        let passphrase = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("Passphrase")
        });
        let confirmation = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("Repeat passphrase")
        });
        let device_name = cx.new(|cx| InputState::new(window, cx).placeholder("Device name"));
        let on_enter = |action: fn(&mut Shell, &mut Window, &mut Context<Shell>)| {
            move |shell: &mut Shell,
                  _: &Entity<InputState>,
                  event: &InputEvent,
                  window: &mut Window,
                  cx: &mut Context<Shell>| {
                if let InputEvent::PressEnter { .. } = event {
                    action(shell, window, cx);
                }
            }
        };
        let subscriptions = vec![
            cx.subscribe_in(&passphrase, window, on_enter(Self::submit_passphrase)),
            cx.subscribe_in(&confirmation, window, on_enter(Self::submit_passphrase)),
            cx.subscribe_in(&device_name, window, on_enter(Self::link)),
        ];
        self.inputs = Some(Inputs {
            window: id,
            passphrase,
            confirmation,
            device_name,
            _subscriptions: subscriptions,
        });
        self.focus_pending = true;
    }

    /// The window closed; the next one makes new inputs.
    pub fn detach(&mut self, cx: &mut Context<Self>) {
        self.inputs = None;
        if let Some(chat) = &self.chat {
            chat.update(cx, |chat, cx| chat.detach(cx));
        }
    }

    fn inputs(&self) -> &Inputs {
        self.inputs.as_ref().expect("attached to a window")
    }

    /// Focuses the composer once a window shows the chats.
    pub fn focus_composer(&mut self, cx: &mut Context<Self>) {
        if let Some(chat) = &self.chat {
            chat.update(cx, |chat, cx| chat.focus_composer(cx));
        }
    }

    /// Applies core events as they arrive, shown or not.
    pub fn listen(&mut self, mut events: Events, cx: &mut Context<Self>) {
        self._events = Some(cx.spawn(async move |this, cx| {
            while let Some(event) = events.recv().await {
                if this
                    .update(cx, |shell, cx| shell.on_event(&event, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    /// Shows the window on clicked notifications, on their chat.
    pub fn listen_to_notifications(
        &mut self,
        mut clicks: UnboundedReceiver<NoticeKey>,
        cx: &mut Context<Self>,
    ) {
        self._clicks = Some(cx.spawn(async move |this, cx| {
            while let Some(key) = clicks.next().await {
                if this
                    .update(cx, |shell, cx| shell.notification_clicked(key, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    pub fn notification_clicked(&mut self, key: NoticeKey, cx: &mut Context<Self>) {
        if let (NoticeKey::Chat(id), Some(chat)) = (key, &self.chat) {
            chat.update(cx, |chat, cx| chat.open_later(id, cx));
        }
        // Deferred: showing may open a window, which renders this.
        cx.defer(|cx| main_window::show(None, cx));
    }

    pub fn on_event(&mut self, event: &Event, cx: &mut Context<Self>) {
        self.onboarding.apply(event);
        // The core started over: the chats are gone.
        let gone =
            !self.onboarding.has_account() && !matches!(self.onboarding.screen(), Screen::Stopped);
        if gone && let Some(chat) = self.chat.take() {
            chat.update(cx, |chat, cx| chat.close_notices(cx));
        }
        match self.onboarding.screen() {
            Screen::Passphrase { busy: false, .. } | Screen::Unlinked { busy: false, .. } => {
                self.focus_pending = true;
            }
            Screen::Linking { url } if self.qr.as_ref().is_none_or(|(cached, _)| cached != url) => {
                self.qr = Some((url.clone(), QrMatrix::encode(url)));
            }
            Screen::Ready => {
                let (core, notifier) = (self.core.clone(), self.notifier.clone());
                let chat = self
                    .chat
                    .get_or_insert_with(|| cx.new(|cx| ChatView::new(core, notifier, cx)))
                    .clone();
                chat.update(cx, |chat, cx| chat.on_event(event, cx));
            }
            Screen::Revoked { .. } => {
                if let Some(chat) = &self.chat {
                    chat.update(cx, |chat, cx| chat.on_event(event, cx));
                }
            }
            _ => {}
        }
        cx.notify();
    }

    /// Focuses the current screen's input.
    fn focus_screen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.onboarding.screen() {
            Screen::Passphrase { busy: false, .. } => {
                let input = self.inputs().passphrase.clone();
                input.update(cx, |input, cx| input.focus(window, cx));
            }
            Screen::Unlinked {
                device_name,
                busy: false,
                ..
            } => {
                let name = device_name.clone();
                let input = self.inputs().device_name.clone();
                input.update(cx, |input, cx| {
                    input.set_value(name, window, cx);
                    input.focus(window, cx);
                });
            }
            _ => {}
        }
    }

    /// The chats, once the core is ready.
    pub fn chat(&self) -> Option<&Entity<ChatView>> {
        self.chat.as_ref()
    }

    pub fn screen(&self) -> &Screen {
        self.onboarding.screen()
    }

    fn submit_passphrase(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let inputs = self.inputs();
        let (passphrase, confirmation) = (inputs.passphrase.clone(), inputs.confirmation.clone());
        if let Some(command) = self
            .onboarding
            .submit_passphrase(&passphrase.read(cx).value(), &confirmation.read(cx).value())
        {
            for input in [&passphrase, &confirmation] {
                input.update(cx, |input, cx| input.set_value("", window, cx));
            }
            self.core.send(command);
        }
        cx.notify();
    }

    fn link(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let name = self.inputs().device_name.read(cx).value();
        if let Some(command) = self.onboarding.link(&name) {
            self.core.send(command);
        }
        cx.notify();
    }

    fn cancel_link(&mut self, _: &mut Window, _: &mut Context<Self>) {
        if let Some(command) = self.onboarding.cancel_link() {
            self.core.send(command);
        }
    }

    fn retry(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(command) = self.onboarding.retry() {
            self.core.send(command);
        }
        cx.notify();
    }

    fn start_over(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.onboarding.start_over();
        cx.notify();
    }

    fn cancel_start_over(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.onboarding.cancel_start_over();
        cx.notify();
    }

    fn confirm_start_over(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(command) = self.onboarding.confirm_start_over() {
            self.core.send(command);
        }
        cx.notify();
    }

    fn wipe(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(command) = self.onboarding.wipe() {
            self.core.send(command);
        }
        cx.notify();
    }

    fn reconnect(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(command) = self.onboarding.reconnect() {
            self.core.send(command);
        }
        cx.notify();
    }

    fn render_screen(&self, cx: &mut Context<Self>) -> AnyElement {
        match self.onboarding.screen().clone() {
            Screen::Starting => status("Starting…", cx),
            Screen::Stopped => card(
                "Copiale has stopped",
                "Restart Copiale to continue.".into(),
                cx,
            )
            .into_any_element(),
            // Shown by the chat view instead.
            Screen::Ready => status("Loading…", cx),
            Screen::Revoked { busy } => card(
                "This device was unlinked",
                "Your phone removed Copiale from its linked devices, so it can't send or \
                 receive messages. Wipe its local data to link it again; your messages stay \
                 on your phone."
                    .into(),
                cx,
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("wipe")
                            .danger()
                            .label(if busy {
                                "Wiping…"
                            } else {
                                "Wipe and link again"
                            })
                            .disabled(busy)
                            .on_click(cx.listener(|shell, _, window, cx| shell.wipe(window, cx))),
                    )
                    .child(
                        Button::new("reconnect")
                            .ghost()
                            .label("Try again")
                            .disabled(busy)
                            .on_click(
                                cx.listener(|shell, _, window, cx| shell.reconnect(window, cx)),
                            ),
                    ),
            )
            .into_any_element(),
            Screen::Passphrase { new, error, busy } => self.render_passphrase(new, error, busy, cx),
            Screen::StorageFailed {
                failure,
                confirm_start_over,
                busy,
            } => {
                let (title, body) = describe(&failure);
                let buttons =
                    if confirm_start_over {
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                Button::new("confirm-start-over")
                                    .danger()
                                    .label("Delete local data and start over")
                                    .disabled(busy)
                                    .on_click(cx.listener(|shell, _, window, cx| {
                                        shell.confirm_start_over(window, cx)
                                    })),
                            )
                            .child(
                                Button::new("cancel-start-over")
                                    .ghost()
                                    .label("Cancel")
                                    .disabled(busy)
                                    .on_click(cx.listener(|shell, _, window, cx| {
                                        shell.cancel_start_over(window, cx)
                                    })),
                            )
                    } else {
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                Button::new("retry")
                                    .primary()
                                    .label("Try again")
                                    .disabled(busy)
                                    .on_click(
                                        cx.listener(|shell, _, window, cx| shell.retry(window, cx)),
                                    ),
                            )
                            .child(
                                Button::new("start-over")
                                    .ghost()
                                    .label("Start over…")
                                    .disabled(busy)
                                    .on_click(cx.listener(|shell, _, window, cx| {
                                        shell.start_over(window, cx)
                                    })),
                            )
                    };
                card(title, body, cx).child(buttons).into_any_element()
            }
            Screen::Unlinked { error, busy, .. } => card(
                "Link this device",
                "Copiale works as a linked device for your Signal account. Chats start \
                 empty: messages from before linking stay on your phone."
                    .into(),
                cx,
            )
            .child(Input::new(&self.inputs().device_name).disabled(busy))
            .child(error_text(error, cx))
            .child(
                Button::new("link")
                    .primary()
                    .label(if busy { "Requesting link…" } else { "Link" })
                    .disabled(busy)
                    .on_click(cx.listener(|shell, _, window, cx| shell.link(window, cx))),
            )
            .into_any_element(),
            Screen::Linking { .. } => {
                let qr = match self.qr.as_ref().and_then(|(_, matrix)| matrix.clone()) {
                    Some(matrix) => qr_code(matrix).into_any_element(),
                    None => error_text(Some("The link is too long for a QR code".into()), cx)
                        .into_any_element(),
                };
                card(
                    "Scan with your phone",
                    "In Signal on your phone, open Settings → Linked devices → Link new \
                     device, then scan this code."
                        .into(),
                    cx,
                )
                .child(div().flex().justify_center().py_2().child(qr))
                .child(
                    Button::new("cancel-link").ghost().label("Cancel").on_click(
                        cx.listener(|shell, _, window, cx| shell.cancel_link(window, cx)),
                    ),
                )
                .into_any_element()
            }
        }
    }

    fn render_passphrase(
        &self,
        new: bool,
        error: Option<String>,
        busy: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (title, body, action, busy_label) = if new {
            (
                "Set a passphrase",
                "No system keychain was found, so Copiale encrypts its local data with a \
                 passphrase. You'll need it every time Copiale starts, and it can't be \
                 recovered.",
                "Continue",
                "Setting up…",
            )
        } else {
            (
                "Unlock Copiale",
                "Enter your passphrase to decrypt your local data.",
                "Unlock",
                "Unlocking…",
            )
        };
        let inputs = self.inputs();
        let mut form =
            card(title, body.into(), cx).child(Input::new(&inputs.passphrase).disabled(busy));
        if new {
            form = form.child(Input::new(&inputs.confirmation).disabled(busy));
        }
        form.child(error_text(error, cx))
            .child(
                Button::new("submit-passphrase")
                    .primary()
                    .label(if busy { busy_label } else { action })
                    .disabled(busy)
                    .on_click(
                        cx.listener(|shell, _, window, cx| shell.submit_passphrase(window, cx)),
                    ),
            )
            .into_any_element()
    }
}

impl Shell {
    /// See-through where the settings' opacity is below 1, blurred if asked.
    fn sync_background(&mut self, window: &Window, cx: &App) {
        let settings = Settings::get(cx).window;
        let wanted = match (settings.opacity < 1., settings.blur) {
            (false, _) => WindowBackgroundAppearance::Opaque,
            (true, false) => WindowBackgroundAppearance::Transparent,
            (true, true) => WindowBackgroundAppearance::Blurred,
        };
        let id = window.window_handle().window_id();
        if self.background != Some((id, wanted)) {
            window.set_background_appearance(wanted);
            self.background = Some((id, wanted));
        }
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.attach(window, cx);
        self.sync_background(window, cx);
        let content = if let (Screen::Ready, Some(chat)) = (self.onboarding.screen(), &self.chat) {
            let chat = chat.clone();
            chat.update(cx, |chat, cx| chat.attach(window, cx));
            div().size_full().child(chat)
        } else {
            if std::mem::take(&mut self.focus_pending) {
                self.focus_screen(window, cx);
            }
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p_4()
                .bg(cx.palette().background)
                .text_color(cx.palette().text)
                .child(self.render_screen(cx))
        };
        v_flex()
            .size_full()
            .children(titlebar::render(window, cx))
            .child(div().flex_1().min_h_0().child(content))
    }
}

/// A centered column with a title and explanation; callers add the rest.
fn card(title: &'static str, body: String, cx: &App) -> gpui_kit::Div {
    v_flex()
        .w_full()
        .max_w(px(420.))
        .gap_3()
        .child(div().text_xl().child(title))
        .child(
            div()
                .text_sm()
                .text_color(cx.palette().text_muted)
                .child(SharedString::from(body)),
        )
}

fn status(text: &'static str, cx: &App) -> AnyElement {
    div()
        .text_color(cx.palette().text_muted)
        .child(text)
        .into_any_element()
}

fn error_text(error: Option<String>, cx: &App) -> impl IntoElement {
    div()
        .text_sm()
        .text_color(cx.palette().danger)
        .children(error.map(SharedString::from))
}
