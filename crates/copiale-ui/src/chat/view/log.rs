//! The open conversation: header, messages in each style, composer.

use std::borrow::Cow;

use copiale_core::config::{MessageStyle, TimestampVisibility};
use copiale_core::{Direction, MessageStatus};
use gpui_kit::component::input::Textarea;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, Font, HighlightStyle, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, Pixels, Rems, SharedString, StatefulInteractiveElement as _, Styled as _,
    StyledText, TestSupportExt as _, TextRun, Window, div, font, list, px, relative, rems,
};

use crate::chat::jumbo::jumbo_size;
use crate::chat::layout::Spacing;
use crate::chat::model::{Row, member_list, title};
use crate::chat::text::{isolate, message_markdown};
use crate::kinetic::kinetic;
use crate::settings::Settings;
use crate::theme::{self, ActivePalette as _, Fonts};

use super::{
    Cancel, ChatView, DeleteForEveryone, DeleteForMe, EMOJI_PICKING, LOG_PADDING, TogglePalette,
    full_size, key_hint,
};

/// Load the previous page when the top visible row is this close to the
/// oldest loaded one.
const LOAD_OLDER_WITHIN: usize = 20;

const PREFIX_SIZE: Rems = rems(0.875);

const MAX_SENDER_CHARS: usize = 24;

