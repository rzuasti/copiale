//! A video in the viewer: played through the sandboxed helper, with its
//! controls. Its own view, so playing redraws it alone.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use copiale_core::video::{Player, Status};
use copiale_core::{AttachmentId, FileContents};
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::{
    App, Bounds, Context, InteractiveElement as _, IntoElement, MouseButton, ObjectFit,
    ParentElement as _, Pixels, Render, RenderImage, SharedString, StatefulInteractiveElement as _,
    Styled as _, StyledImage as _, TestSupportExt as _, Window, canvas, div, img, px, relative,
};
use image::{Frame, RgbaImage};

use crate::media::VideoPlayback;
use crate::theme::ActivePalette as _;

/// Seconds the arrow keys move.
pub const SKIP_SECONDS: i64 = 5;
/// The controls' narrowest width.
const MIN_CONTROLS: f32 = 280.;

pub struct VideoView {
    id: AttachmentId,
    player: Option<Player>,
    /// The frame shown, and its size in pixels.
    image: Option<(Arc<RenderImage>, (u32, u32))>,
    /// Nothing to play: not loaded, or the helper refused it.
    failed: bool,
    /// The progress bar's bounds, for clicks.
    progress: Rc<Cell<Bounds<Pixels>>>,
}

impl VideoView {
    /// A video waiting for its contents.
    pub fn new(id: AttachmentId) -> Self {
        Self {
            id,
            player: None,
            image: None,
            failed: false,
            progress: Rc::default(),
        }
    }

    pub fn id(&self) -> AttachmentId {
        self.id
    }

    /// Whether it waits for its contents.
    pub fn loading(&self) -> bool {
        self.player.is_none() && !self.failed
    }

    /// Plays `contents`; fails without them.
    pub fn start(
        &mut self,
        playback: &VideoPlayback,
        contents: Option<&FileContents>,
        cx: &mut Context<Self>,
    ) {
        let started = contents.map(|contents| {
            Player::start(
                &playback.helper,
                contents.as_bytes().to_vec(),
                playback.sound,
            )
        });
        match started {
            Some(Ok(player)) => self.player = Some(player),
            Some(Err(error)) => {
                tracing::warn!(%error, "can't start the video helper");
                self.failed = true;
            }
            None => self.failed = true,
        }
        cx.notify();
    }

    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        if let Some(player) = &mut self.player {
            player.toggle(Instant::now());
            cx.notify();
        }
    }

    /// Moves `seconds` back or ahead.
    pub fn skip(&mut self, seconds: i64, cx: &mut Context<Self>) {
        if let Some(player) = &mut self.player {
            player.skip(seconds, Instant::now());
            cx.notify();
        }
    }

    pub fn toggle_mute(&mut self, cx: &mut Context<Self>) {
        if let Some(player) = &mut self.player {
            let muted = player.status(Instant::now()).muted;
            player.set_muted(!muted);
            cx.notify();
        }
    }

    /// Stops playing and frees the frame's GPU memory.
    pub fn stop(&mut self, cx: &mut App) {
        self.player = None;
        if let Some((image, _)) = self.image.take() {
            cx.drop_image(image, None);
        }
    }

    /// Shows the frame due, if new.
    fn advance(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<Status> {
        let now = Instant::now();
        let player = self.player.as_mut()?;
        if let Some(frame) = player.frame(now)
            && let Some(pixels) = RgbaImage::from_raw(frame.width, frame.height, frame.pixels)
        {
            let image = Arc::new(RenderImage::new([Frame::new(pixels)]));
            if let Some((old, _)) = self.image.replace((image, (frame.width, frame.height))) {
                cx.drop_image(old, Some(window));
            }
        }
        let status = player.status(now);
        if status.failed {
            self.failed = true;
        }
        if status.playing || status.waiting {
            window.request_animation_frame();
        }
        Some(status)
    }

    fn seek_to(&mut self, fraction: f32, cx: &mut Context<Self>) {
        let Some(player) = &mut self.player else {
            return;
        };
        let Some(duration) = player.status(Instant::now()).duration else {
            return;
        };
        player.seek(duration.mul_f32(fraction.clamp(0., 1.)));
        cx.notify();
    }
}

