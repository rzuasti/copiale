//! Readies files to send: reads them, and re-encodes still images unless
//! sent as they are, applying their orientation and leaving their metadata
//! (EXIF, XMP, location) behind.

use std::io::{Cursor, Read as _};
use std::path::Path;

use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, ImageDecoder as _, ImageFormat, ImageReader, Limits};

use crate::model::{
    Attachment, AttachmentKind, AttachmentSource, MAX_ATTACHMENT_SIZE, NewAttachment,
};

/// Re-encoded images fit this square, as Signal's standard quality.
const MAX_SIDE: u32 = 2048;
const JPEG_QUALITY: u8 = 85;
/// The blurred preview is made from the image shrunk to fit this square.
const BLURHASH_SOURCE: u32 = 64;
/// Images larger than this aren't decoded.
const MAX_PIXELS_SIDE: u32 = 16_384;
const MAX_DECODE_BYTES: u64 = 512 * 1024 * 1024;

/// A file ready to store and send.
#[derive(Debug)]
pub(crate) struct Prepared {
    pub attachment: Attachment,
    pub contents: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum PrepareError {
    #[error("{0} is larger than 100 MB")]
    TooLarge(String),
    #[error("couldn't read {name}: {error}")]
    Unreadable { name: String, error: std::io::Error },
}

/// Reads `new` and readies it to send. Blocking.
pub(crate) fn prepare(new: NewAttachment) -> Result<Prepared, PrepareError> {
    let (name, contents) = match new.source {
        AttachmentSource::Path(path) => {
            let name = file_name(&path);
            let contents = read(&path).map_err(|error| match error.kind() {
                std::io::ErrorKind::FileTooLarge => PrepareError::TooLarge(name.clone()),
                _ => PrepareError::Unreadable {
                    name: name.clone(),
                    error,
                },
            })?;
            (name, contents)
        }
        AttachmentSource::Pasted { name, bytes } => (name, bytes),
    };
    if contents.len() as u64 > u64::from(MAX_ATTACHMENT_SIZE) {
        return Err(PrepareError::TooLarge(name));
    }
    let format = image::guess_format(&contents).ok();
    let prepared = match format {
        Some(ImageFormat::Jpeg | ImageFormat::Png | ImageFormat::WebP) if !new.original => {
            reencode(&name, &contents)
        }
        Some(
            format @ (ImageFormat::Jpeg | ImageFormat::Png | ImageFormat::WebP | ImageFormat::Gif),
        ) => image_as_is(&name, contents.clone(), format),
        _ => None,
    };
    let prepared = prepared.unwrap_or_else(|| as_file(name, contents));
    if prepared.contents.len() as u64 > u64::from(MAX_ATTACHMENT_SIZE) {
        let name = prepared.attachment.name.unwrap_or_default();
        return Err(PrepareError::TooLarge(name));
    }
    Ok(prepared)
}

/// The file at `path`, refusing more than Signal takes.
fn read(path: &Path) -> std::io::Result<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    let mut contents = Vec::new();
    file.take(u64::from(MAX_ATTACHMENT_SIZE) + 1)
        .read_to_end(&mut contents)?;
    if contents.len() as u64 > u64::from(MAX_ATTACHMENT_SIZE) {
        return Err(std::io::ErrorKind::FileTooLarge.into());
    }
    Ok(contents)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into())
}

/// `contents` decoded upright, within the limits.
fn decode(contents: &[u8]) -> Option<DynamicImage> {
    let mut reader = ImageReader::new(Cursor::new(contents))
        .with_guessed_format()
        .ok()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_PIXELS_SIDE);
    limits.max_image_height = Some(MAX_PIXELS_SIDE);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().ok()?;
    let orientation = decoder.orientation().ok();
    let mut image = DynamicImage::from_decoder(decoder).ok()?;
    if let Some(orientation) = orientation {
        image.apply_orientation(orientation);
    }
    Some(image)
}

