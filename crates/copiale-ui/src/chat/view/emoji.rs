//! Emoji in the composer: conversions, the shortcode suggestions and the
//! emoji picker.

use std::ops::Range;

use copiale_core::{Command, RecentEmoji};
use gpui_kit::component::input::{Input, InputEvent, InputState, TextareaState};
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, Entity, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, ScrollStrategy, StatefulInteractiveElement as _, Styled as _, StyledText,
    TestSupportExt as _, UniformListScrollHandle, Window, div, px, uniform_list,
};

use crate::chat::emoji::Edit;
use crate::chat::emoji_grid::used;
use crate::chat::{CATEGORIES, COLUMNS, EmojiGrid, GridRow, RECENT};
use crate::icons::IconName;
use crate::settings::Settings;
use crate::theme::ActivePalette as _;

use super::overlays::picker_row;
use super::{
    ChatView, CloseEmojiGrid, CloseEmojiPicker, EMOJI_GRID, EmojiNext, EmojiPrevious, GridDown,
    GridLeft, GridRight, GridUp, PickEmoji, ToggleEmojiGrid, icon_button,
};

/// An emoji cell's side.
const CELL: f32 = 34.;
/// Rows the picker shows at once.
const VISIBLE_ROWS: f32 = 8.;
/// Stands for the recent emoji among the category tabs.
const RECENT_TAB: &str = "🕘";

/// The open emoji picker.
pub(super) struct GridState {
    grid: EmojiGrid,
    /// The composer's selection when opened; the emoji replaces it.
    at: Range<usize>,
    scroll: UniformListScrollHandle,
}

impl ChatView {
    /// After the composer's text changed: converts what was just typed and
    /// updates the picker.
    pub(super) fn composer_changed(
        &mut self,
        composer: &Entity<TextareaState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (text, cursor) = {
            let composer = composer.read(cx);
            (composer.value(), composer.cursor())
        };
        let emoticons = Settings::get(cx).composer.convert_emoticons;
        if let Some(edit) = self.emoji.changed(&text, cursor, emoticons) {
            apply(composer, edit, window, cx);
        }
        cx.notify();
    }

    fn pick_emoji_at(&mut self, ix: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(picker) = self.emoji.picker_mut() else {
            return;
        };
        if let Some(ix) = ix {
            picker.selected = ix;
        }
        let edit = picker.pick();
        self.insert_emoji(edit, window, cx);
    }

    /// Puts a picked emoji in the composer and first among the recent ones.
    fn insert_emoji(&mut self, edit: Edit, window: &mut Window, cx: &mut Context<Self>) {
        used(&mut self.recent_emoji, &edit.text);
        self.core.send(Command::SetRecentEmoji(RecentEmoji(
            self.recent_emoji.clone(),
        )));
        let composer = self.inputs().composer.clone();
        self.emoji.applying(&edit, &composer.read(cx).value());
        apply(&composer, edit, window, cx);
        composer.update(cx, |composer, cx| composer.focus(window, cx));
        cx.notify();
    }

    pub(super) fn pick_emoji(
        &mut self,
        _: &PickEmoji,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pick_emoji_at(None, window, cx);
    }

    pub(super) fn emoji_previous(
        &mut self,
        _: &EmojiPrevious,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(picker) = self.emoji.picker_mut() {
            picker.step(-1);
            cx.notify();
        }
    }

    pub(super) fn emoji_next(&mut self, _: &EmojiNext, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(picker) = self.emoji.picker_mut() {
            picker.step(1);
            cx.notify();
        }
    }

    pub(super) fn close_emoji_picker(
        &mut self,
        _: &CloseEmojiPicker,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.emoji.close_picker();
        cx.notify();
    }

    /// Matching emoji above the composer while typing `:name`.
    pub(super) fn render_emoji_picker(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let picker = self.emoji.picker()?;
        let muted = cx.palette().text_muted;
        let mono = cx.theme().mono_font_family.clone();
        let rows = picker.matches.iter().enumerate().map(|(ix, shortcode)| {
            picker_row(
                ("emoji", ix as u64),
                ix == picker.selected,
                Some(shortcode.emoji),
                StyledText::new(format!(":{}:", shortcode.name)),
                None,
                muted,
                mono.clone(),
            )
            .on_click(
                cx.listener(move |view, _, window, cx| view.pick_emoji_at(Some(ix), window, cx)),
            )
        });
        Some(
            v_flex()
                .id("emoji-picker")
                .font_family(mono.clone())
                .children(rows)
                .test_support()
                .into_any_element(),
        )
    }
}