impl ChatView {
    pub(super) fn render_conversation_pane(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let compact = !self.layout.shows_chat_list();
        let Some(summary) = self.chat.open_conversation() else {
            let text: SharedString = if !self.chat.has_conversations() {
                "Messages you receive will appear here".into()
            } else if compact {
                match key_hint(&TogglePalette, window) {
                    Some(key) => format!("{key} to pick a chat").into(),
                    None => "Pick a chat from the palette".into(),
                }
            } else {
                "Select a chat".into()
            };
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(cx.palette().text_muted)
                .child(text)
                .into_any_element();
        };
        let header = h_flex()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(cx.palette().border)
            .font_family(cx.theme().mono_font_family.clone())
            .child(div().text_color(cx.palette().text_muted).child("# "))
            .child(
                div()
                    .flex_none()
                    .max_w_1_2()
                    .truncate()
                    .child(title(summary)),
            )
            .children(member_list(summary).map(|members| {
                div()
                    .id("members")
                    .flex_1()
                    .min_w_0()
                    .ml_3()
                    .truncate()
                    .text_sm()
                    .text_color(cx.palette().text_muted)
                    .child(members)
                    .test_support()
            }))
            .when(compact, |header| {
                header
                    .child(div().flex_1())
                    .children(self.render_compact_status(cx))
            });

        if self.log.logical_scroll_top().item_ix < LOAD_OLDER_WITHIN {
            let command = self.chat.load_older();
            self.send(command);
        }

        v_flex()
            .size_full()
            .min_w_0()
            .child(header)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(self.log_part.clone().cached(full_size())),
            )
            .child(
                v_flex()
                    .px_4()
                    .py_2()
                    .gap_1()
                    .border_t_1()
                    .border_color(cx.palette().border)
                    .key_context(self.composer_context())
                    .on_action(cx.listener(Self::edit_last))
                    .on_action(cx.listener(Self::select_last))
                    .on_action(cx.listener(Self::select_previous))
                    .on_action(cx.listener(Self::select_next))
                    .on_action(cx.listener(Self::reply_selected))
                    .on_action(cx.listener(Self::edit_selected))
                    .on_action(cx.listener(Self::delete_selected))
                    .on_action(cx.listener(Self::delete_for_everyone))
                    .on_action(cx.listener(Self::delete_for_me))
                    .on_action(cx.listener(Self::cancel))
                    .children(self.chat.identity_banner().map(|(banner, peer)| {
                        h_flex()
                            .id("identity-banner")
                            .gap_2()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_size(PREFIX_SIZE)
                            .text_color(cx.palette().danger)
                            .child(div().flex_1().min_w_0().child(banner))
                            .child(
                                div()
                                    .id("review-safety-number")
                                    .cursor_pointer()
                                    .text_color(cx.palette().action)
                                    .child("review")
                                    .on_click(cx.listener(move |view, _, window, cx| {
                                        view.show_safety_number(peer, window, cx)
                                    }))
                                    .test_support(),
                            )
                            .test_support()
                    }))
                    .children(self.chat.compose_banner().map(|banner| {
                        h_flex()
                            .id("compose")
                            .gap_2()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_size(PREFIX_SIZE)
                            .text_color(cx.palette().text_muted)
                            .child(div().flex_1().min_w_0().truncate().child(banner))
                            .child(
                                div()
                                    .id("cancel-compose")
                                    .cursor_pointer()
                                    .text_color(cx.palette().action)
                                    .child("esc")
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.cancel(&Cancel, window, cx)
                                    }))
                                    .test_support(),
                            )
                            .test_support()
                    }))
                    .map(|composer| match self.render_request_banner(cx) {
                        Some(banner) => composer.child(banner),
                        None => composer
                            .relative()
                            .children(self.render_emoji_grid(cx))
                            .children(self.render_emoji_picker(cx))
                            .child(
                                div()
                                    .relative()
                                    .when(self.emoji.picker().is_some(), |input| {
                                        input.key_context(EMOJI_PICKING)
                                    })
                                    .on_action(cx.listener(Self::pick_emoji))
                                    .on_action(cx.listener(Self::emoji_previous))
                                    .on_action(cx.listener(Self::emoji_next))
                                    .on_action(cx.listener(Self::close_emoji_picker))
                                    // Room for the emoji button inside the frame.
                                    .child(Textarea::new(&self.inputs().composer).pr(px(32.)))
                                    .child(
                                        div()
                                            .absolute()
                                            .top_0()
                                            .bottom_0()
                                            .right_1()
                                            .flex()
                                            .items_center()
                                            .child(self.render_emoji_button(cx)),
                                    ),
                            ),
                    }),
            )
            .into_any_element()
    }

    /// The open conversation's messages; drawn as [`Part`].
    pub(super) fn render_log(&self, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity();
        let messages = list(self.log.clone(), move |ix, window, cx| {
            view.update(cx, |view, cx| view.render_row(ix, window, cx))
        })
        .size_full()
        .py(LOG_PADDING);
        div()
            .id("log")
            .relative()
            .size_full()
            .when_some(cx.try_global::<Fonts>().cloned(), |log, fonts| {
                log.font_family(fonts.body)
                    .line_height(relative(fonts.line_height))
            })
            .child(kinetic(
                "kinetic",
                &self.kinetic,
                &self.log,
                LOG_PADDING * 2.,
                messages,
            ))
            .vertical_scrollbar(&self.log)
            .test_support()
            .into_any_element()
    }

    fn render_row(&self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if let Some(row) = self.chat.row(ix) {
            let hovered = self.hovered == Some(row.message.id);
            let layout = &Settings::get(cx).layout;
            let style = LogStyle {
                message_style: layout.message_style,
                spacing: Spacing::for_density(layout.density),
                timestamps: layout.timestamps,
                time: self.chat.clock().time(row.message.sent_at),
            };
            return render_log_row(row, hovered, &style, window, cx);
        }
        match self.chat.typing_label() {
            Some(label) if self.typing_row => div()
                .id("typing")
                .px_4()
                .pt_1()
                .font_family(cx.theme().mono_font_family.clone())
                .text_size(PREFIX_SIZE)
                .line_height(window.line_height())
                .text_color(cx.palette().text_muted)
                .child(label)
                .test_support()
                .into_any_element(),
            _ => div().into_any_element(),
        }
    }
}

/// How message rows look, from the settings.
struct LogStyle {
    message_style: MessageStyle,
    spacing: Spacing,
    timestamps: TimestampVisibility,
    /// The message's own time, for showing on hover.
    time: String,
}

