//! The settings list and font picker.

use copiale_core::config;
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, Entity, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, StyledText, TestSupportExt as _,
    Window, div, px, rems,
};

use crate::chat::settings_panel::{Activation, FIELDS, Field, FontRole, Section, Value};
use crate::settings::{self, Settings};
use crate::theme::{ActivePalette as _, InstalledFonts};

use super::overlays::{match_highlights, picker_row};
use super::{
    ChatView, PICKING, PickNext, PickPrevious, SETTINGS, SettingDecrease, SettingIncrease,
    SettingNext, SettingPrevious,
};

impl ChatView {
    pub(super) fn on_settings_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                self.settings_panel.set_query(&input.read(cx).value());
                self.reveal_setting();
                cx.notify();
            }
            InputEvent::PressEnter { .. } => {
                if let Some(field) = self.settings_panel.selected() {
                    self.activate_setting(field, window, cx);
                }
            }
            _ => {}
        }
    }

    /// `Enter` or a click on a row's value.
    fn activate_setting(&mut self, field: Field, window: &mut Window, cx: &mut Context<Self>) {
        match field.activate(Settings::get(cx)) {
            Activation::Change(setting) => settings::change(setting, cx),
            Activation::PickFont(role) => {
                self.settings_panel.pick_font(role, Settings::get(cx));
                Self::ready_query(&self.inputs().font, window, cx);
            }
            Activation::ConfirmUnlink => {
                self.settings_panel.confirm_unlink();
                // Neither button: `Enter` alone can't wipe.
                window.focus(&self.confirm_focus, cx);
            }
            Activation::Nothing => {}
        }
        cx.notify();
    }

    fn step_setting(&mut self, step: isize, cx: &mut Context<Self>) {
        self.settings_panel.step(step);
        self.reveal_setting();
        cx.notify();
    }

    fn change_setting(&mut self, field: Field, step: i32, cx: &mut Context<Self>) {
        if let Some(setting) = field.step(Settings::get(cx), step) {
            settings::change(setting, cx);
        }
        cx.notify();
    }

    fn change_selected(&mut self, step: i32, cx: &mut Context<Self>) {
        if let Some(field) = self.settings_panel.selected() {
            self.change_setting(field, step, cx);
        }
    }

    /// Scrolls the highlighted row into view.
    fn reveal_setting(&self) {
        let fields = self.settings_panel.fields();
        if let Some(ix) = self
            .settings_panel
            .selected()
            .and_then(|field| settings_item(&fields, field))
        {
            self.settings_scroll.scroll_to_item(ix);
        }
    }

    pub(super) fn on_font_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(picker) = self.settings_panel.font_picker_mut() else {
            return;
        };
        match event {
            InputEvent::Change => {
                picker.set_query(&input.read(cx).value());
                cx.notify();
            }
            InputEvent::PressEnter { .. } => self.choose_font(None, window, cx),
            _ => {}
        }
    }

    /// Moves the font picker's highlight and previews that font.
    fn step_font(&mut self, step: isize, cx: &mut Context<Self>) {
        let installed = InstalledFonts::get(cx);
        let Some(picker) = self.settings_panel.font_picker_mut() else {
            return;
        };
        let entries = picker.entries(&installed);
        picker.step(step, entries.len());
        if let Some(setting) = picker.setting(&entries, picker.selected()) {
            settings::preview(&setting, cx);
        }
        cx.notify();
    }

    /// Saves font entry `ix`, or the highlighted one, and returns to the
    /// list.
    fn choose_font(&mut self, ix: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let installed = InstalledFonts::get(cx);
        let Some(picker) = self.settings_panel.font_picker_mut() else {
            return;
        };
        let entries = picker.entries(&installed);
        let setting = picker.setting(&entries, ix.unwrap_or(picker.selected()));
        self.settings_panel.close_font_picker(true);
        if let Some(setting) = setting {
            settings::change(setting, cx);
        }
        Self::focus_input(&self.inputs().settings, window, cx);
        cx.notify();
    }

    /// Closes the font picker, undoing its preview unless `keep`.
    pub(super) fn close_font_picker(&mut self, keep: bool, cx: &mut Context<Self>) {
        if let Some(setting) = self.settings_panel.close_font_picker(keep) {
            settings::preview(&setting, cx);
        }
    }

    fn cancel_font(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_font_picker(false, cx);
        Self::focus_input(&self.inputs().settings, window, cx);
        cx.notify();
    }

    /// Every setting by section, filtered by the query; or the font picker.
    pub(super) fn render_settings(&self, window: &Window, cx: &mut Context<Self>) -> gpui_kit::Div {
        if self.settings_panel.font_picker().is_some() {
            return self.render_font_picker(cx);
        }
        if self.settings_panel.confirming_unlink() {
            return self.render_unlink_confirm(cx);
        }
        let config = Settings::get(cx).clone();
        let mono = cx.theme().mono_font_family.clone();
        let muted = cx.palette().text_muted;
        let border = cx.palette().border;
        let fields = self.settings_panel.fields();
        let selected = self.settings_panel.selected();
        let mut items = Vec::new();
        let mut section = None;
        for field in &fields {
            if section != Some(field.section()) {
                section = Some(field.section());
                items.push(section_header(field.section(), muted, mono.clone()));
            }
            items.push(self.render_setting(*field, &config, selected == Some(*field), cx));
        }
        let empty = fields.is_empty().then(|| {
            div()
                .px_3()
                .py_2()
                .text_sm()
                .text_color(muted)
                .child("No matches")
        });
        let path = Settings::path(cx).map_or_else(
            || "No config directory: not saved".to_owned(),
            |path| path.display().to_string(),
        );
        let max_height = (window.viewport_size().height - px(220.)).max(px(160.));
        v_flex()
            .key_context(SETTINGS)
            .on_action(cx.listener(|view, _: &SettingPrevious, _, cx| view.step_setting(-1, cx)))
            .on_action(cx.listener(|view, _: &SettingNext, _, cx| view.step_setting(1, cx)))
            .on_action(cx.listener(|view, _: &SettingDecrease, _, cx| view.change_selected(-1, cx)))
            .on_action(cx.listener(|view, _: &SettingIncrease, _, cx| view.change_selected(1, cx)))
            .on_action(cx.listener(|view, _: &Escape, window, cx| view.close_overlay(window, cx)))
            .child(
                div()
                    .p_2()
                    .border_b_1()
                    .border_color(border)
                    .child(Input::new(&self.inputs().settings).small()),
            )
            .child(
                div()
                    .id("settings-list")
                    .max_h(max_height)
                    .overflow_y_scroll()
                    .track_scroll(&self.settings_scroll)
                    .py_1()
                    .children(items)
                    .children(empty)
                    .test_support(),
            )
            .child(
                h_flex()
                    .px_3()
                    .py_2()
                    .gap_3()
                    .border_t_1()
                    .border_color(border)
                    .font_family(mono)
                    .text_xs()
                    .text_color(muted)
                    .child(
                        div()
                            .id("config-path")
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(path)
                            .test_support(),
                    )
                    .child("↑↓ ←→ ↵")
                    .child(
                        div()
                            .id("close-settings")
                            .cursor_pointer()
                            .text_color(cx.palette().action)
                            .child("esc close")
                            .on_click(
                                cx.listener(|view, _, window, cx| view.close_overlay(window, cx)),
                            )
                            .test_support(),
                    ),
            )
    }

    /// A row: marker, label and hint, then the value, which clicks change.
    fn render_setting(
        &self,
        field: Field,
        config: &config::Config,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let n = FIELDS.iter().position(|f| *f == field).unwrap_or(0) as u64;
        let palette = cx.palette();
        let (accent, muted) = (palette.accent, palette.text_muted);
        let mono = cx.theme().mono_font_family.clone();
        let read_only = field.read_only();
        let value =
            match field.value(config, self.account.as_ref()) {
                Value::Choice { options, selected } => {
                    let mut parts = Vec::new();
                    for (ix, option) in options.into_iter().enumerate() {
                        if ix > 0 {
                            parts.push(div().child("│").into_any_element());
                        }
                        let current = ix == selected;
                        parts.push(
                            div()
                                .id(("setting-option", n * 16 + ix as u64))
                                .when(current && !read_only, |el| el.text_color(accent))
                                .when(!current && !read_only, |el| {
                                    el.cursor_pointer()
                                        .hover(|style| style.underline())
                                        .on_click(cx.listener(move |_, _, _, cx| {
                                            if let Some(setting) = field.choose(ix) {
                                                settings::change(setting, cx);
                                            }
                                        }))
                                })
                                .child(option)
                                .test_support()
                                .into_any_element(),
                        );
                    }
                    h_flex().gap_1().children(parts).into_any_element()
                }
                Value::Number(text) => {
                    let arrow = |id: &'static str, glyph: &'static str, step: i32| {
                        div()
                            .id((id, n))
                            .px_1()
                            .cursor_pointer()
                            .hover(|style| style.text_color(accent))
                            .child(glyph)
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.change_setting(field, step, cx)
                            }))
                            .test_support()
                    };
                    h_flex()
                        .child(arrow("setting-less", "‹", -1))
                        .child(div().text_color(palette.text).child(text))
                        .child(arrow("setting-more", "›", 1))
                        .into_any_element()
                }
                Value::Toggle(on) => div()
                    .id(("setting-toggle", n))
                    .cursor_pointer()
                    .when(on, |el| el.text_color(accent))
                    .child(if on { "on" } else { "off" })
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.activate_setting(field, window, cx)
                    }))
                    .test_support()
                    .into_any_element(),
                Value::Font(font) => div()
                    .id(("setting-font", n))
                    .cursor_pointer()
                    .max_w(px(200.))
                    .truncate()
                    .when(font.is_some(), |el| el.text_color(palette.text))
                    .child(match (font, selected) {
                        (Some(font), true) => format!("{font} ↵"),
                        (Some(font), false) => font,
                        (None, true) => "default ↵".into(),
                        (None, false) => "default".into(),
                    })
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.activate_setting(field, window, cx)
                    }))
                    .test_support()
                    .into_any_element(),
                Value::Info(text) => div()
                    .max_w(px(220.))
                    .truncate()
                    .child(text)
                    .into_any_element(),
                Value::Action(glyph) => div()
                    .id(("setting-action", n))
                    .cursor_pointer()
                    .text_color(palette.danger)
                    .child(glyph)
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.activate_setting(field, window, cx)
                    }))
                    .test_support()
                    .into_any_element(),
            };
        h_flex()
            .id(("setting", n))
            .px_3()
            .py(px(3.))
            .gap_2()
            .items_start()
            .when(selected, |row| row.bg(palette.surface))
            .on_click(cx.listener(move |view, _, _, cx| {
                view.settings_panel.select(field);
                cx.notify();
            }))
            .child(
                div()
                    .flex_none()
                    .w(rems(0.75))
                    .font_family(mono.clone())
                    .text_color(accent)
                    .child(if selected { "▸" } else { "" }),
            )
            .child(
                v_flex().flex_1().min_w_0().child(field.label()).children(
                    field
                        .hint()
                        .map(|hint| div().text_xs().text_color(muted).child(hint)),
                ),
            )
            .child(
                div()
                    .flex_none()
                    .font_family(mono)
                    .text_sm()
                    .text_color(muted)
                    .child(value),
            )
            .test_support()
            .into_any_element()
    }

    /// Fonts to pick from, previewed as the highlight moves.
    fn render_font_picker(&self, cx: &mut Context<Self>) -> gpui_kit::Div {
        let installed = InstalledFonts::get(cx);
        let Some(picker) = self.settings_panel.font_picker() else {
            return div();
        };
        let palette = cx.palette();
        let (muted, border) = (palette.text_muted, palette.border);
        let mono = cx.theme().mono_font_family.clone();
        let rows: Vec<_> = picker
            .entries(&installed)
            .into_iter()
            .enumerate()
            .map(|(ix, entry)| {
                let current = (entry.font == picker.original).then(|| "current".into());
                picker_row(
                    ("font", ix as u64),
                    picker.selected() == ix,
                    None,
                    StyledText::new(entry.label)
                        .with_highlights(match_highlights(entry.highlights, cx)),
                    current,
                    muted,
                    mono.clone(),
                )
                .on_click(
                    cx.listener(move |view, _, window, cx| view.choose_font(Some(ix), window, cx)),
                )
                .into_any_element()
            })
            .collect();
        let empty = rows.is_empty().then(|| {
            div()
                .px_3()
                .py_2()
                .text_sm()
                .text_color(muted)
                .child("No matches")
        });
        let label = match picker.role {
            FontRole::Ui => Field::UiFont,
            FontRole::Mono => Field::MonoFont,
            FontRole::Body => Field::BodyFont,
        }
        .label();
        v_flex()
            .key_context(PICKING)
            .on_action(cx.listener(|view, _: &PickPrevious, _, cx| view.step_font(-1, cx)))
            .on_action(cx.listener(|view, _: &PickNext, _, cx| view.step_font(1, cx)))
            .on_action(cx.listener(|view, _: &Escape, window, cx| view.cancel_font(window, cx)))
            .child(
                h_flex()
                    .px_3()
                    .pt_2()
                    .gap_3()
                    .font_family(mono)
                    .text_xs()
                    .text_color(muted)
                    .child(div().flex_1().child(label))
                    .child("↑↓ preview · ↵ pick · esc back"),
            )
            .child(
                div()
                    .p_2()
                    .border_b_1()
                    .border_color(border)
                    .child(Input::new(&self.inputs().font).small()),
            )
            .child(v_flex().p_1().children(rows).children(empty))
    }
}

/// A muted section title in the settings list.
fn section_header(section: Section, muted: Hsla, mono: SharedString) -> AnyElement {
    div()
        .px_3()
        .pt_2()
        .pb_1()
        .font_family(mono)
        .text_xs()
        .text_color(muted)
        .child(section.label().to_uppercase())
        .into_any_element()
}

/// Where `field` is among the settings list's children, counting section
/// headers.
fn settings_item(fields: &[Field], field: Field) -> Option<usize> {
    let mut ix = 0;
    let mut section = None;
    for listed in fields {
        if section != Some(listed.section()) {
            section = Some(listed.section());
            ix += 1;
        }
        if *listed == field {
            return Some(ix);
        }
        ix += 1;
    }
    None
}
