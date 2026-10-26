//! Attachments in the log, the picture viewer, and saving.

use std::sync::Arc;

use copiale_core::{
    Attachment, AttachmentId, AttachmentState, Command, FileContents, MessageId, downloads_dir,
};
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, AppContext as _, Bounds, Context, Entity, ImageSource,
    InteractiveElement as _, IntoElement, MouseButton, ObjectFit, ParentElement as _, PathBuilder,
    PathPromptOptions, Pixels, RenderImage, SharedString, StatefulInteractiveElement as _, Styled,
    StyledImage as _, TestSupportExt as _, Window, canvas, div, img, point, px, relative, white,
};

use crate::chat::media::{
    self, ALL_CORNERS, CELL, Corners, GAP, MAX_CELLS, RADIUS, Shown, box_size, cell_corners,
    file_action, file_line, shown, status_label,
};
use crate::chat::model::{Overlay, Row};
use crate::media::{Picture, Slot, Use, VideoPlayback, decode};
use crate::theme::ActivePalette as _;

use super::video::{SKIP_SECONDS, VideoView};
use super::{
    Cancel, ChatView, SaveViewed, ToggleMute, TogglePlay, VIEWER, ViewerNext, ViewerPrevious,
    ViewerStepNext, ViewerStepPrevious,
};

impl ChatView {
    /// A row's pictures and videos, alone or as an album, `space` above
    /// and below, then its file lines.
    pub(super) fn render_media(
        &self,
        row: &Row<'_>,
        space: Pixels,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        let message = row.message.id;
        let mut elements = Vec::new();
        match row.visual.as_slice() {
            [] => {}
            [one] => {
                let decoded = self.thumbnail_shape(one);
                let (width, height) = box_size(decoded, one);
                elements.push(self.render_visual(
                    message,
                    one,
                    (width, height),
                    ALL_CORNERS,
                    None,
                    cx,
                ));
            }
            many => {
                let extra = many.len().saturating_sub(MAX_CELLS);
                let count = many.len().min(MAX_CELLS);
                let cells = many.iter().take(count).enumerate().map(|(ix, attachment)| {
                    let more = (ix == MAX_CELLS - 1 && extra > 0).then_some(extra + 1);
                    let corners = cell_corners(ix, count);
                    self.render_visual(message, attachment, (CELL, CELL), corners, more, cx)
                });
                elements.push(
                    h_flex()
                        .flex_wrap()
                        .gap(px(GAP))
                        .w(px(CELL * 2. + GAP))
                        .children(cells)
                        .into_any_element(),
                );
            }
        }
        let mut elements: Vec<AnyElement> = elements
            .into_iter()
            .map(|visual| div().py(space).child(visual).into_any_element())
            .collect();
        elements.extend(
            row.files
                .iter()
                .map(|file| self.render_file(message, file, cx)),
        );
        elements
    }

    /// The decoded thumbnail's shape, if drawn already.
    fn thumbnail_shape(&self, attachment: &Attachment) -> Option<(u32, u32)> {
        let id = attachment.id?;
        match self.pictures.borrow_mut().get(id, Use::Thumbnail) {
            Some(Slot::Ready(picture)) => Some((picture.width, picture.height)),
            _ => None,
        }
    }