/// A message in the configured style, after its day label; system lines
/// look the same in every style.
fn render_log_row(
    row: Row<'_>,
    hovered: bool,
    style: &LogStyle,
    window: &mut Window,
    cx: &mut Context<ChatView>,
) -> AnyElement {
    let palette = cx.palette();
    let (muted, border, surface) = (palette.text_muted, palette.border, palette.surface);
    let mono = cx.theme().mono_font_family.clone();
    let day = row.day.clone().map(|day| {
        h_flex()
            .px_4()
            .pt_3()
            .pb_1()
            .gap_2()
            .font_family(mono.clone())
            .text_xs()
            .text_color(muted)
            .child(div().flex_1().h(px(1.)).bg(border))
            .child(day)
            .child(div().flex_1().h(px(1.)).bg(border))
    });
    if !row.system.is_empty() {
        let time = &row.prefix.time;
        return v_flex()
            .w_full()
            .children(day)
            .child(
                v_flex()
                    .px_4()
                    .pt_1()
                    .font_family(mono)
                    .text_size(PREFIX_SIZE)
                    .line_height(window.line_height())
                    .text_color(muted)
                    .children(
                        row.system
                            .iter()
                            .map(|line| div().child(format!("-- {line} · {time}"))),
                    ),
            )
            .into_any_element();
    }

    let shown = hovered || row.selected || row.confirming_delete;
    let bubbles = style.message_style == MessageStyle::Bubbles;
    let message = match style.message_style {
        MessageStyle::Log => log_row(&row, shown, style, window, cx),
        MessageStyle::Blocks => block_row(&row, shown, style, window, cx),
        MessageStyle::Bubbles => bubble_row(&row, shown, style, window, cx),
    };
    let id = row.message.id;
    v_flex()
        .w_full()
        .children(day)
        .child(
            message
                .id(("row", id.get() as u64))
                .on_hover(
                    cx.listener(move |view, hovered: &bool, _, cx| {
                        view.hover_row(id, *hovered, cx)
                    }),
                )
                .w_full()
                .px_4()
                // Bubbles mark the selected bubble instead.
                .when(shown && !bubbles, |this| this.bg(surface))
                .test_support(),
        )
        .into_any_element()
}

/// Log style: `sender @ time > text`; follow-ups in a group indent to the
/// first message's text.
fn log_row(
    row: &Row<'_>,
    shown: bool,
    style: &LogStyle,
    window: &mut Window,
    cx: &mut Context<ChatView>,
) -> gpui_kit::Div {
    let palette = cx.palette();
    let (muted, text) = (palette.text_muted, palette.text);
    let mono = cx.theme().mono_font_family.clone();
    let sender = isolate(&truncate(&row.prefix.sender, MAX_SENDER_CHARS));
    let prefix: SharedString = match style.timestamps {
        TimestampVisibility::Always => format!("{sender} @ {} >", row.prefix.time),
        TimestampVisibility::Hover | TimestampVisibility::Never => format!("{sender} >"),
    }
    .into();
    let font_size = PREFIX_SIZE.to_pixels(window.rem_size());
    let run = TextRun {
        len: prefix.len(),
        font: Font {
            fallbacks: theme::emoji_fallbacks(),
            ..font(mono.clone())
        },
        color: muted,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let width = window
        .text_system()
        .shape_line(prefix.clone(), font_size, &[run], None)
        .width;
    let color = sender_color(row, cx);
    let prefix = row.leads.then(|| {
        let sender_end = sender.len();
        div()
            .flex_none()
            .w(width)
            // The measured width may round below the drawn one.
            .whitespace_nowrap()
            .font_family(mono.clone())
            .text_size(PREFIX_SIZE)
            .line_height(window.line_height())
            .text_color(muted)
            .child(StyledText::new(prefix).with_highlights([(
                0..sender_end,
                HighlightStyle {
                    color: Some(color),
                    ..Default::default()
                },
            )]))
    });
    let indent = (!row.leads).then(|| div().flex_none().w(width));
    let hover_time = style.timestamps == TimestampVisibility::Hover && shown;
    h_flex()
        .items_start()
        .gap_2()
        .pt(group_gap(row, style.spacing, 1.))
        .children(prefix)
        .children(indent)
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .text_color(text)
                .children(message_text(row, window, cx))
                .children(shown.then(|| row_actions(row, muted, mono.clone(), cx))),
        )
        .children(message_meta(row, hover_time, &style.time, cx))
        .children(message_failures(row, cx))
}

