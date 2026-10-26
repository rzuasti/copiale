//! How a message's attachments show in the log, and which the viewer steps
//! through.

use copiale_core::{Attachment, AttachmentId, AttachmentKind, AttachmentState, Content};

/// Largest box an image or video takes in the log, in logical pixels.
pub const MAX_BOX: (f32, f32) = (320., 240.);
/// One cell of an album's grid, in logical pixels.
pub const CELL: f32 = 120.;
/// Cells an album shows; a last one counts the rest.
pub const MAX_CELLS: usize = 4;
/// Space between an album's cells, in logical pixels.
pub const GAP: f32 = 2.;
/// Corner radius of a picture or video, or an album, in logical pixels.
pub const RADIUS: f32 = 14.;

/// Which corners are rounded: top left, top right, bottom right, bottom
/// left.
pub type Corners = [bool; 4];

/// A lone picture or video's corners.
pub const ALL_CORNERS: Corners = [true; 4];

/// The corners of cell `ix` of an album of `count` cells, two a row, that
/// are the album's outer ones, so rounded.
pub fn cell_corners(ix: usize, count: usize) -> Corners {
    let left = ix.is_multiple_of(2);
    let last_row = ix + 2 >= count;
    let right_edge = !left || ix + 1 == count;
    [
        ix == 0,
        ix == 1 || (ix == 0 && count == 1),
        last_row && right_edge,
        last_row && left,
    ]
}

/// How an attachment shows in the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shown {
    /// An image or GIF, drawn once downloaded.
    Picture,
    /// A preview to play elsewhere.
    Video,
    /// A line with its name and size.
    File,
}

/// How `attachment` shows; `None` if only a placeholder line stands for it:
/// it can't be downloaded here, or is gone from Signal's servers.
pub fn shown(attachment: &Attachment) -> Option<Shown> {
    if matches!(
        attachment.state,
        AttachmentState::Unavailable | AttachmentState::Expired
    ) {
        return None;
    }
    Some(match attachment.kind {
        _ if attachment.is_video() => Shown::Video,
        AttachmentKind::Image | AttachmentKind::Gif | AttachmentKind::Video => Shown::Picture,
        AttachmentKind::File | AttachmentKind::Audio | AttachmentKind::Voice => Shown::File,
    })
}

/// `content`'s attachments shown as pictures or videos, and as file lines.
pub fn split(content: &Content) -> (Vec<&Attachment>, Vec<&Attachment>) {
    let Content::Text { attachments, .. } = content else {
        return (Vec::new(), Vec::new());
    };
    let mut visual = Vec::new();
    let mut files = Vec::new();
    for attachment in attachments {
        match shown(attachment) {
            Some(Shown::Picture | Shown::Video) => visual.push(attachment),
            Some(Shown::File) => files.push(attachment),
            None => {}
        }
    }
    (visual, files)
}

/// The pictures of `content` that are here, which the viewer steps through.
pub fn viewable(content: &Content) -> Vec<AttachmentId> {
    let Content::Text { attachments, .. } = content else {
        return Vec::new();
    };
    attachments
        .iter()
        .filter(|a| a.state == AttachmentState::Ready && shown(a) == Some(Shown::Picture))
        .filter_map(|a| a.id)
        .collect()
}

/// The attachments of `content` that are here, to save.
pub fn saveable(content: &Content) -> Vec<&Attachment> {
    match content {
        Content::Text { attachments, .. } => attachments
            .iter()
            .filter(|a| a.state == AttachmentState::Ready && a.id.is_some())
            .collect(),
        _ => Vec::new(),
    }
}

/// The attachments of `content` to download when asked.
pub fn downloadable(content: &Content) -> Vec<AttachmentId> {
    match content {
        Content::Text { attachments, .. } => attachments
            .iter()
            .filter(|a| matches!(a.state, AttachmentState::Remote | AttachmentState::Failed))
            .filter_map(|a| a.id)
            .collect(),
        _ => Vec::new(),
    }
}