    /// A picture or video in a box of `(width, height)`, rounded at
    /// `corners`; `more` counts the album's pictures past the last cell.
    fn render_visual(
        &self,
        message: MessageId,
        attachment: &Attachment,
        (width, height): (f32, f32),
        corners: Corners,
        more: Option<usize>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let palette = cx.palette();
        let mono = cx.theme().mono_font_family.clone();
        let video = shown(attachment) == Some(Shown::Video);
        let picture = match (video, attachment.state, attachment.id) {
            (false, AttachmentState::Ready, Some(id)) => self.thumbnail(id),
            (true, AttachmentState::Ready, Some(id)) if attachment.has_preview => {
                self.video_frame(id)
            }
            _ => None,
        };
        let undrawable = picture.is_none()
            && !video
            && attachment.state == AttachmentState::Ready
            && attachment.id.is_some_and(|id| {
                matches!(
                    self.pictures.borrow_mut().get(id, Use::Thumbnail),
                    Some(Slot::Failed)
                )
            });
        // A lone one's box has its shape; an album's cells crop.
        let fit = if corners == ALL_CORNERS {
            ObjectFit::Contain
        } else {
            ObjectFit::Cover
        };
        let background = picture
            .map(|picture| ImageSource::Render(picture.image))
            .or_else(|| {
                let hash = attachment.blurhash.as_deref()?;
                self.pictures
                    .borrow_mut()
                    .preview(hash)
                    .map(ImageSource::Render)
            })
            .map(|source| {
                rounded(
                    img(source).absolute().inset_0().size_full().object_fit(fit),
                    corners,
                )
            });
        let label = if undrawable {
            Some("can't show this · save".to_owned())
        } else if video && !self.chat.videos_play() {
            Some(status_label(attachment).unwrap_or_else(|| "save".into()))
        } else {
            status_label(attachment)
        };
        // A large ▶ in the middle: anywhere on it plays.
        let play = video.then(|| {
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .child(canvas(|_, _, _| {}, paint_play).size(px(PLAY_SIZE)))
        });
        let label = label.map(|label| {
            div()
                .absolute()
                .left(px(8.))
                .bottom(px(8.))
                .px_1()
                .rounded(px(3.))
                .bg(palette.background.opacity(0.8))
                .font_family(mono.clone())
                .text_xs()
                .text_color(palette.text)
                .child(label)
        });
        let more = more.map(|count| {
            rounded(div(), corners)
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(palette.background.opacity(0.6))
                .font_family(mono)
                .text_color(palette.text)
                .child(format!("+{count}"))
        });
        let id = attachment.id.map_or(0, |id| id.get() as u64);
        let clicked = attachment.clone();
        rounded(div(), corners)
            .id(("attachment", id))
            .relative()
            .flex_none()
            .w(px(width))
            .h(px(height))
            .overflow_hidden()
            .bg(palette.surface)
            .cursor_pointer()
            .children(background)
            .children(play)
            .children(label)
            .children(more)
            .on_click(cx.listener(move |view, _, window, cx| {
                view.click_attachment(message, &clicked, window, cx)
            }))
            .test_support()
            .into_any_element()
    }

    /// A file's line: name, size and what can be done.
    fn render_file(&self, message: MessageId, file: &Attachment, cx: &Context<Self>) -> AnyElement {
        let palette = cx.palette();
        let id = file.id.map_or(0, |id| id.get() as u64);
        let state = match file.state {
            AttachmentState::Downloading => Some("downloading…"),
            _ => None,
        };
        let clicked = file.clone();
        let action = file_action(file).map(|label| {
            div()
                .id(("file-action", id))
                .flex_none()
                .text_color(palette.action)
                .cursor_pointer()
                .hover(|style| style.underline())
                .child(label)
                .on_click(cx.listener(move |view, _, window, cx| {
                    view.click_attachment(message, &clicked, window, cx)
                }))
                .test_support()
        });
        h_flex()
            .id(("file", id))
            .gap_1()
            .min_w_0()
            .font_family(cx.theme().mono_font_family.clone())
            .text_xs()
            .text_color(palette.text_muted)
            .child(div().min_w_0().truncate().child(file_line(file)))
            .children(state.map(|state| div().flex_none().child(format!("· {state}"))))
            .children(action.map(|action| h_flex().flex_none().gap_1().child("·").child(action)))
            .test_support()
            .into_any_element()
    }

    /// The thumbnail of a picture that's here, asking the core for it
    /// first.
    fn thumbnail(&self, id: AttachmentId) -> Option<Picture> {
        self.picture(id, Use::Thumbnail)
    }

    fn picture(&self, id: AttachmentId, use_: Use) -> Option<Picture> {
        self.load(id, use_, Command::LoadAttachment(id))
    }

    /// A video's first frame, as its thumbnail.
    fn video_frame(&self, id: AttachmentId) -> Option<Picture> {
        self.load(id, Use::Thumbnail, Command::LoadPreview(id))
    }

    /// The picture for `id` and `use_`, sending `command` to load it
    /// unless it's loading.
    fn load(&self, id: AttachmentId, use_: Use, command: Command) -> Option<Picture> {
        let mut pictures = self.pictures.borrow_mut();
        match pictures.get(id, use_) {
            Some(Slot::Ready(picture)) => Some(picture),
            Some(Slot::Loading | Slot::Failed) => None,
            None => {
                let first = pictures.loading(id).is_empty();
                pictures.want(id, use_);
                // One load serves every use waiting.
                if first {
                    self.core.send(command);
                }
                None
            }
        }
    }