/// A still image upright, shrunk to [`MAX_SIDE`], as a JPEG, or a PNG if
/// it's transparent; its metadata isn't carried over.
fn reencode(name: &str, contents: &[u8]) -> Option<Prepared> {
    let mut image = decode(contents)?;
    if image.width() > MAX_SIDE || image.height() > MAX_SIDE {
        image = image.resize(MAX_SIDE, MAX_SIDE, image::imageops::FilterType::Lanczos3);
    }
    let transparent = image.color().has_alpha() && image.to_rgba8().pixels().any(|p| p.0[3] < 255);
    let mut encoded = Vec::new();
    let (extension, content_type) = if transparent {
        image
            .write_to(&mut Cursor::new(&mut encoded), ImageFormat::Png)
            .ok()?;
        ("png", "image/png")
    } else {
        let encoder = JpegEncoder::new_with_quality(&mut encoded, JPEG_QUALITY);
        image.to_rgb8().write_with_encoder(encoder).ok()?;
        ("jpg", "image/jpeg")
    };
    let stem = Path::new(name)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| "image".into());
    Some(Prepared {
        attachment: Attachment {
            content_type: Some(content_type.into()),
            name: Some(format!("{stem}.{extension}")),
            size: Some(encoded.len() as u32),
            width: Some(image.width()),
            height: Some(image.height()),
            blurhash: blurhash(&image),
            ..Attachment::new(AttachmentKind::Image)
        },
        contents: encoded,
    })
}

/// An image sent as it is, described by its decoded size.
fn image_as_is(name: &str, contents: Vec<u8>, format: ImageFormat) -> Option<Prepared> {
    let image = decode(&contents)?;
    let kind = match format {
        ImageFormat::Gif => AttachmentKind::Gif,
        _ => AttachmentKind::Image,
    };
    Some(Prepared {
        attachment: Attachment {
            content_type: Some(format.to_mime_type().into()),
            name: Some(name.into()),
            size: Some(contents.len() as u32),
            width: Some(image.width()),
            height: Some(image.height()),
            blurhash: blurhash(&image),
            ..Attachment::new(kind)
        },
        contents,
    })
}

/// Anything else, typed by its name.
fn as_file(name: String, contents: Vec<u8>) -> Prepared {
    let content_type = mime_guess::from_path(&name).first_or_octet_stream();
    // Images that didn't decode go as files too.
    let kind = match content_type.type_() {
        mime_guess::mime::VIDEO => AttachmentKind::Video,
        mime_guess::mime::AUDIO => AttachmentKind::Audio,
        _ => AttachmentKind::File,
    };
    Prepared {
        attachment: Attachment {
            content_type: Some(content_type.essence_str().into()),
            name: Some(name),
            size: Some(contents.len() as u32),
            ..Attachment::new(kind)
        },
        contents,
    }
}

fn blurhash(image: &DynamicImage) -> Option<String> {
    let small = image.thumbnail(BLURHASH_SOURCE, BLURHASH_SOURCE).to_rgba8();
    blurhash::encode(4, 3, small.width(), small.height(), small.as_raw()).ok()
}

#[cfg(test)]
mod tests {
    use image::{ImageEncoder as _, Rgba, RgbaImage};

    use super::*;

    fn pasted(name: &str, bytes: Vec<u8>, original: bool) -> NewAttachment {
        NewAttachment {
            source: AttachmentSource::Pasted {
                name: name.into(),
                bytes,
            },
            original,
        }
    }

    fn png(width: u32, height: u32, alpha: u8) -> Vec<u8> {
        let image = RgbaImage::from_pixel(width, height, Rgba([200, 30, 30, alpha]));
        let mut bytes = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .unwrap();
        bytes
    }

    /// A JPEG with an EXIF block saying it's rotated a quarter turn, and a
    /// location-like comment.
    fn rotated_jpeg_with_exif() -> Vec<u8> {
        let image = image::RgbImage::from_pixel(40, 20, image::Rgb([30, 30, 200]));
        let mut bytes = Vec::new();
        let mut encoder = JpegEncoder::new_with_quality(&mut bytes, 90);
        // Big-endian TIFF with one entry: Orientation (0x0112) = 6.
        let exif = [
            b'M', b'M', 0, 42, 0, 0, 0, 8, 0, 1, 0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 6, 0, 0, 0, 0, 0,
            0, 0, 0, b'G', b'P', b'S',
        ];
        encoder.set_exif_metadata(exif.to_vec()).unwrap();
        encoder
            .write_image(image.as_raw(), 40, 20, image::ExtendedColorType::Rgb8)
            .unwrap();
        bytes
    }