/// Blocks style: the sender and time head each group, messages below.
fn block_row(
    row: &Row<'_>,
    shown: bool,
    style: &LogStyle,
    window: &mut Window,
    cx: &mut Context<ChatView>,
) -> gpui_kit::Div {
    let palette = cx.palette();
    let (muted, text) = (palette.text_muted, palette.text);
    let mono = cx.theme().mono_font_family.clone();
    let color = sender_color(row, cx);
    let header = row.leads.then(|| {
        h_flex()
            .gap_2()
            .items_baseline()
            .child(
                div()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(color)
                    .child(truncate(&row.prefix.sender, MAX_SENDER_CHARS).into_owned()),
            )
            .when(style.timestamps == TimestampVisibility::Always, |header| {
                header.child(
                    div()
                        .font_family(mono.clone())
                        .text_xs()
                        .text_color(muted)
                        .child(row.prefix.time.clone()),
                )
            })
    });
    let hover_time = style.timestamps == TimestampVisibility::Hover && shown;
    h_flex()
        .items_start()
        .gap_2()
        .pt(group_gap(row, style.spacing, 2.))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .text_color(text)
                .children(header)
                .children(message_text(row, window, cx))
                .children(shown.then(|| row_actions(row, muted, mono.clone(), cx))),
        )
        .children(message_meta(row, hover_time, &style.time, cx))
        .children(message_failures(row, cx))
}

/// Bubbles style: incoming on the left, ours on the right, with the time
/// and status inside; the sender's name heads each group.
fn bubble_row(
    row: &Row<'_>,
    shown: bool,
    style: &LogStyle,
    window: &mut Window,
    cx: &mut Context<ChatView>,
) -> gpui_kit::Div {
    let palette = cx.palette();
    let (muted, text, accent) = (palette.text_muted, palette.text, palette.accent);
    let surface = palette.surface;
    let mono = cx.theme().mono_font_family.clone();
    let outgoing = row.message.direction == Direction::Outgoing;
    let color = sender_color(row, cx);
    let name = row.leads.then(|| {
        div()
            .id(("bubble-name", row.message.id.get() as u64))
            .px_3()
            .text_xs()
            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
            .text_color(color)
            .child(truncate(&row.prefix.sender, MAX_SENDER_CHARS).into_owned())
            .test_support()
    });
    let time = match style.timestamps {
        TimestampVisibility::Always => true,
        TimestampVisibility::Hover => shown,
        TimestampVisibility::Never => false,
    };
    let meta = message_meta(row, time, &style.time, cx);
    let footer = (!meta.is_empty()).then(|| h_flex().justify_end().gap_1().children(meta));
    let marked = row.selected || row.confirming_delete;
    let bubble = v_flex()
        .id(("bubble", row.message.id.get() as u64))
        .min_w_0()
        .px_3()
        .py(px(6.))
        .rounded(px(12.))
        .bg(if outgoing {
            accent.opacity(0.2)
        } else {
            surface
        })
        .border_1()
        .border_color(if marked {
            accent
        } else {
            gpui_kit::transparent_black()
        })
        .text_color(text)
        .children(message_text(row, window, cx))
        .children(footer)
        .test_support();
    h_flex()
        .items_start()
        .pt(group_gap(row, style.spacing, 2.))
        .when(outgoing, |row| row.justify_end())
        .child(
            v_flex()
                .max_w(relative(0.75))
                .min_w_0()
                .gap_1()
                .when(outgoing, |column| column.items_end())
                .when(!outgoing, |column| column.items_start())
                .children(name)
                .child(bubble)
                .children(message_failures(row, cx))
                .children(shown.then(|| row_actions(row, muted, mono.clone(), cx))),
        )
}