    /// Decodes a loaded attachment for each use waiting for it.
    pub(super) fn attachment_loaded(
        &mut self,
        id: AttachmentId,
        contents: Option<&FileContents>,
        cx: &mut Context<Self>,
    ) {
        let uses = self.pictures.borrow().loading(id);
        let Some(contents) = contents else {
            for use_ in uses {
                self.picture_decoded(id, use_, Slot::Failed, cx);
            }
            return;
        };
        for use_ in uses {
            let contents = contents.clone();
            cx.spawn(async move |view, cx| {
                let decoded = cx
                    .background_executor()
                    .spawn(async move { decode(contents.as_bytes(), use_.fit()) })
                    .await;
                let slot = match decoded {
                    Ok(decoded) => Slot::Ready(Picture::new(decoded)),
                    Err(_) => Slot::Failed,
                };
                view.update(cx, |view, cx| view.picture_decoded(id, use_, slot, cx))
                    .ok();
            })
            .detach();
        }
    }

    fn picture_decoded(&mut self, id: AttachmentId, use_: Use, slot: Slot, cx: &mut Context<Self>) {
        let dropped = self.pictures.borrow_mut().finish(id, use_, slot);
        self.drop_pictures(dropped, cx);
        if use_ == Use::Thumbnail
            && let Some(ix) = self.chat.row_of_attachment(id)
        {
            // Its shape sets the box.
            self.log.remeasure_items(ix..ix + 1);
        }
        cx.notify();
    }

    /// Frees GPU memory of pictures no longer kept.
    pub(super) fn drop_pictures(
        &mut self,
        mut images: Vec<Arc<RenderImage>>,
        cx: &mut Context<Self>,
    ) {
        images.extend(self.pictures.borrow_mut().take_dropped());
        for image in images {
            cx.drop_image(image, None);
        }
    }

    /// A click on an attachment: shows a picture that's here, saves
    /// anything else that is, downloads the rest.
    pub(super) fn click_attachment(
        &mut self,
        message: MessageId,
        attachment: &Attachment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match (attachment.state, attachment.id) {
            (AttachmentState::Remote | AttachmentState::Failed, Some(id)) => {
                self.send(Some(Command::Download(id)));
            }
            (AttachmentState::Ready, Some(id))
                if !self.open_viewer(message, Some(id), window, cx) =>
            {
                self.save(vec![attachment.clone()], window, cx);
            }
            _ => {}
        }
    }

    /// Shows a message's pictures; `false` if none are here.
    pub(super) fn open_viewer(
        &mut self,
        message: MessageId,
        attachment: Option<AttachmentId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.chat.open_viewer(message, attachment) {
            return false;
        }
        window.focus(&self.viewer_focus, cx);
        self.sync_video(cx);
        cx.notify();
        true
    }

    /// Plays the video the viewer shows, if any, stopping any other.
    pub(super) fn sync_video(&mut self, cx: &mut Context<Self>) {
        let shown = self
            .chat
            .viewed()
            .filter(|viewed| shown(viewed.attachment) == Some(Shown::Video))
            .and_then(|viewed| viewed.attachment.id)
            .filter(|_| cx.has_global::<VideoPlayback>());
        let playing = self.video.as_ref().map(|video| video.read(cx).id());
        if shown == playing {
            return;
        }
        if let Some(video) = self.video.take() {
            video.update(cx, |video, cx| video.stop(cx));
        }
        if let Some(id) = shown {
            self.video = Some(cx.new(|_| VideoView::new(id)));
            self.core.send(Command::LoadAttachment(id));
        }
    }

    /// Plays a video loaded for the viewer.
    pub(super) fn video_loaded(
        &mut self,
        id: AttachmentId,
        contents: Option<&FileContents>,
        cx: &mut Context<Self>,
    ) {
        let Some(video) = &self.video else {
            return;
        };
        let waiting = video.read(cx);
        if waiting.id() != id || !waiting.loading() {
            return;
        }
        let Some(playback) = cx.try_global::<VideoPlayback>().cloned() else {
            return;
        };
        video.update(cx, |video, cx| video.start(&playback, contents, cx));
    }

    /// The viewer's video, if it shows one.
    fn viewed_video(&self) -> Option<&Entity<VideoView>> {
        let viewed = self.chat.viewed()?;
        self.video
            .as_ref()
            .filter(|_| shown(viewed.attachment) == Some(Shown::Video))
    }

