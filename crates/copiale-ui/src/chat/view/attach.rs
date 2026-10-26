//! Attaching files to the next message: picked, dropped or pasted, then
//! waiting in the tray above the composer.

use std::path::PathBuf;
use std::sync::Arc;

use copiale_core::{AttachmentSource, MAX_ATTACHMENT_SIZE};
use gpui_kit::component::{ActiveTheme as _, h_flex};
use gpui_kit::{
    AnyElement, ClipboardEntry, Context, ExternalPaths, ImageFormat, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement as _, PathPromptOptions, RenderImage, SharedString,
    StatefulInteractiveElement as _, Styled as _, StyledImage as _, TestSupportExt as _, Window,
    div, img, px,
};
use url::Url;

use crate::chat::media::size_label;
use crate::chat::model::ToStage;
use crate::chat::text::visible;
use crate::icons::IconName;
use crate::media::{Picture, decode};
use crate::theme::ActivePalette as _;

use super::{AttachFiles, ChatView, PasteAttachments};

/// The tray's thumbnails' side, in pixels.
const THUMB: f32 = 28.;
/// Thumbnails are decoded to fit this square: room for HiDPI.
const THUMB_FIT: u32 = 96;

impl ChatView {
    /// Asks which files to attach.
    pub(super) fn attach_files(&mut self, _: &AttachFiles, _: &mut Window, cx: &mut Context<Self>) {
        if self.chat.open_id().is_none() {
            return;
        }
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach".into()),
        });
        cx.spawn(async move |view, cx| {
            if let Ok(Ok(Some(paths))) = chosen.await {
                view.update(cx, |view, cx| view.stage_paths(paths, cx)).ok();
            }
        })
        .detach();
    }

    /// Puts files in the tray; folders and what can't be read are skipped.
    pub(super) fn stage_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let files = paths
            .into_iter()
            .filter_map(|path| {
                let metadata = std::fs::metadata(&path).ok().filter(|m| m.is_file())?;
                let name = path.file_name()?.to_string_lossy().into_owned();
                Some(ToStage {
                    source: AttachmentSource::Path(path),
                    name,
                    size: metadata.len(),
                })
            })
            .collect();
        self.stage(files, cx);
    }

    fn stage(&mut self, files: Vec<ToStage>, cx: &mut Context<Self>) {
        let ids = self.chat.stage(files);
        for id in ids {
            self.load_staged_thumbnail(id, cx);
        }
        cx.notify();
    }

    /// Pastes images and files into the tray; anything else into the
    /// composer.
    pub(super) fn paste_attachments(
        &mut self,
        _: &PasteAttachments,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(item) = cx.read_from_clipboard() else {
            return cx.propagate();
        };
        let mut paths = Vec::new();
        let mut pasted = Vec::new();
        for entry in item.entries() {
            match entry {
                ClipboardEntry::Image(image) => {
                    let Some(extension) = extension(image.format) else {
                        continue;
                    };
                    pasted.push(ToStage {
                        size: image.bytes.len() as u64,
                        name: format!("image.{extension}"),
                        source: AttachmentSource::Pasted {
                            name: format!("image.{extension}"),
                            bytes: image.bytes.clone(),
                        },
                    });
                }
                ClipboardEntry::ExternalPaths(external) => {
                    paths.extend(external.paths().iter().cloned());
                }
                ClipboardEntry::String(text) => paths.extend(file_uris(text.text())),
            }
        }
        if paths.is_empty() && pasted.is_empty() {
            return cx.propagate();
        }
        self.stage(pasted, cx);
        self.stage_paths(paths, cx);
    }

    /// The button asking which files to attach.
    pub(super) fn render_attach_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        super::icon_button(
            "attach-button",
            IconName::Paperclip,
            "Attach files",
            &AttachFiles,
            false,
            cx,
        )
        .on_click(cx.listener(|view, _, window, cx| view.attach_files(&AttachFiles, window, cx)))
        .test_support()
    }

    /// Files dropped on the chat.
    pub(super) fn drop_paths(&mut self, paths: &ExternalPaths, cx: &mut Context<Self>) {
        self.stage_paths(paths.paths().to_vec(), cx);
    }

    fn unstage(&mut self, id: u64, cx: &mut Context<Self>) {
        self.chat.unstage(id);
        self.prune_staged_thumbnails(cx);
        cx.notify();
    }

    /// Decodes a staged picture's thumbnail in the background; files that
    /// aren't pictures get none.
    fn load_staged_thumbnail(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(staged) = self.chat.staged().iter().find(|s| s.id == id) else {
            return;
        };
        if !picture_name(&staged.name) {
            return;
        }
        let source = staged.source.clone();
        cx.spawn(async move |view, cx| {
            let decoded = cx
                .background_executor()
                .spawn(async move {
                    let bytes = match source {
                        AttachmentSource::Path(path) => read_limited(&path)?,
                        AttachmentSource::Pasted { bytes, .. } => bytes,
                    };
                    decode(&bytes, THUMB_FIT).ok()
                })
                .await;
            view.update(cx, |view, cx| {
                let still_staged = view.chat.staged().iter().any(|s| s.id == id);
                if let Some(decoded) = decoded.filter(|_| still_staged) {
                    view.staged_thumbnails
                        .insert(id, Picture::new(decoded).image);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Frees thumbnails of files no longer in the tray.
    pub(super) fn prune_staged_thumbnails(&mut self, cx: &mut Context<Self>) {
        let staged: Vec<u64> = self.chat.staged().iter().map(|s| s.id).collect();
        let gone: Vec<u64> = self
            .staged_thumbnails
            .keys()
            .filter(|id| !staged.contains(id))
            .copied()
            .collect();
        for id in gone {
            if let Some(image) = self.staged_thumbnails.remove(&id) {
                cx.drop_image(image, None);
            }
        }
    }

    /// The files waiting to go, above the composer.
    pub(super) fn render_tray(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let staged = self.chat.staged();
        if staged.is_empty() {
            return None;
        }
        let palette = cx.palette();
        let mono = cx.theme().mono_font_family.clone();
        let chips = staged.iter().map(|staged| {
            let id = staged.id;
            let thumbnail: Option<Arc<RenderImage>> = self.staged_thumbnails.get(&id).cloned();
            let preview = match thumbnail {
                Some(image) => img(image)
                    .size(px(THUMB))
                    .rounded(px(4.))
                    .object_fit(gpui_kit::ObjectFit::Cover)
                    .into_any_element(),
                None => div()
                    .size(px(THUMB))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.))
                    .bg(palette.background)
                    .child("▤")
                    .into_any_element(),
            };
            let original = picture_name(&staged.name).then(|| {
                div()
                    .id(("staged-original", id))
                    .flex_none()
                    .cursor_pointer()
                    .text_color(if staged.original {
                        palette.action
                    } else {
                        palette.text_muted
                    })
                    .hover(|style| style.underline())
                    .child(if staged.original {
                        "original ✓"
                    } else {
                        "original"
                    })
                    // Keeps the composer focused.
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.chat.toggle_original(id);
                        cx.notify();
                    }))
                    .test_support()
            });
            h_flex()
                .id(("staged", id))
                .gap_2()
                .pl_1()
                .pr_2()
                .py_1()
                .max_w(px(320.))
                .rounded(px(6.))
                .bg(palette.surface)
                .child(preview)
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(palette.text)
                        .child(SharedString::from(visible(&staged.name))),
                )
                .child(
                    div()
                        .flex_none()
                        .child(size_label(staged.size.min(u64::from(u32::MAX)) as u32)),
                )
                .children(original)
                .child(
                    div()
                        .id(("unstage", id))
                        .flex_none()
                        .cursor_pointer()
                        .hover(|style| style.text_color(palette.danger))
                        .child("✕")
                        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                        .on_click(cx.listener(move |view, _, _, cx| view.unstage(id, cx)))
                        .test_support(),
                )
                .test_support()
        });
        Some(
            h_flex()
                .id("tray")
                .flex_wrap()
                .gap_1()
                .font_family(mono)
                .text_xs()
                .text_color(palette.text_muted)
                .children(chips)
                .test_support()
                .into_any_element(),
        )
    }
}

