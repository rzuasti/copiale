//! Attachments in the log, the picture viewer, and saving.

use std::sync::Arc;

use copiale_core::{
    Attachment, AttachmentId, AttachmentState, Command, FileContents, MessageId, downloads_dir,
};
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, ImageSource, InteractiveElement as _, IntoElement, MouseButton, ObjectFit,
    ParentElement as _, PathPromptOptions, RenderImage, SharedString,
    StatefulInteractiveElement as _, Styled as _, StyledImage as _, TestSupportExt as _, Window,
    div, img, px, relative,
};

use crate::chat::media::{
    self, CELL, MAX_CELLS, Shown, box_size, file_action, file_line, shown, status_label,
};
use crate::chat::model::{Overlay, Row};
use crate::media::{Picture, Slot, Use, decode};
use crate::theme::ActivePalette as _;

use super::{Cancel, ChatView, SaveViewed, VIEWER, ViewerNext, ViewerPrevious};

impl ChatView {
    /// A row's pictures and videos, alone or as an album, then its file
    /// lines.
    pub(super) fn render_media(&self, row: &Row<'_>, cx: &Context<Self>) -> Vec<AnyElement> {
        let message = row.message.id;
        let mut elements = Vec::new();
        match row.visual.as_slice() {
            [] => {}
            [one] => {
                let decoded = self.thumbnail_shape(one);
                let (width, height) = box_size(decoded, one);
                elements.push(self.render_visual(message, one, width, height, None, cx));
            }
            many => {
                let extra = many.len().saturating_sub(MAX_CELLS);
                let cells = many
                    .iter()
                    .take(MAX_CELLS)
                    .enumerate()
                    .map(|(ix, attachment)| {
                        let more = (ix == MAX_CELLS - 1 && extra > 0).then_some(extra + 1);
                        self.render_visual(message, attachment, CELL, CELL, more, cx)
                    });
                elements.push(
                    h_flex()
                        .flex_wrap()
                        .gap_1()
                        .w(px(CELL * 2. + 4.))
                        .children(cells)
                        .into_any_element(),
                );
            }
        }
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

    /// A picture or video in a box of `width` by `height`; `more` counts
    /// the album's pictures past the last cell.
    fn render_visual(
        &self,
        message: MessageId,
        attachment: &Attachment,
        width: f32,
        height: f32,
        more: Option<usize>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let palette = cx.palette();
        let mono = cx.theme().mono_font_family.clone();
        let video = shown(attachment) == Some(Shown::Video);
        let picture = match (video, attachment.state, attachment.id) {
            (false, AttachmentState::Ready, Some(id)) => self.thumbnail(id),
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
        let fit = if more.is_some() || width == CELL {
            ObjectFit::Cover
        } else {
            ObjectFit::Contain
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
            .map(|source| img(source).absolute().inset_0().size_full().object_fit(fit));
        let label = if undrawable {
            Some("can't show this · save".to_owned())
        } else if video {
            Some(match status_label(attachment) {
                Some(status) => format!("▶ {status}"),
                None => "▶ save".into(),
            })
        } else {
            status_label(attachment)
        };
        let label = label.map(|label| {
            div()
                .absolute()
                .left(px(4.))
                .bottom(px(4.))
                .px_1()
                .rounded(px(3.))
                .bg(palette.background.opacity(0.8))
                .font_family(mono.clone())
                .text_xs()
                .text_color(palette.text)
                .child(label)
        });
        let more = more.map(|count| {
            div()
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
        div()
            .id(("attachment", id))
            .relative()
            .flex_none()
            .w(px(width))
            .h(px(height))
            .overflow_hidden()
            .rounded(px(4.))
            .bg(palette.surface)
            .cursor_pointer()
            .children(background)
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
        let mut pictures = self.pictures.borrow_mut();
        match pictures.get(id, use_) {
            Some(Slot::Ready(picture)) => Some(picture),
            Some(Slot::Loading | Slot::Failed) => None,
            None => {
                let first = pictures.loading(id).is_empty();
                pictures.want(id, use_);
                // One load serves every use waiting.
                if first {
                    self.core.send(Command::LoadAttachment(id));
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
        cx.notify();
        true
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

    fn viewer_previous(&mut self, _: &ViewerPrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.step_viewer(-1, cx);
    }

    fn viewer_next(&mut self, _: &ViewerNext, _: &mut Window, cx: &mut Context<Self>) {
        self.step_viewer(1, cx);
    }

    fn step_viewer(&mut self, step: isize, cx: &mut Context<Self>) {
        if self.chat.viewer_step(step) {
            let shown = self.chat.viewed().and_then(|viewed| viewed.attachment.id);
            let dropped = self.pictures.borrow_mut().forget_full(shown);
            self.drop_pictures(dropped, cx);
            cx.notify();
        }
    }

    fn save_viewed(&mut self, _: &SaveViewed, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(viewed) = self.chat.viewed() {
            let attachment = viewed.attachment.clone();
            self.save(vec![attachment], window, cx);
        }
    }

    /// Frees the viewer's pictures once it's closed.
    pub(super) fn viewer_closed(&mut self, cx: &mut Context<Self>) {
        let dropped = self.pictures.borrow_mut().forget_full(None);
        self.drop_pictures(dropped, cx);
    }

    /// The picture over the whole window, scaled to fit, with its place
    /// among the message's pictures.
    pub(super) fn render_viewer(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let palette = cx.palette();
        let mono = cx.theme().mono_font_family.clone();
        let viewed = self.chat.viewed();
        let title = viewed.map(|viewed| {
            let name = viewed
                .attachment
                .name
                .as_deref()
                .filter(|name| !name.trim().is_empty())
                .map(crate::chat::text::visible)
                .unwrap_or_else(|| "image".into());
            match viewed.count {
                1 => name,
                count => format!("{name} · {}/{count}", viewed.position + 1),
            }
        });
        let picture = viewed
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
        let loading = picture.is_none().then(|| {
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
            .on_action(cx.listener(Self::save_viewed))
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
            .children(loading)
            .child(
                div()
                    .font_family(mono)
                    .text_xs()
                    .text_color(palette.text_muted)
                    .child("← → · s save · esc close"),
            )
            .test_support()
            .into_any_element()
    }

    /// Whether the viewer is open.
    pub(super) fn viewing(&self) -> bool {
        self.chat.overlay() == Some(Overlay::Viewer)
    }
}