    /// Asks where to save, then has the core write them: a file name for
    /// one, a folder for several.
    pub(super) fn save(
        &mut self,
        attachments: Vec<Attachment>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ids: Vec<_> = attachments.iter().filter_map(|a| a.id).collect();
        let dir = downloads_dir().unwrap_or_default();
        match attachments.as_slice() {
            [] => {}
            [one] => {
                let Some(id) = one.id else {
                    return;
                };
                let chosen = cx.prompt_for_new_path(&dir, Some(&media::save_name(one)));
                cx.spawn(async move |view, cx| {
                    let chosen = chosen.await;
                    view.update(cx, |view, cx| match chosen {
                        Ok(Ok(Some(path))) => view.send(Some(Command::SaveAttachment { id, path })),
                        Ok(Ok(None)) | Err(_) => {}
                        Ok(Err(error)) => view.save_dialog_failed(&error, cx),
                    })
                    .ok();
                })
                .detach();
            }
            _ => {
                let chosen = cx.prompt_for_paths(PathPromptOptions {
                    files: false,
                    directories: true,
                    multiple: false,
                    prompt: Some("Save here".into()),
                });
                cx.spawn(async move |view, cx| {
                    let chosen = chosen.await;
                    view.update(cx, |view, cx| match chosen {
                        Ok(Ok(Some(dirs))) => {
                            if let Some(dir) = dirs.into_iter().next() {
                                view.send(Some(Command::SaveAttachments { ids, dir }));
                            }
                        }
                        Ok(Ok(None)) | Err(_) => {}
                        Ok(Err(error)) => view.save_dialog_failed(&error, cx),
                    })
                    .ok();
                })
                .detach();
            }
        }
    }

    fn save_dialog_failed(&mut self, error: &dyn std::fmt::Display, cx: &mut Context<Self>) {
        tracing::warn!(%error, "the save dialog failed");
        self.chat
            .report_error(format!("couldn't open the save dialog: {error}"));
        cx.notify();
    }

    /// Saves a message's attachments that are here.
    pub(super) fn save_message(
        &mut self,
        message: MessageId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let attachments = self.chat.saveable(message);
        self.save(attachments, window, cx);
    }

    /// Downloads a message's attachments that aren't here.
    pub(super) fn download_message(&mut self, message: MessageId) {
        for command in self.chat.download(message) {
            self.send(Some(command));
        }
    }

    /// ←: back in a video, else the previous picture.
    fn viewer_previous(&mut self, _: &ViewerPrevious, _: &mut Window, cx: &mut Context<Self>) {
        match self.viewed_video().cloned() {
            Some(video) => video.update(cx, |video, cx| video.skip(-SKIP_SECONDS, cx)),
            None => self.step_viewer(-1, cx),
        }
    }

    /// →: ahead in a video, else the next picture.
    fn viewer_next(&mut self, _: &ViewerNext, _: &mut Window, cx: &mut Context<Self>) {
        match self.viewed_video().cloned() {
            Some(video) => video.update(cx, |video, cx| video.skip(SKIP_SECONDS, cx)),
            None => self.step_viewer(1, cx),
        }
    }

    fn viewer_step_previous(
        &mut self,
        _: &ViewerStepPrevious,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.step_viewer(-1, cx);
    }

    fn viewer_step_next(&mut self, _: &ViewerStepNext, _: &mut Window, cx: &mut Context<Self>) {
        self.step_viewer(1, cx);
    }

    fn step_viewer(&mut self, step: isize, cx: &mut Context<Self>) {
        if self.chat.viewer_step(step) {
            let shown = self.chat.viewed().and_then(|viewed| viewed.attachment.id);
            let dropped = self.pictures.borrow_mut().forget_full(shown);
            self.drop_pictures(dropped, cx);
            self.sync_video(cx);
            cx.notify();
        }
    }

    fn toggle_play(&mut self, _: &TogglePlay, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(video) = self.viewed_video().cloned() {
            video.update(cx, |video, cx| video.toggle(cx));
        }
    }

    fn toggle_mute(&mut self, _: &ToggleMute, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(video) = self.viewed_video().cloned() {
            video.update(cx, |video, cx| video.toggle_mute(cx));
        }
    }

    fn save_viewed(&mut self, _: &SaveViewed, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(viewed) = self.chat.viewed() {
            let attachment = viewed.attachment.clone();
            self.save(vec![attachment], window, cx);
        }
    }

    /// Frees the viewer's pictures and stops its video once it's closed.
    pub(super) fn viewer_closed(&mut self, cx: &mut Context<Self>) {
        let dropped = self.pictures.borrow_mut().forget_full(None);
        self.drop_pictures(dropped, cx);
        if let Some(video) = self.video.take() {
            video.update(cx, |video, cx| video.stop(cx));
        }
    }