    #[test]
    fn images_are_reencoded_upright_without_metadata() {
        let original = rotated_jpeg_with_exif();
        assert!(original.windows(4).any(|w| w == b"Exif"));

        let prepared = prepare(pasted("IMG_1.JPEG", original, false)).unwrap();

        let attachment = &prepared.attachment;
        assert_eq!(attachment.kind, AttachmentKind::Image);
        assert_eq!(attachment.content_type.as_deref(), Some("image/jpeg"));
        assert_eq!(attachment.name.as_deref(), Some("IMG_1.jpg"));
        // Turned upright.
        assert_eq!((attachment.width, attachment.height), (Some(20), Some(40)));
        assert_eq!(attachment.size, Some(prepared.contents.len() as u32));
        assert!(attachment.blurhash.is_some());
        assert!(!prepared.contents.windows(4).any(|w| w == b"Exif"));
        assert!(!prepared.contents.windows(3).any(|w| w == b"GPS"));
    }

    #[test]
    fn large_images_shrink_and_transparent_ones_stay_png() {
        let big = prepare(pasted("big.png", png(4096, 1024, 255), false)).unwrap();
        assert_eq!(big.attachment.content_type.as_deref(), Some("image/jpeg"));
        assert_eq!(
            (big.attachment.width, big.attachment.height),
            (Some(2048), Some(512))
        );

        let clear = prepare(pasted("clear.png", png(10, 10, 100), false)).unwrap();
        assert_eq!(clear.attachment.content_type.as_deref(), Some("image/png"));
        assert_eq!(clear.attachment.name.as_deref(), Some("clear.png"));
    }

    #[test]
    fn originals_and_gifs_go_as_they_are() {
        let original = rotated_jpeg_with_exif();
        let prepared = prepare(pasted("IMG_1.JPEG", original.clone(), true)).unwrap();
        assert_eq!(prepared.contents, original);
        assert_eq!(prepared.attachment.name.as_deref(), Some("IMG_1.JPEG"));
        assert_eq!(prepared.attachment.kind, AttachmentKind::Image);

        let mut gif = Vec::new();
        RgbaImage::from_pixel(3, 2, Rgba([0, 0, 0, 255]))
            .write_to(&mut Cursor::new(&mut gif), ImageFormat::Gif)
            .unwrap();
        let prepared = prepare(pasted("cat.gif", gif.clone(), false)).unwrap();
        assert_eq!(prepared.contents, gif);
        assert_eq!(prepared.attachment.kind, AttachmentKind::Gif);
        assert_eq!(
            (prepared.attachment.width, prepared.attachment.height),
            (Some(3), Some(2))
        );
    }

    #[test]
    fn other_files_are_typed_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.pdf");
        std::fs::write(&path, b"%PDF-1.7").unwrap();
        let new = |path: &Path| NewAttachment {
            source: AttachmentSource::Path(path.to_owned()),
            original: false,
        };

        let pdf = prepare(new(&path)).unwrap();
        assert_eq!(pdf.attachment.kind, AttachmentKind::File);
        assert_eq!(
            pdf.attachment.content_type.as_deref(),
            Some("application/pdf")
        );
        assert_eq!(pdf.attachment.name.as_deref(), Some("notes.pdf"));
        assert_eq!(pdf.contents, b"%PDF-1.7");

        let video = prepare(pasted("clip.mp4", b"not really".to_vec(), false)).unwrap();
        assert_eq!(video.attachment.kind, AttachmentKind::Video);
        // Not an image after all: a file.
        let fake = prepare(pasted("fake.png", b"hello".to_vec(), false)).unwrap();
        assert_eq!(fake.attachment.kind, AttachmentKind::File);
        assert_eq!(fake.contents, b"hello");

        assert!(matches!(
            prepare(new(&dir.path().join("missing"))),
            Err(PrepareError::Unreadable { .. })
        ));
    }
}
