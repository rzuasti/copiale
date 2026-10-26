//! The new chat and new group pickers, the palette, safety numbers, and
//! the link and unlink confirmations.

use std::ops::Range;

use copiale_core::text::MAX_GROUP_TITLE;
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
use crate::chat::new_group::GroupStep;
use crate::chat::palette::PaletteAction;
use crate::chat::text::{isolate, link_host};
use crate::theme::ActivePalette as _;

use super::{
    CONFIRM_UNLINK, Cancel, ChatView, ConfirmSafetyNumber, GROUP_MEMBERS, LINK, NewChat,
    NewGroupChat, OpenLink, OpenSettings, PICKING, PickNext, PickPrevious, SAFETY_NUMBER,
    ToggleMember, key_hint,
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
                Some(Overlay::NewGroup) => self.name_group(window, cx),
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
            None if self.chat.new_group().is_some() => self.ready_group_step(window, cx),
            None if !self.chat.new_chat_open() => window.focus(&self.focus, cx),
            None => {}
        }
        cx.notify();
    }

    /// Focuses the new group picker's step: the query, empty, or the name.
    pub(super) fn ready_group_step(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(group) = self.chat.new_group() else {
            return;
        };
        match group.step() {
            GroupStep::Members => Self::ready_query(&self.inputs().group_members, window, cx),
            GroupStep::Name => {
                let name = group.name().to_owned();
                self.inputs().group_name.update(cx, |input, cx| {
                    input.set_value(name, window, cx);
                    input.focus(window, cx);
                });
            }
        }
    }

    fn toggle_member(&mut self, _: &ToggleMember, window: &mut Window, cx: &mut Context<Self>) {
        self.pick_member(None, window, cx);
    }

    /// Adds or removes candidate `ix`, or the highlighted one; a query
    /// typed to find them clears.
    fn pick_member(&mut self, ix: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        if self.chat.toggle_group_member(ix) {
            let input = self.inputs().group_members.clone();
            if !input.read(cx).value().is_empty() {
                input.update(cx, |input, cx| input.set_value("", window, cx));
                self.chat.set_picker_query("");
            }
            Self::focus_input(&input, window, cx);
        }
        cx.notify();
    }

    fn unpick_member(&mut self, id: ServiceId, window: &mut Window, cx: &mut Context<Self>) {
        self.chat.remove_group_member(id);
        Self::focus_input(&self.inputs().group_members, window, cx);
        cx.notify();
    }

    /// On to the name, once members are picked.
    fn name_group(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.chat.new_group_next() {
            self.ready_group_step(window, cx);
        }
        cx.notify();
    }

    /// Name changes, and Enter creating the group.
    pub(super) fn on_group_name_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                self.chat.set_group_name(&input.read(cx).value());
                cx.notify();
            }
            InputEvent::PressEnter { .. } => {
                let command = self.chat.create_group();
                self.send(command);
                cx.notify();
            }
            _ => {}
        }
    }

    /// Back to the members, from the name.
    fn group_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.chat.new_group_back() {
            self.ready_group_step(window, cx);
        } else if !self.chat.new_group().is_some_and(|group| group.creating()) {
            self.close_overlay(window, cx);
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
            PaletteAction::NewGroup => self.start_new_group(&NewGroupChat, window, cx),
            PaletteAction::Settings => self.open_settings(&OpenSettings, window, cx),
            PaletteAction::Theme => self.pick_theme(window, cx),
            PaletteAction::Quit => window.dispatch_action(Quit.boxed_clone(), cx),
        }
        cx.notify();
    }

    pub(super) fn render_overlay(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.viewing() {
            return Some(self.render_viewer(window, cx));
        }
        let (id, card) = match self.chat.overlay()? {
            Overlay::NewChat => ("new-chat", self.render_new_chat(window, cx)),
            Overlay::NewGroup => ("new-group", self.render_new_group(cx)),
            Overlay::Palette => ("palette", self.render_palette(window, cx)),
            Overlay::Settings => ("settings", self.render_settings(window, cx)),
            Overlay::SafetyNumber(_) => ("safety-number", self.render_safety_number(cx)),
            Overlay::Link => ("link", self.render_link(cx)),
            Overlay::Viewer => return None,
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
        let empty = rows
            .is_empty()
            .then(|| muted_line(empty, palette.text_muted));
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

    fn render_new_chat(&self, window: &Window, cx: &mut Context<Self>) -> gpui_kit::Div {
        let palette = cx.palette();
        let muted = palette.text_muted;
        let mono = cx.theme().mono_font_family.clone();
        let selected = self.chat.picker_selected();
        let new_group = picker_row(
            "new-group-entry",
            selected == Some(0),
            Some("+"),
            StyledText::new("New group…"),
            key_hint(&NewGroupChat, window),
            muted,
            mono.clone(),
        )
        .on_click(cx.listener(|view, _, window, cx| view.choose_new_chat(Some(0), window, cx)))
        .into_any_element();
        let candidates = self.chat.new_chat_candidates();
        let empty = candidates.is_empty().then(|| {
            let text = if self.chat.has_contacts() {
                "No matches"
            } else {
                "No contacts yet; they sync from your phone"
            };
            muted_line(text, muted).into_any_element()
        });
        let rows: Vec<_> = std::iter::once(new_group)
            .chain(empty)
            .chain(candidates.into_iter().enumerate().map(|(ix, candidate)| {
                // After the new group entry.
                let entry = ix + 1;
                let details = candidate
                    .detail
                    .map(|phone| SharedString::from(phone.to_owned()))
                    .into_iter()
                    .chain(candidate.existing.then(|| "chat".into()));
                picker_row(
                    ("contact", ix as u64),
                    selected == Some(entry),
                    None,
                    StyledText::new(candidate.label)
                        .with_highlights(match_highlights(candidate.highlights, cx)),
                    details,
                    muted,
                    mono.clone(),
                )
                .on_click(cx.listener(move |view, _, window, cx| {
                    view.choose_new_chat(Some(entry), window, cx)
                }))
                .into_any_element()
            }))
            .collect();
        self.picker(&self.inputs().new_chat, rows, "", cx)
    }

    fn render_new_group(&self, cx: &mut Context<Self>) -> gpui_kit::Div {
        match self.chat.new_group().map(|group| group.step()) {
            Some(GroupStep::Name) => self.render_group_name(cx),
            _ => self.render_group_members(cx),
        }
    }

    /// Picked members as chips over the query, then contacts to pick.
    fn render_group_members(&self, cx: &mut Context<Self>) -> gpui_kit::Div {
        let palette = cx.palette();
        let muted = palette.text_muted;
        let mono = cx.theme().mono_font_family.clone();
        let selected = self.chat.picker_selected();
        let members = self
            .chat
            .new_group()
            .map(|group| group.members().to_vec())
            .unwrap_or_default();
        let chips = (!members.is_empty()).then(|| {
            h_flex()
                .id("group-chips")
                .flex_wrap()
                .gap_1()
                .px_2()
                .pt_2()
                .children(members.iter().map(|&id| {
                    let label = isolate(&self.chat.contact_label(id));
                    Button::new(("group-chip", id.uuid().as_u128() as u64))
                        .xsmall()
                        .outline()
                        .label(format!("{label} ×"))
                        .on_click(cx.listener(move |view, _, window, cx| {
                            view.unpick_member(id, window, cx)
                        }))
                }))
                .test_support()
        });
        let rows: Vec<_> = self
            .chat
            .new_group_candidates()
            .into_iter()
            .enumerate()
            .map(|(ix, candidate)| {
                let picked = members.contains(&candidate.contact.id);
                let details = candidate
                    .detail
                    .map(|phone| SharedString::from(phone.to_owned()))
                    .into_iter()
                    .chain(candidate.contact.invite_only.then(|| "invite".into()));
                picker_row(
                    ("member", ix as u64),
                    selected == Some(ix),
                    Some(if picked { "[x]" } else { "[ ]" }),
                    StyledText::new(candidate.label)
                        .with_highlights(match_highlights(candidate.highlights, cx)),
                    details,
                    muted,
                    mono.clone(),
                )
                .on_click(
                    cx.listener(move |view, _, window, cx| view.pick_member(Some(ix), window, cx)),
                )
                .into_any_element()
            })
            .collect();
        let empty = if self.chat.has_contacts() {
            "No matches"
        } else {
            "No contacts yet; they sync from your phone"
        };
        let title = match members.len() {
            0 => "New group".to_owned(),
            count => format!("New group · {count} picked"),
        };
        v_flex()
            .key_context(GROUP_MEMBERS)
            .on_action(cx.listener(Self::toggle_member))
            .child(card_header(title, muted, mono.clone()))
            .children(chips)
            .child(self.picker(&self.inputs().group_members, rows, empty, cx))
            .child(hint_line(
                "space pick · enter next · esc close",
                muted,
                mono,
            ))
    }

    /// The name, who joins, and how creating went.
    fn render_group_name(&self, cx: &mut Context<Self>) -> gpui_kit::Div {
        let palette = cx.palette();
        let muted = palette.text_muted;
        let mono = cx.theme().mono_font_family.clone();
        let Some(group) = self.chat.new_group() else {
            return v_flex();
        };
        let label = |id: ServiceId| isolate(&self.chat.contact_label(id));
        let added: Vec<_> = group
            .members()
            .iter()
            .filter(|id| !self.chat.invite_only(**id))
            .map(|id| label(*id))
            .collect();
        let invited: Vec<_> = group
            .members()
            .iter()
            .filter(|id| self.chat.invite_only(**id))
            .map(|id| label(*id))
            .collect();
        let status = if group.creating() {
            Some(muted_line("Creating…", muted))
        } else if let Some(error) = group.error() {
            Some(muted_line(
                format!("Couldn't create it: {error}"),
                palette.danger,
            ))
        } else if !group.name().trim().is_empty() && !group.name_valid() {
            Some(muted_line(
                format!("Names have at most {MAX_GROUP_TITLE} characters"),
                palette.danger,
            ))
        } else {
            None
        };
        v_flex()
            .on_action(cx.listener(|view, _: &Escape, window, cx| view.group_back(window, cx)))
            .child(card_header("New group".into(), muted, mono.clone()))
            .child(
                div()
                    .p_2()
                    .border_b_1()
                    .border_color(palette.border)
                    .child(Input::new(&self.inputs().group_name).small()),
            )
            .child(
                v_flex()
                    .id("group-joining")
                    .px_3()
                    .py_2()
                    .gap_1()
                    .text_sm()
                    .children((!added.is_empty()).then(|| div().child(added.join(", "))))
                    .children((!invited.is_empty()).then(|| {
                        div()
                            .text_color(muted)
                            .child(format!("Invited: {}", invited.join(", ")))
                    }))
                    .children(status)
                    .test_support(),
            )
            .child(hint_line("enter create · esc back", muted, mono))
    }

    fn render_palette(&self, window: &Window, cx: &mut Context<Self>) -> gpui_kit::Div {
        let palette = cx.palette();
        let muted = palette.text_muted;
        let mono = cx.theme().mono_font_family.clone();
        let selected = self.chat.picker_selected();
        let rows: Vec<_> =
            self.chat
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
                        PaletteAction::NewGroup => (">", key_hint(&NewGroupChat, window)),
                        PaletteAction::Settings => (">", key_hint(&OpenSettings, window)),
                        PaletteAction::Theme => (">", None),
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
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.choose_palette(Some(ix), window, cx)
                    }))
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

/// A small muted line, e.g. what a picker shows without entries.
fn muted_line(text: impl Into<SharedString>, color: Hsla) -> gpui_kit::Div {
    div()
        .px_3()
        .py_2()
        .text_sm()
        .text_color(color)
        .child(text.into())
}

/// A card's title, in the corner.
fn card_header(title: String, muted: Hsla, mono: SharedString) -> gpui_kit::Div {
    div()
        .px_3()
        .pt_2()
        .font_family(mono)
        .text_xs()
        .text_color(muted)
        .child(title)
}

/// The keys a card takes, under it.
fn hint_line(text: &'static str, muted: Hsla, mono: SharedString) -> gpui_kit::Div {
    div()
        .px_3()
        .py_1()
        .border_t_1()
        .border_color(muted.opacity(0.2))
        .font_family(mono)
        .text_xs()
        .text_color(muted)
        .child(text)
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