/// A size for people: `512 B`, `12 KB`, `2.3 MB`.
pub fn size_label(bytes: u32) -> String {
    const KB: f64 = 1024.;
    let bytes = f64::from(bytes);
    if bytes < KB {
        format!("{bytes} B")
    } else if bytes < KB * KB {
        format!("{:.0} KB", bytes / KB)
    } else {
        format!("{:.1} MB", bytes / (KB * KB))
    }
}

/// The box a lone picture or video takes, in logical pixels: its shape as
/// decoded, else as its sender says, else 4:3, fit in [`MAX_BOX`] and never
/// enlarged.
pub fn box_size(decoded: Option<(u32, u32)>, attachment: &Attachment) -> (f32, f32) {
    let sent = attachment.width.zip(attachment.height);
    let (width, height) = match decoded.or(sent) {
        Some((width, height)) if width > 0 && height > 0 => (width as f32, height as f32),
        _ => (4., 3.),
    };
    let scale = (MAX_BOX.0 / width).min(MAX_BOX.1 / height);
    // Tiny images stay small, but not so small they can't be clicked.
    let scale = if decoded.or(sent).is_some() {
        scale.min(1.)
    } else {
        scale
    };
    ((width * scale).max(24.), (height * scale).max(24.))
}

/// What a picture or video not yet drawn says over its preview.
pub fn status_label(attachment: &Attachment) -> Option<String> {
    let size = attachment.size.map(size_label);
    match attachment.state {
        AttachmentState::Remote => Some(match size {
            Some(size) => format!("↓ {size}"),
            None => "↓".into(),
        }),
        AttachmentState::Downloading => Some("downloading…".into()),
        AttachmentState::Failed => Some("failed · retry".into()),
        _ => None,
    }
}

/// A file line: its name, size and state.
pub fn file_line(attachment: &Attachment) -> String {
    let name = attachment
        .name
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .map(super::text::visible)
        .unwrap_or_else(|| kind_name(attachment.kind).into());
    match attachment.size {
        Some(size) => format!("▤ {name} · {}", size_label(size)),
        None => format!("▤ {name}"),
    }
}

/// What a file line offers.
pub fn file_action(attachment: &Attachment) -> Option<&'static str> {
    match attachment.state {
        AttachmentState::Remote => Some("download"),
        AttachmentState::Downloading => None,
        AttachmentState::Failed => Some("retry"),
        AttachmentState::Ready => Some("save"),
        AttachmentState::Unavailable | AttachmentState::Expired => None,
    }
}

fn kind_name(kind: AttachmentKind) -> &'static str {
    match kind {
        AttachmentKind::Image => "image",
        AttachmentKind::Gif => "GIF",
        AttachmentKind::Video => "video",
        AttachmentKind::Voice => "voice message",
        AttachmentKind::Audio => "audio",
        AttachmentKind::File => "file",
    }
}

/// The name a save dialog suggests.
pub fn save_name(attachment: &Attachment) -> String {
    copiale_core::text::save_name(
        attachment.name.as_deref(),
        attachment.content_type.as_deref(),
    )
}

#[cfg(test)]
mod tests {
    use copiale_core::MessageBody;

    use super::*;

    fn attachment(kind: AttachmentKind, state: AttachmentState, id: i64) -> Attachment {
        Attachment {
            id: Some(AttachmentId::new(id)),
            state,
            size: Some(2_400_000),
            ..Attachment::new(kind)
        }
    }

    fn content(attachments: Vec<Attachment>) -> Content {
        Content::Text {
            body: MessageBody::new(""),
            attachments,
            quote: None,
        }
    }