impl ChatView {
    pub(super) fn toggle_emoji_grid(
        &mut self,
        _: &ToggleEmojiGrid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.emoji_grid.is_some() {
            self.close_emoji_grid(&CloseEmojiGrid, window, cx);
            return;
        }
        if self.chat.open_id().is_none()
            || self.chat.request_banner().is_some()
            || self.chat.overlay().is_some()
        {
            return;
        }
        let at = self.inputs().composer.read(cx).selected_range();
        self.emoji.close_picker();
        self.emoji_grid = Some(GridState {
            grid: EmojiGrid::new("", &self.recent_emoji),
            at,
            scroll: UniformListScrollHandle::new(),
        });
        self.inputs().emoji_search.update(cx, |search, cx| {
            search.set_value("", window, cx);
            search.focus(window, cx);
        });
        cx.notify();
    }

    /// Closes the picker, back to the composer.
    pub(super) fn close_emoji_grid(
        &mut self,
        _: &CloseEmojiGrid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.emoji_grid.take().is_some() {
            self.restore_focus(window, cx);
            cx.notify();
        }
    }

    /// Search changes, Enter and leaving the picker's search box.
    pub(super) fn on_emoji_search_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                let query = input.read(cx).value();
                if let Some(state) = &mut self.emoji_grid {
                    state.grid = EmojiGrid::new(&query, &self.recent_emoji);
                    state.scroll.scroll_to_item(0, ScrollStrategy::Top);
                    cx.notify();
                }
            }
            InputEvent::PressEnter { .. } => self.pick_from_grid(None, window, cx),
            InputEvent::Blur => {
                self.emoji_grid = None;
                cx.notify();
            }
            InputEvent::Focus => {}
        }
    }

    /// Inserts emoji `ix`, or the selected one, where the composer's
    /// selection was.
    fn pick_from_grid(&mut self, ix: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut state) = self.emoji_grid.take() else {
            return;
        };
        if let Some(ix) = ix {
            state.grid.select(ix);
        }
        let Some(picked) = state.grid.selected() else {
            self.emoji_grid = Some(state);
            return;
        };
        let len = self.inputs().composer.read(cx).value().len();
        let range = state.at.start.min(len)..state.at.end.min(len);
        let edit = Edit {
            range,
            text: picked.emoji.to_owned(),
        };
        self.insert_emoji(edit, window, cx);
    }

    fn move_in_grid(&mut self, change: impl FnOnce(&mut EmojiGrid), cx: &mut Context<Self>) {
        let Some(state) = &mut self.emoji_grid else {
            return;
        };
        change(&mut state.grid);
        if let Some(row) = state.grid.selected_row() {
            state.scroll.scroll_to_item(row, ScrollStrategy::Nearest);
        }
        cx.notify();
    }

    pub(super) fn grid_left(&mut self, _: &GridLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_in_grid(|grid| grid.step(-1), cx);
    }

    pub(super) fn grid_right(&mut self, _: &GridRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_in_grid(|grid| grid.step(1), cx);
    }

    pub(super) fn grid_up(&mut self, _: &GridUp, _: &mut Window, cx: &mut Context<Self>) {
        self.move_in_grid(|grid| grid.step_row(-1), cx);
    }

    pub(super) fn grid_down(&mut self, _: &GridDown, _: &mut Window, cx: &mut Context<Self>) {
        self.move_in_grid(|grid| grid.step_row(1), cx);
    }

    /// Scrolls to a category, selecting its first emoji.
    fn jump_to(&mut self, category: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        if self.emoji_grid.is_none() {
            return;
        }
        self.inputs()
            .emoji_search
            .update(cx, |search, cx| search.set_value("", window, cx));
        let Some(state) = &mut self.emoji_grid else {
            return;
        };
        state.grid = EmojiGrid::new("", &self.recent_emoji);
        if let Some(row) = state.grid.jump(category) {
            state.scroll.scroll_to_item(row, ScrollStrategy::Top);
        }
        cx.notify();
    }

    /// The button opening the picker.
    pub(super) fn render_emoji_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        icon_button(
            "emoji-button",
            IconName::FaceSlightlySmiling,
            "Emoji",
            &ToggleEmojiGrid,
            self.emoji_grid.is_some(),
            cx,
        )
        // Keeps the composer's selection for the emoji to replace.
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .on_click(
            cx.listener(|view, _, window, cx| view.toggle_emoji_grid(&ToggleEmojiGrid, window, cx)),
        )
        .test_support()
    }

    /// The picker above the composer: search, the grid and category tabs.
    pub(super) fn render_emoji_grid(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state = self.emoji_grid.as_ref()?;
        let palette = cx.palette();
        let muted = palette.text_muted;
        let mono = cx.theme().mono_font_family.clone();
        let rows = state.grid.rows().len();
        let body = if rows == 0 {
            div()
                .h(px(CELL * VISIBLE_ROWS))
                .p_2()
                .text_color(muted)
                .child("No emoji found")
                .into_any_element()
        } else {
            uniform_list(
                "emoji-grid-rows",
                rows,
                cx.processor(|view, range: Range<usize>, _, cx| view.render_grid_rows(range, cx)),
            )
            .track_scroll(&state.scroll)
            .h(px(CELL * VISIBLE_ROWS))
            .into_any_element()
        };
        let hover = palette.surface;
        let recent = (!self.recent_emoji.is_empty()).then_some((RECENT, RECENT_TAB));
        let tabs = recent
            .into_iter()
            .chain(CATEGORIES)
            .map(|(category, glyph)| {
                div()
                    .id(category)
                    .flex_1()
                    .flex()
                    .justify_center()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .hover(move |tab| tab.bg(hover))
                    .on_click(
                        cx.listener(move |view, _, window, cx| view.jump_to(category, window, cx)),
                    )
                    .child(glyph)
                    .test_support()
            });
        let name = state
            .grid
            .selected()
            .map(|picked| format!(":{}:", picked.name));
        Some(
            v_flex()
                .id("emoji-grid")
                .key_context(EMOJI_GRID)
                .on_action(cx.listener(Self::grid_left))
                .on_action(cx.listener(Self::grid_right))
                .on_action(cx.listener(Self::grid_up))
                .on_action(cx.listener(Self::grid_down))
                .on_action(cx.listener(Self::close_emoji_grid))
                .occlude()
                .absolute()
                .bottom_full()
                .right_4()
                .w(px(CELL * COLUMNS as f32 + 16.))
                .p_2()
                .gap_1()
                .bg(cx.theme().popover)
                .border_1()
                .border_color(palette.border)
                .rounded(px(6.))
                .shadow_lg()
                .child(Input::new(&self.inputs().emoji_search))
                .child(body)
                .child(h_flex().gap_1().children(tabs))
                .child(
                    div()
                        .h(px(16.))
                        .font_family(mono)
                        .text_xs()
                        .text_color(muted)
                        .truncate()
                        .children(name),
                )
                .test_support()
                .into_any_element(),
        )
    }

    fn render_grid_rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(state) = &self.emoji_grid else {
            return Vec::new();
        };
        let palette = cx.palette();
        let (muted, hover) = (palette.text_muted, palette.surface);
        let highlight = palette.accent.opacity(0.25);
        let selected = state.grid.selected_ix();
        range
            .filter_map(|ix| state.grid.rows().get(ix))
            .map(|row| match row {
                GridRow::Header(header) => div()
                    .h(px(CELL))
                    .flex()
                    .items_end()
                    .pb_1()
                    .text_xs()
                    .text_color(muted)
                    .child(*header)
                    .into_any_element(),
                GridRow::Cells(cells) => h_flex()
                    .h(px(CELL))
                    .children(cells.clone().map(|ix| {
                        div()
                            .id(("emoji-cell", ix as u64))
                            .size(px(CELL))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .text_size(px(CELL * 0.65))
                            .cursor_pointer()
                            .when(ix == selected, |cell| cell.bg(highlight))
                            .hover(move |cell| cell.bg(hover))
                            .on_click(cx.listener(move |view, _, window, cx| {
                                view.pick_from_grid(Some(ix), window, cx)
                            }))
                            .child(state.grid.cells()[ix].emoji)
                            .test_support()
                    }))
                    .into_any_element(),
            })
            .collect()
    }
}

fn apply(
    composer: &Entity<TextareaState>,
    edit: Edit,
    window: &mut Window,
    cx: &mut Context<ChatView>,
) {
    composer.update(cx, |composer, cx| {
        composer.set_selected_range(edit.range, cx);
        composer.replace(edit.text, window, cx);
    });
}
