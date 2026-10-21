//! The new chat picker, the palette, safety numbers, and the link and
//! unlink confirmations.

use std::ops::Range;

use copiale_core::config::Setting;
use copiale_core::{Command, ServiceId, Verification};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Action, AnyElement, App, Context, Entity, HighlightStyle, Hsla, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement as _, SharedString, Styled as _, StyledText,
    TestSupportExt as _, Window, div, px, relative,
};

use crate::Quit;
use crate::chat::model::Overlay;
use crate::chat::palette::PaletteAction;
use crate::chat::text::{isolate, link_host};
use crate::settings::{self, Settings};
use crate::theme::ActivePalette as _;

use super::{
    CONFIRM_UNLINK, Cancel, ChatView, ConfirmSafetyNumber, LINK, NewChat, OpenLink, OpenSettings,
    PICKING, PickNext, PickPrevious, SAFETY_NUMBER, key_hint,
};

impl ChatView {
    /// Query changes and Enter in the new chat picker or the palette.
    pub(super) fn on_picker_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                self.chat.set_picker_query(&input.read(cx).value());
                cx.notify();
            }
            InputEvent::PressEnter { .. } => match self.chat.overlay() {
                Some(Overlay::NewChat) => self.choose_new_chat(None, window, cx),
                Some(Overlay::Palette) => self.choose_palette(None, window, cx),
                _ => {}
            },
            _ => {}
        }
    }

    fn pick_previous(&mut self, _: &PickPrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.chat.picker_step(-1);
        cx.notify();
    }

    fn pick_next(&mut self, _: &PickNext, _: &mut Window, cx: &mut Context<Self>) {
        self.chat.picker_step(1);
        cx.notify();
    }

    /// Opens the chat with candidate `ix`, or the highlighted one; a new
    /// one opens once the core has created it.
    fn choose_new_chat(&mut self, ix: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        if !self.chat.new_chat_open() {
            return;
        }
        let command = self.chat.choose_new_chat(ix);
        self.send(command);
        match self.chat.take_requested_open() {
            Some(id) => self.open(id, window, cx),
            None if !self.chat.new_chat_open() => window.focus(&self.focus, cx),
            None => {}
        }
        cx.notify();
    }

    /// Runs palette entry `ix`, or the highlighted one.
    fn choose_palette(&mut self, ix: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(action) = self.chat.choose_palette(ix) else {
            return;
        };
        match action {
            PaletteAction::Open(id) => self.open(id, window, cx),
            PaletteAction::SafetyNumber(peer) => self.show_safety_number(peer, window, cx),
            PaletteAction::Accept(id) | PaletteAction::Unblock(id) => {
                self.answer_request(Command::Accept(id), window, cx)
            }
            PaletteAction::Block(id) => self.answer_request(Command::Block(id), window, cx),
            PaletteAction::DeleteRequest(id) => {
                self.answer_request(Command::DeleteRequest(id), window, cx)
            }
            PaletteAction::NewChat => self.start_new_chat(&NewChat, window, cx),
            PaletteAction::Settings => self.open_settings(&OpenSettings, window, cx),
            PaletteAction::Theme(mode) => {
                settings::change(Setting::Theme(mode), cx);
                self.restore_focus(window, cx);
            }
            PaletteAction::Quit => window.dispatch_action(Quit.boxed_clone(), cx),
        }
        cx.notify();
    }

    pub(super) fn render_overlay(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (id, card) = match self.chat.overlay()? {
            Overlay::NewChat => ("new-chat", self.render_new_chat(cx)),
            Overlay::Palette => ("palette", self.render_palette(window, cx)),
            Overlay::Settings => ("settings", self.render_settings(window, cx)),
            Overlay::SafetyNumber(_) => ("safety-number", self.render_safety_number(cx)),
            Overlay::Link => ("link", self.render_link(cx)),
        };
        let width = match self.chat.overlay() {
            Some(Overlay::Settings) => px(560.),
            _ => px(420.),
        };
        let palette = cx.palette();
        let card = card
            .id(id)
            // Clicks inside don't reach the backdrop.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .w(width)
            .max_w(relative(0.9))
            // Opaque, unlike the window background.
            .bg(cx.theme().popover)
            .border_1()
            .border_color(palette.border)
            .rounded(px(6.))
            .shadow_lg()
            .test_support();
        Some(
            div()
                .id(SharedString::from(format!("{id}-backdrop")))
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .items_start()
                .pt(px(64.))
                .bg(palette.background.opacity(0.6))
                // The window underneath gets no hover, clicks or scrolling.
                .occlude()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|view, _, window, cx| view.close_overlay(window, cx)),
                )
                .child(card)
                .test_support()
                .into_any_element(),
        )
    }

    /// A query box over the rows, or a muted line if there are none.
    fn picker(
        &self,
        input: &Entity<InputState>,
        rows: Vec<AnyElement>,
        empty: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let palette = cx.palette();
        let empty = rows.is_empty().then(|| {
            div()
                .px_3()
                .py_2()
                .text_sm()
                .text_color(palette.text_muted)
                .child(empty)
        });
        v_flex()
            .key_context(PICKING)
            .on_action(cx.listener(Self::pick_previous))
            .on_action(cx.listener(Self::pick_next))
            .on_action(cx.listener(|view, _: &Escape, window, cx| view.close_overlay(window, cx)))
            .child(
                div()
                    .p_2()
                    .border_b_1()
                    .border_color(palette.border)
                    .child(Input::new(input).small()),
            )
            .child(v_flex().p_1().children(rows).children(empty))
    }

    fn render_new_chat(&self, cx: &mut Context<Self>) -> gpui_kit::Div {
        let palette = cx.palette();
        let muted = palette.text_muted;
        let mono = cx.theme().mono_font_family.clone();
        let selected = self.chat.picker_selected();
        let rows: Vec<_> = self
            .chat
            .new_chat_candidates()
            .into_iter()
            .enumerate()
            .map(|(ix, candidate)| {
                let details = candidate
                    .detail
                    .map(|phone| SharedString::from(phone.to_owned()))
                    .into_iter()
                    .chain(candidate.existing.then(|| "chat".into()));
                picker_row(
                    ("contact", ix as u64),
                    selected == Some(ix),
                    None,
                    StyledText::new(candidate.label)
                        .with_highlights(match_highlights(candidate.highlights, cx)),
                    details,
                    muted,
                    mono.clone(),
                )
                .on_click(cx.listener(move |view, _, window, cx| {
                    view.choose_new_chat(Some(ix), window, cx)
                }))
                .into_any_element()
            })
            .collect();
        let empty = if self.chat.has_contacts() {
            "No matches"
        } else {
            "No contacts yet; they sync from your phone"
        };
        self.picker(&self.inputs().new_chat, rows, empty, cx)
    }

    fn render_palette(&self, window: &Window, cx: &mut Context<Self>) -> gpui_kit::Div {
        let palette = cx.palette();
        let muted = palette.text_muted;
        let mono = cx.theme().mono_font_family.clone();
        let selected = self.chat.picker_selected();
        let theme = Settings::get(cx).appearance.theme;
        let rows: Vec<_> = self
            .chat
            .palette_entries()
            .into_iter()
            .enumerate()
            .map(|(ix, entry)| {
                let (glyph, hint) = match entry.action {
                    PaletteAction::Open(_) => ("#", None),
                    PaletteAction::SafetyNumber(_)
                    | PaletteAction::Accept(_)
                    | PaletteAction::Block(_)
                    | PaletteAction::Unblock(_)
                    | PaletteAction::DeleteRequest(_) => (">", None),
                    PaletteAction::NewChat => (">", key_hint(&NewChat, window)),
                    PaletteAction::Settings => (">", key_hint(&OpenSettings, window)),
                    PaletteAction::Theme(mode) => (">", (mode == theme).then(|| "current".into())),
                    PaletteAction::Quit => (">", key_hint(&Quit, window)),
                };
                picker_row(
                    ("palette-entry", ix as u64),
                    selected == Some(ix),
                    Some(glyph),
                    StyledText::new(entry.label)
                        .with_highlights(match_highlights(entry.highlights, cx)),
                    hint,
                    muted,
                    mono.clone(),
                )
                .on_click(
                    cx.listener(move |view, _, window, cx| {
                        view.choose_palette(Some(ix), window, cx)
                    }),
                )
                .into_any_element()
            })
            .collect();
        self.picker(&self.inputs().palette, rows, "No matches", cx)
    }

    /// Shows `peer`'s safety number; `Enter` confirms a change, `Esc`
    /// closes.
    pub(super) fn show_safety_number(
        &mut self,
        peer: ServiceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let command = self.chat.show_safety_number(peer);
        self.core.send(command);
        window.focus(&self.identity_focus, cx);
        cx.notify();
    }

    fn confirm_safety_number(
        &mut self,
        _: &ConfirmSafetyNumber,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let command = self.chat.confirm_identity();
        if command.is_some() {
            self.send(command);
            self.restore_focus(window, cx);
        }
        cx.notify();
    }

    /// The digits in rows of four blocks, and whether they're verified.
    fn render_safety_number(&self, cx: &mut Context<Self>) -> gpui_kit::Div {
        let muted = cx.palette().text_muted;
        let mono = cx.theme().mono_font_family.clone();
        let (name, identity) = self.chat.shown_identity().unwrap_or_default();
        let name = isolate(&name);
        let verification = identity.map(|identity| identity.verification);
        let digits = match identity.map(|identity| identity.safety_number.as_ref()) {
            Some(Some(number)) => v_flex()
                .id("safety-number-digits")
                .gap_1()
                .font_family(mono.clone())
                .text_lg()
                .children(number.blocks().chunks(4).map(|row| {
                    h_flex()
                        .gap_3()
                        .children(row.iter().map(|block| div().child(block.to_string())))
                }))
                .test_support()
                .into_any_element(),
            Some(None) => div()
                .text_sm()
                .text_color(muted)
                .child(format!(
                    "No safety number yet: it appears once you and {name} have exchanged \
                     messages."
                ))
                .into_any_element(),
            None => div().into_any_element(),
        };
        let status = match verification {
            Some(Verification::Verified) => "Verified on your phone.".to_string(),
            Some(Verification::Unverified) => format!(
                "Not verified. Compare it with {name}'s in person or on a call, then mark it \
                 verified on your phone."
            ),
            Some(Verification::Changed) => format!(
                "{name}'s safety number changed since you verified it. Compare the new one with \
                 theirs; confirming it lets you send to them again."
            ),
            None => String::new(),
        };
        let changed = verification == Some(Verification::Changed);
        let hint = if changed {
            "enter confirm · esc close"
        } else {
            "esc close"
        };
        v_flex()
            .key_context(SAFETY_NUMBER)
            .track_focus(&self.identity_focus)
            .on_action(cx.listener(|view, _: &Cancel, window, cx| view.close_overlay(window, cx)))
            .on_action(cx.listener(Self::confirm_safety_number))
            .p_4()
            .gap_3()
            .child(div().text_lg().child(format!("Safety number with {name}")))
            .child(digits)
            .child(div().text_sm().text_color(muted).child(status))
            .child(
                h_flex()
                    .gap_2()
                    .when(changed, |row| {
                        row.child(
                            Button::new("confirm-safety-number")
                                .primary()
                                .label("Confirm")
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.confirm_safety_number(&ConfirmSafetyNumber, window, cx)
                                })),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        div()
                            .font_family(mono)
                            .text_xs()
                            .text_color(muted)
                            .child(hint),
                    ),
            )
    }

    /// Asks before opening the clicked `href`.
    pub(super) fn ask_open_link(
        &mut self,
        href: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.chat.ask_open_link(href);
        // After the click, which focuses the text.
        cx.defer_in(window, |view, window, cx| {
            if view.chat.shown_link().is_some() {
                window.focus(&view.link_focus, cx);
            }
        });
        cx.notify();
    }

    fn open_link(&mut self, _: &OpenLink, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(url) = self.chat.confirm_link() {
            cx.open_url(url.as_str());
        }
        self.restore_focus(window, cx);
        cx.notify();
    }

    /// The link's host in ASCII, so look-alike letters show, then the
    /// whole URL.
    fn render_link(&self, cx: &mut Context<Self>) -> gpui_kit::Div {
        let muted = cx.palette().text_muted;
        let mono = cx.theme().mono_font_family.clone();
        let (host, url) = self
            .chat
            .shown_link()
            .map(|url| (link_host(url), url.to_string()))
            .unwrap_or_default();
        v_flex()
            .key_context(LINK)
            .track_focus(&self.link_focus)
            .on_action(cx.listener(|view, _: &Cancel, window, cx| view.close_overlay(window, cx)))
            .on_action(cx.listener(Self::open_link))
            .p_4()
            .gap_3()
            .child(div().text_lg().child("Open link?"))
            .child(div().font_family(mono.clone()).text_lg().child(host))
            .child(
                div()
                    .font_family(mono.clone())
                    .text_xs()
                    .text_color(muted)
                    .line_clamp(4)
                    .child(url),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(Button::new("cancel-link").ghost().label("Cancel").on_click(
                        cx.listener(|view, _, window, cx| view.close_overlay(window, cx)),
                    ))
                    .child(Button::new("open-link").primary().label("Open").on_click(
                        cx.listener(|view, _, window, cx| view.open_link(&OpenLink, window, cx)),
                    ))
                    .child(div().flex_1())
                    .child(
                        div()
                            .font_family(mono)
                            .text_xs()
                            .text_color(muted)
                            .child("enter open · esc cancel"),
                    ),
            )
    }

    /// Asks before unlinking; `Tab` reaches the buttons, `Esc` goes back.
    pub(super) fn render_unlink_confirm(&self, cx: &mut Context<Self>) -> gpui_kit::Div {
        let muted = cx.palette().text_muted;
        let mono = cx.theme().mono_font_family.clone();
        v_flex()
            .key_context(CONFIRM_UNLINK)
            .track_focus(&self.confirm_focus)
            .on_action(cx.listener(|view, _: &Cancel, window, cx| view.cancel_unlink(window, cx)))
            .p_4()
            .gap_3()
            .child(div().text_lg().child("Unlink this device?"))
            .child(div().text_sm().text_color(muted).child(
                "This removes the device from your account, deletes all messages and keys on \
                 it and returns to the link screen. Settings stay. Your messages stay on your \
                 phone.",
            ))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("cancel-unlink")
                            .ghost()
                            .label("Cancel")
                            .on_click(
                                cx.listener(|view, _, window, cx| view.cancel_unlink(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("confirm-unlink")
                            .danger()
                            .label("Unlink and wipe")
                            .on_click(cx.listener(|view, _, window, cx| view.unlink(window, cx))),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .font_family(mono)
                            .text_xs()
                            .text_color(muted)
                            .child("tab choose · esc back"),
                    ),
            )
    }

    fn cancel_unlink(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_panel.cancel_unlink();
        Self::focus_input(&self.inputs().settings, window, cx);
        cx.notify();
    }

    /// Wipes local data; the shell shows the link screen once the core
    /// starts over.
    fn unlink(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.core.send(Command::Unlink);
        self.close_overlay(window, cx);
    }
}

/// A picker entry: an optional glyph, the label, then muted details.
pub(super) fn picker_row(
    id: impl Into<gpui_kit::ElementId>,
    selected: bool,
    glyph: Option<&'static str>,
    label: StyledText,
    details: impl IntoIterator<Item = SharedString>,
    muted: Hsla,
    mono: SharedString,
) -> ListItem {
    let detail = |text: SharedString| {
        div()
            .flex_none()
            .font_family(mono.clone())
            .text_xs()
            .text_color(muted)
            .child(text)
    };
    ListItem::new(id).selected(selected).child(
        h_flex()
            .w_full()
            .gap_2()
            .children(glyph.map(|glyph| {
                div()
                    .flex_none()
                    .font_family(mono.clone())
                    .text_color(muted)
                    .child(glyph)
            }))
            .child(div().flex_1().min_w_0().truncate().child(label))
            .children(details.into_iter().map(detail)),
    )
}

/// Matched characters in the accent color.
pub(super) fn match_highlights(
    ranges: Vec<Range<usize>>,
    cx: &App,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let highlight = HighlightStyle {
        color: Some(cx.palette().accent),
        ..Default::default()
    };
    ranges.into_iter().map(|range| (range, highlight)).collect()
}
