use std::rc::Rc;

use copiale_core::{Event, Events};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{Disableable as _, v_flex};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Subscription, Task, Window, div, px,
};

use crate::CoreSink;
use crate::chat::ChatView;
use crate::onboarding::{Onboarding, Screen, describe};
use crate::qr::{QrMatrix, qr_code};
use crate::theme::ActivePalette as _;

/// Root view of the main window: onboarding until the core is ready, then
/// the chats.
pub struct Shell {
    core: Rc<dyn CoreSink>,
    onboarding: Onboarding,
    chat: Option<Entity<ChatView>>,
    passphrase: Entity<InputState>,
    confirmation: Entity<InputState>,
    device_name: Entity<InputState>,
    /// QR code for the current provisioning URL.
    qr: Option<(String, Option<QrMatrix>)>,
    _subscriptions: Vec<Subscription>,
    _events: Option<Task<()>>,
}

impl Shell {
    pub fn new(core: Rc<dyn CoreSink>, window: &mut Window, cx: &mut Context<Self>) -> Self {
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
        Self {
            core,
            onboarding: Onboarding::default(),
            chat: None,
            passphrase,
            confirmation,
            device_name,
            qr: None,
            _subscriptions: subscriptions,
            _events: None,
        }
    }

    /// Applies core events as they arrive.
    pub fn listen(&mut self, mut events: Events, window: &mut Window, cx: &mut Context<Self>) {
        self._events = Some(cx.spawn_in(window, async move |this, cx| {
            while let Some(event) = events.recv().await {
                let applied = this.update_in(cx, |shell, window, cx| {
                    shell.on_event(&event, window, cx);
                });
                if applied.is_err() {
                    break;
                }
            }
        }));
    }

    pub fn on_event(&mut self, event: &Event, window: &mut Window, cx: &mut Context<Self>) {
        self.onboarding.apply(event);
        match self.onboarding.screen() {
            Screen::Passphrase { busy: false, .. } => {
                self.passphrase
                    .update(cx, |input, cx| input.focus(window, cx));
            }
            Screen::Unlinked {
                device_name,
                busy: false,
                ..
            } => {
                let name = device_name.clone();
                self.device_name.update(cx, |input, cx| {
                    input.set_value(name, window, cx);
                    input.focus(window, cx);
                });
            }
            Screen::Linking { url } if self.qr.as_ref().is_none_or(|(cached, _)| cached != url) => {
                self.qr = Some((url.clone(), QrMatrix::encode(url)));
            }
            Screen::Ready => {
                let core = self.core.clone();
                let chat = self
                    .chat
                    .get_or_insert_with(|| cx.new(|cx| ChatView::new(core, window, cx)))
                    .clone();
                chat.update(cx, |chat, cx| chat.on_event(event, window, cx));
            }
            _ => {}
        }
        cx.notify();
    }

    /// The chats, once the core is ready.
    pub fn chat(&self) -> Option<&Entity<ChatView>> {
        self.chat.as_ref()
    }

    pub fn screen(&self) -> &Screen {
        self.onboarding.screen()
    }

    fn submit_passphrase(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let passphrase = self.passphrase.read(cx).value();
        let confirmation = self.confirmation.read(cx).value();
        if let Some(command) = self
            .onboarding
            .submit_passphrase(&passphrase, &confirmation)
        {
            for input in [&self.passphrase, &self.confirmation] {
                input.update(cx, |input, cx| input.set_value("", window, cx));
            }
            self.core.send(command);
        }
        cx.notify();
    }

    fn link(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let name = self.device_name.read(cx).value();
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
            .child(Input::new(&self.device_name).disabled(busy))
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
        let mut form =
            card(title, body.into(), cx).child(Input::new(&self.passphrase).disabled(busy));
        if new {
            form = form.child(Input::new(&self.confirmation).disabled(busy));
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

impl Render for Shell {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let (Screen::Ready, Some(chat)) = (self.onboarding.screen(), &self.chat) {
            return div().size_full().child(chat.clone());
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