/// The extension of pasted images the app sends; others are left out.
fn extension(format: ImageFormat) -> Option<&'static str> {
    match format {
        ImageFormat::Png => Some("png"),
        ImageFormat::Jpeg => Some("jpg"),
        ImageFormat::Webp => Some("webp"),
        ImageFormat::Gif => Some("gif"),
        _ => None,
    }
}

/// Whether a file's name says it's a picture that can be re-encoded or
/// shown.
fn picture_name(name: &str) -> bool {
    let extension = std::path::Path::new(name)
        .extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase());
    matches!(
        extension.as_deref(),
        Some("jpg" | "jpeg" | "png" | "webp" | "gif")
    )
}

/// Files a pasted text lists as `file://` URIs, one a line, as file
/// managers copy them; none unless every line is one.
fn file_uris(text: &str) -> Vec<PathBuf> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    let paths: Vec<PathBuf> = lines
        .iter()
        .filter_map(|line| {
            Url::parse(line)
                .ok()
                .filter(|url| url.scheme() == "file")?
                .to_file_path()
                .ok()
        })
        .collect();
    if paths.len() == lines.len() {
        paths
    } else {
        Vec::new()
    }
}

/// A file's bytes, unless larger than Signal takes.
fn read_limited(path: &std::path::Path) -> Option<Vec<u8>> {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(u64::from(MAX_ATTACHMENT_SIZE) + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= u64::from(MAX_ATTACHMENT_SIZE)).then_some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_lists_of_file_uris_paste_as_files() {
        assert_eq!(
            file_uris("file:///home/a/b%20c.jpg\r\nfile:///tmp/x.pdf\n"),
            [
                PathBuf::from("/home/a/b c.jpg"),
                PathBuf::from("/tmp/x.pdf")
            ]
        );
        assert!(file_uris("file:///tmp/x.pdf\nand some text").is_empty());
        assert!(file_uris("/etc/passwd").is_empty());
        assert!(file_uris("https://example.com/a.jpg").is_empty());
        assert!(file_uris("").is_empty());
    }

    #[test]
    fn pictures_are_known_by_name() {
        assert!(picture_name("IMG_1.JPG"));
        assert!(picture_name("a.webp"));
        assert!(!picture_name("notes.pdf"));
        assert!(!picture_name("jpg"));
    }
}