    #[test]
    fn attachments_show_by_kind_and_state() {
        use AttachmentKind::*;
        use AttachmentState::*;
        let gif_video = Attachment {
            content_type: Some("video/mp4".into()),
            ..attachment(Gif, Remote, 1)
        };

        assert_eq!(shown(&attachment(Image, Ready, 1)), Some(Shown::Picture));
        assert_eq!(shown(&attachment(Gif, Remote, 1)), Some(Shown::Picture));
        assert_eq!(shown(&gif_video), Some(Shown::Video));
        assert_eq!(
            shown(&attachment(Video, Downloading, 1)),
            Some(Shown::Video)
        );
        assert_eq!(shown(&attachment(Voice, Ready, 1)), Some(Shown::File));
        assert_eq!(shown(&attachment(File, Failed, 1)), Some(Shown::File));
        assert_eq!(shown(&attachment(Image, Unavailable, 1)), None);
        assert_eq!(shown(&attachment(Image, Expired, 1)), None);
    }

    #[test]
    fn the_viewer_steps_through_pictures_that_are_here() {
        use AttachmentKind::*;
        use AttachmentState::*;
        let content = content(vec![
            attachment(Image, Ready, 1),
            attachment(Video, Ready, 2),
            attachment(Image, Remote, 3),
            attachment(Gif, Ready, 4),
            attachment(File, Ready, 5),
        ]);

        let ids: Vec<_> = viewable(&content)
            .into_iter()
            .map(AttachmentId::get)
            .collect();
        assert_eq!(ids, [1, 4]);
        let saved: Vec<_> = saveable(&content)
            .iter()
            .map(|a| a.id.unwrap().get())
            .collect();
        assert_eq!(saved, [1, 2, 4, 5]);
        let downloads: Vec<_> = downloadable(&content)
            .into_iter()
            .map(AttachmentId::get)
            .collect();
        assert_eq!(downloads, [3]);
    }

    #[test]
    fn albums_round_their_outer_corners() {
        let corners = |count| {
            (0..count)
                .map(|ix| cell_corners(ix, count))
                .collect::<Vec<_>>()
        };
        let (o, i) = (true, false);

        assert_eq!(corners(2), [[o, i, i, o], [i, o, o, i]]);
        assert_eq!(corners(3), [[o, i, i, i], [i, o, o, i], [i, i, o, o]]);
        assert_eq!(
            corners(4),
            [[o, i, i, i], [i, o, i, i], [i, i, i, o], [i, i, o, i]]
        );
        assert_eq!(corners(1), [ALL_CORNERS]);
    }

    #[test]
    fn sizes_read_for_people() {
        assert_eq!(size_label(512), "512 B");
        assert_eq!(size_label(12 * 1024), "12 KB");
        assert_eq!(size_label(2_400_000), "2.3 MB");
    }

    #[test]
    fn boxes_keep_the_shape_within_bounds() {
        let image = Attachment::new(AttachmentKind::Image);
        assert_eq!(box_size(Some((640, 480)), &image), (320., 240.));
        assert_eq!(box_size(Some((100, 1000)), &image), (24., 240.));
        assert_eq!(box_size(Some((100, 50)), &image), (100., 50.));
        assert_eq!(box_size(None, &image), (320., 240.));
        let sent = Attachment {
            width: Some(1000),
            height: Some(1000),
            ..image
        };
        assert_eq!(box_size(None, &sent), (240., 240.));
        assert_eq!(box_size(Some((2000, 1000)), &sent), (320., 160.));
    }

    #[test]
    fn files_read_with_name_size_and_action() {
        let mut file = attachment(AttachmentKind::File, AttachmentState::Remote, 1);
        file.name = Some("a\u{202e}fdp.exe".into());

        assert_eq!(
            file_line(&file),
            "▤ \u{2068}a<U+202E>fdp.exe\u{2069} · 2.3 MB"
        );
        assert_eq!(file_action(&file), Some("download"));
        file.name = None;
        file.state = AttachmentState::Ready;
        assert_eq!(file_line(&file), "▤ file · 2.3 MB");
        assert_eq!(file_action(&file), Some("save"));
        assert_eq!(
            status_label(&attachment(
                AttachmentKind::Image,
                AttachmentState::Remote,
                1
            ))
            .as_deref(),
            Some("↓ 2.3 MB")
        );
    }
}