/// Space above a message: wider before a new group, by `scale`.
fn group_gap(row: &Row<'_>, spacing: Spacing, scale: f32) -> Pixels {
    if row.leads {
        spacing.group * scale
    } else {
        spacing.message
    }
}

fn sender_color(row: &Row<'_>, cx: &App) -> Hsla {
    let palette = cx.palette();
    row.prefix
        .peer
        .map_or(palette.text_secondary, |peer| palette.nick(peer))
}

/// The quote and placeholder lines, then the text; links ask before
/// opening, and a few emoji alone draw large.
fn message_text(row: &Row<'_>, window: &Window, cx: &Context<ChatView>) -> Vec<AnyElement> {
    let mono = cx.theme().mono_font_family.clone();
    let muted = cx.palette().text_muted;
    let line_height = window.line_height();
    let id = row.message.id;
    row.quote
        .iter()
        .chain(&row.placeholders)
        .map(|line| {
            div()
                .font_family(mono.clone())
                .text_size(PREFIX_SIZE)
                .line_height(line_height)
                .text_color(muted)
                .truncate()
                .child(line.clone())
                .into_any_element()
        })
        .chain(row.message.content.body().map(|body| {
            let view = cx.weak_entity();
            TextView::markdown(("body", id.get() as u64), message_markdown(body.as_str()))
                .when_some(jumbo_size(body.as_str()), |view, size| {
                    view.text_size(rems(size))
                })
                .selectable(true)
                .on_link_click(move |href, _, window, cx| {
                    view.update(cx, |view, cx| view.ask_open_link(href, window, cx))
                        .ok();
                })
                .into_any_element()
        }))
        .collect()
}

/// Small muted glyphs: the time if `time`, edited, timer and status.
fn message_meta(row: &Row<'_>, time: bool, at: &str, cx: &App) -> Vec<AnyElement> {
    let palette = cx.palette();
    let muted = palette.text_muted;
    let mono = cx.theme().mono_font_family.clone();
    let status = match row.message.status {
        Some(MessageStatus::Pending) => Some(("…", muted)),
        Some(MessageStatus::Sent) => Some(("✓", muted)),
        Some(MessageStatus::Delivered) => Some(("✓✓", muted)),
        Some(MessageStatus::Read) => Some(("✓✓", palette.accent)),
        Some(MessageStatus::Failed) | None => None,
    };
    let glyph = |text: SharedString, color: Hsla| {
        div()
            .flex_none()
            .font_family(mono.clone())
            .text_xs()
            .text_color(color)
            .child(text)
            .into_any_element()
    };
    [
        time.then(|| glyph(at.to_owned().into(), muted)),
        row.message.edited().then(|| glyph("edited".into(), muted)),
        row.timer.clone().map(|timer| glyph(timer.into(), muted)),
        status.map(|(status, color)| glyph(status.into(), color)),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Sending it, or an edit or delete of it, failed.
fn message_failures(row: &Row<'_>, cx: &mut Context<ChatView>) -> Vec<AnyElement> {
    let palette = cx.palette();
    let (danger, action) = (palette.danger, palette.action);
    let mono = cx.theme().mono_font_family.clone();
    let id = row.message.id;
    let error: Option<SharedString> = row.error.map(|error| error.to_string().into());
    let failed = row.message.status == Some(MessageStatus::Failed);
    let failure = failed.then(|| {
        h_flex()
            .id(("retry", id.get() as u64))
            .flex_none()
            .gap_1()
            .font_family(mono.clone())
            .text_xs()
            .text_color(danger)
            .cursor_pointer()
            .child("not sent ·")
            .child(div().text_color(action).child("retry"))
            .when_some(error, |this, error| {
                this.tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(error.clone()).build(window, cx)
                })
            })
            .on_click(cx.listener(move |view, _, _, _| view.retry(id)))
            .test_support()
            .into_any_element()
    });
    let change_failed = row.change_failed.then(|| {
        div()
            .flex_none()
            .font_family(mono)
            .text_xs()
            .text_color(danger)
            .child("change not sent")
            .into_any_element()
    });
    failure.into_iter().chain(change_failed).collect()
}