impl Render for VideoView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let status = self.advance(window, cx);
        let palette = cx.palette().clone();
        let mono = cx.theme().mono_font_family.clone();
        let available = window.viewport_size();
        let shown = self.image.clone().map(|(image, (width, height))| {
            let scale = window.scale_factor();
            let (width, height) = (width as f32 / scale, height as f32 / scale);
            // Videos may grow, unlike pictures: they're often small.
            let fit = (f32::from(available.width) * 0.9 / width)
                .min(f32::from(available.height) * 0.75 / height)
                .min(2.);
            (image, width * fit, height * fit)
        });
        let width = shown.as_ref().map_or(MIN_CONTROLS, |(_, width, _)| *width);
        let picture = shown.map(|(image, width, height)| {
            img(image)
                .id("video-frame")
                .w(px(width))
                .h(px(height))
                .object_fit(ObjectFit::Contain)
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|view, _, _, cx| view.toggle(cx)))
                .test_support()
        });
        let note = if self.failed {
            Some("can't play this · s save")
        } else if picture.is_none() {
            Some("loading…")
        } else {
            None
        };
        let note = note.map(|note| {
            div()
                .font_family(mono.clone())
                .text_color(palette.text_muted)
                .child(note)
        });
        let controls = status.filter(|_| !self.failed).map(|status| {
            let progress = match status.duration {
                Some(duration) if !duration.is_zero() => {
                    status.position.as_secs_f32() / duration.as_secs_f32()
                }
                _ => 0.,
            };
            let bounds = self.progress.clone();
            let clicked = self.progress.clone();
            let glyph = if status.playing { "❚❚" } else { "▶" };
            h_flex()
                .gap_2()
                .w(px(width.max(MIN_CONTROLS)))
                .font_family(mono.clone())
                .text_xs()
                .text_color(palette.text_muted)
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .id("video-play")
                        .flex_none()
                        .text_color(palette.text)
                        .cursor_pointer()
                        .child(glyph)
                        .on_click(cx.listener(|view, _, _, cx| view.toggle(cx)))
                        .test_support(),
                )
                .child(div().flex_none().child(clock(status.position)))
                .child(
                    div()
                        .id("video-progress")
                        .relative()
                        .flex_1()
                        .py_1()
                        .cursor_pointer()
                        .child(
                            canvas(move |area, _, _| bounds.set(area), |_, _, _, _| {})
                                .absolute()
                                .inset_0(),
                        )
                        .child(
                            div().h(px(3.)).rounded(px(2.)).bg(palette.border).child(
                                div()
                                    .h_full()
                                    .rounded(px(2.))
                                    .w(relative(progress.clamp(0., 1.)))
                                    .bg(palette.accent),
                            ),
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |view, event: &gpui_kit::MouseDownEvent, _, cx| {
                                let area = clicked.get();
                                if area.size.width > px(0.) {
                                    let fraction =
                                        (event.position.x - area.origin.x) / area.size.width;
                                    view.seek_to(fraction, cx);
                                }
                            }),
                        )
                        .test_support(),
                )
                .child(
                    div()
                        .flex_none()
                        .child(status.duration.map_or_else(|| "–".into(), clock)),
                )
                .children(status.muted.then(|| div().flex_none().child("muted")))
        });
        v_flex()
            .id("video")
            .items_center()
            .gap_2()
            .children(picture)
            .children(note)
            .children(controls)
            .test_support()
    }
}

/// `m:ss`, or `h:mm:ss` from an hour.
pub fn clock(time: Duration) -> SharedString {
    let seconds = time.as_secs();
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}").into()
    } else {
        format!("{minutes}:{seconds:02}").into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_read_as_clocks() {
        assert_eq!(clock(Duration::from_millis(4_900)), "0:04");
        assert_eq!(clock(Duration::from_secs(75)), "1:15");
        assert_eq!(clock(Duration::from_secs(3_725)), "1:02:05");
    }
}