    /// The picture or video over the whole window, scaled to fit, with its
    /// place among the message's.
    pub(super) fn render_viewer(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let palette = cx.palette();
        let mono = cx.theme().mono_font_family.clone();
        let viewed = self.chat.viewed();
        let video = self.viewed_video().cloned();
        let title = viewed.map(|viewed| {
            let name = viewed
                .attachment
                .name
                .as_deref()
                .filter(|name| !name.trim().is_empty())
                .map(crate::chat::text::visible)
                .unwrap_or_else(|| if video.is_some() { "video" } else { "image" }.into());
            match viewed.count {
                1 => name,
                count => format!("{name} · {}/{count}", viewed.position + 1),
            }
        });
        let several = viewed.is_some_and(|viewed| viewed.count > 1);
        let hints = match (&video, several) {
            (Some(_), true) => "space play · ← → 5s · shift ← → next · m mute · s save · esc close",
            (Some(_), false) => "space play · ← → 5s · m mute · s save · esc close",
            (None, true) => "← → · s save · esc close",
            (None, false) => "s save · esc close",
        };
        let picture = viewed
            .filter(|_| video.is_none())
            .and_then(|viewed| viewed.attachment.id)
            .and_then(|id| self.picture(id, Use::Full));
        let available = window.viewport_size();
        let picture = picture.map(|picture| {
            let scale = window.scale_factor();
            let (width, height) = (picture.width as f32 / scale, picture.height as f32 / scale);
            let fit = (f32::from(available.width) * 0.9 / width)
                .min(f32::from(available.height) * 0.8 / height)
                .min(1.);
            img(ImageSource::Render(picture.image))
                .w(px(width * fit))
                .h(px(height * fit))
                .object_fit(ObjectFit::Contain)
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        });
        let loading = (picture.is_none() && video.is_none()).then(|| {
            div()
                .font_family(mono.clone())
                .text_color(palette.text_muted)
                .child("loading…")
        });
        v_flex()
            .id("viewer")
            .key_context(VIEWER)
            .track_focus(&self.viewer_focus)
            .on_action(cx.listener(|view, _: &Cancel, window, cx| view.close_overlay(window, cx)))
            .on_action(cx.listener(Self::viewer_previous))
            .on_action(cx.listener(Self::viewer_next))
            .on_action(cx.listener(Self::viewer_step_previous))
            .on_action(cx.listener(Self::viewer_step_next))
            .on_action(cx.listener(Self::save_viewed))
            .on_action(cx.listener(Self::toggle_play))
            .on_action(cx.listener(Self::toggle_mute))
            .absolute()
            .inset_0()
            .items_center()
            .justify_center()
            .gap_2()
            .bg(palette.background.opacity(0.95))
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, _, window, cx| view.close_overlay(window, cx)),
            )
            .children(title.map(|title| {
                div()
                    .max_w(relative(0.9))
                    .truncate()
                    .font_family(mono.clone())
                    .text_sm()
                    .text_color(palette.text)
                    .child(SharedString::from(title))
            }))
            .children(picture)
            .children(video)
            .children(loading)
            .child(
                div()
                    .font_family(mono)
                    .text_xs()
                    .text_color(palette.text_muted)
                    .child(hints),
            )
            .test_support()
            .into_any_element()
    }

    /// Whether the viewer is open.
    pub(super) fn viewing(&self) -> bool {
        self.chat.overlay() == Some(Overlay::Viewer)
    }
}

/// The play arrow's box on a video.
const PLAY_SIZE: f32 = 44.;

/// A white triangle pointing right, filling `bounds`; drawn rather than a
/// glyph, which some fonts show as an emoji.
fn paint_play(bounds: Bounds<Pixels>, _: (), window: &mut Window, _: &mut App) {
    let at = |x: f32, y: f32| bounds.origin + point(bounds.size.width * x, bounds.size.height * y);
    let mut path = PathBuilder::fill();
    path.move_to(at(0.2, 0.1));
    path.line_to(at(0.9, 0.5));
    path.line_to(at(0.2, 0.9));
    path.close();
    if let Ok(path) = path.build() {
        window.paint_path(path, white().opacity(0.9));
    }
}

/// `element` with `corners` rounded.
fn rounded<E: Styled>(
    mut element: E,
    [top_left, top_right, bottom_right, bottom_left]: Corners,
) -> E {
    let radius = px(RADIUS);
    if top_left {
        element = element.rounded_tl(radius);
    }
    if top_right {
        element = element.rounded_tr(radius);
    }
    if bottom_right {
        element = element.rounded_br(radius);
    }
    if bottom_left {
        element = element.rounded_bl(radius);
    }
    element
}