/// Reply, edit and delete, or how to delete, with key hints when selected:
/// a line under the text of hovered and selected rows only.
fn row_actions(
    row: &Row<'_>,
    muted: Hsla,
    mono: SharedString,
    cx: &mut Context<ChatView>,
) -> AnyElement {
    let id = row.message.id;
    let n = id.get() as u64;
    let color = cx.palette().action;
    let action = |name: &'static str, label: &'static str| {
        div()
            .id((name, n))
            .text_color(color)
            .cursor_pointer()
            .hover(|style| style.underline())
            .child(label)
            .test_support()
    };
    let keys = row.selected;
    let mut items: Vec<AnyElement> = Vec::new();
    if row.confirming_delete {
        if row.actions.delete_for_everyone {
            items.push(
                action(
                    "for-everyone",
                    if keys {
                        "e for everyone"
                    } else {
                        "for everyone"
                    },
                )
                .on_click(cx.listener(|view, _, window, cx| {
                    view.delete_for_everyone(&DeleteForEveryone, window, cx)
                }))
                .into_any_element(),
            );
        }
        items.push(
            action("for-me", if keys { "m for me" } else { "for me" })
                .on_click(
                    cx.listener(|view, _, window, cx| view.delete_for_me(&DeleteForMe, window, cx)),
                )
                .into_any_element(),
        );
        items.push(
            action("cancel-delete", if keys { "esc cancel" } else { "cancel" })
                .on_click(cx.listener(|view, _, window, cx| view.cancel(&Cancel, window, cx)))
                .into_any_element(),
        );
    } else {
        if row.actions.reply {
            items.push(
                action("reply", if keys { "r reply" } else { "reply" })
                    .on_click(cx.listener(move |view, _, window, cx| view.reply_to(id, window, cx)))
                    .into_any_element(),
            );
        }
        if row.actions.edit {
            items.push(
                action("edit", if keys { "e edit" } else { "edit" })
                    .on_click(
                        cx.listener(move |view, _, window, cx| view.edit_message(id, window, cx)),
                    )
                    .into_any_element(),
            );
        }
        items.push(
            action("delete", if keys { "del delete" } else { "delete" })
                .on_click(cx.listener(move |view, _, _, cx| {
                    view.chat.confirm_delete(id);
                    cx.notify();
                }))
                .into_any_element(),
        );
    }
    let mut separated = Vec::new();
    for (ix, item) in items.into_iter().enumerate() {
        if ix > 0 {
            separated.push(div().child("·").into_any_element());
        }
        separated.push(item);
    }
    h_flex()
        .id(("actions", n))
        .flex_none()
        .gap_1()
        .font_family(mono)
        .text_xs()
        .text_color(muted)
        .children(separated)
        .test_support()
        .into_any_element()
}

/// `text`, cut to `max` characters with an ellipsis.
fn truncate(text: &str, max: usize) -> Cow<'_, str> {
    match text.char_indices().nth(max) {
        Some(_) => {
            let end = text.char_indices().nth(max - 1).map_or(0, |(ix, _)| ix);
            format!("{}…", &text[..end]).into()
        }
        None => text.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_names_are_truncated() {
        assert_eq!(truncate("Alice", 5), "Alice");
        assert_eq!(truncate("Alexandra", 5), "Alex…");
        assert_eq!(truncate("Ångström", 4), "Ång…");
    }
}
