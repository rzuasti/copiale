//! Decodes a video's first frame with FFmpeg: MP4 and MOV only, with the
//! codecs Signal's clients send, on one thread.

use std::io::Cursor;

use ffmpeg_next::codec::{self, threading};
use ffmpeg_next::format::context::StreamIo;
use ffmpeg_next::format::{self, Pixel};
use ffmpeg_next::frame::{self, side_data};
use ffmpeg_next::software::scaling;
use ffmpeg_next::{Dictionary, media};
use image::{ImageFormat, RgbaImage};

/// Containers FFmpeg may probe.
const FORMATS: &str = "mov,mp4,m4a,3gp,3g2,mj2";
/// Decoders FFmpeg may open.
const CODECS: &str = "h264,hevc,vp9,av1,libdav1d";
/// Widest or tallest video decoded.
const MAX_SIDE: u32 = 8192;
/// Packets read looking for a first frame.
const MAX_PACKETS: usize = 500;
/// The frame is scaled down to fit this square, as the log's thumbnails.
const FIT: u32 = 720;

#[derive(Debug, PartialEq, Eq)]
pub enum FrameError {
    /// Not a video this decodes.
    Unsupported,
    /// Corrupt, or over the limits.
    Invalid,
}

impl From<ffmpeg_next::Error> for FrameError {
    fn from(_: ffmpeg_next::Error) -> Self {
        Self::Invalid
    }
}

/// Silences FFmpeg's logging; before the sandbox, which allows no files.
pub fn init() {
    ffmpeg_next::util::log::set_level(ffmpeg_next::util::log::Level::Quiet);
}

/// `video`'s first frame, upright and scaled to fit [`FIT`], as a PNG.
pub fn first_frame(video: Vec<u8>) -> Result<Vec<u8>, FrameError> {
    let io = StreamIo::from_read_seek(Cursor::new(video))?;
    let mut options = Dictionary::new();
    options.set("format_whitelist", FORMATS);
    options.set("codec_whitelist", CODECS);
    options.set("protocol_whitelist", "");
    let mut input =
        format::input_from_stream(io, None, Some(options)).map_err(|_| FrameError::Unsupported)?;
    let stream = input
        .streams()
        .best(media::Type::Video)
        .ok_or(FrameError::Unsupported)?;
    let index = stream.index();
    let parameters = stream.parameters();
    if !matches!(
        parameters.id(),
        codec::Id::H264 | codec::Id::HEVC | codec::Id::VP9 | codec::Id::AV1
    ) {
        return Err(FrameError::Unsupported);
    }
    let mut context = codec::context::Context::from_parameters(parameters)?;
    context.set_threading(threading::Config {
        kind: threading::Type::None,
        count: 1,
    });
    let mut decoder = context.decoder().video()?;
    if decoder.width() > MAX_SIDE || decoder.height() > MAX_SIDE {
        return Err(FrameError::Invalid);
    }
    let mut decoded = frame::Video::empty();
    let mut found = false;
    for (stream, packet) in input.packets().take(MAX_PACKETS) {
        if stream.index() != index {
            continue;
        }
        // A bad packet may be followed by good ones.
        if decoder.send_packet(&packet).is_err() {
            continue;
        }
        if decoder.receive_frame(&mut decoded).is_ok() {
            found = true;
            break;
        }
    }
    if !found {
        decoder.send_eof()?;
        decoder.receive_frame(&mut decoded)?;
    }
    let image = upright(rgba(&decoded)?, rotation(&decoded));
    let mut png = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
        .map_err(|_| FrameError::Invalid)?;
    Ok(png)
}

/// `frame` scaled to fit [`FIT`], as RGBA.
fn rgba(frame: &frame::Video) -> Result<RgbaImage, FrameError> {
    let (width, height) = (frame.width(), frame.height());
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return Err(FrameError::Invalid);
    }
    let scale = (f64::from(FIT) / f64::from(width.max(height))).min(1.);
    let fit = |side: u32| ((f64::from(side) * scale).round() as u32).max(1);
    let (to_width, to_height) = (fit(width), fit(height));
    let mut scaler = scaling::Context::get(
        frame.format(),
        width,
        height,
        Pixel::RGBA,
        to_width,
        to_height,
        scaling::Flags::AREA,
    )?;
    let mut scaled = frame::Video::empty();
    scaler.run(frame, &mut scaled)?;
    let stride = scaled.stride(0);
    let row = to_width as usize * 4;
    let mut pixels = Vec::with_capacity(row * to_height as usize);
    for line in scaled.data(0).chunks(stride).take(to_height as usize) {
        pixels.extend_from_slice(line.get(..row).ok_or(FrameError::Invalid)?);
    }
    RgbaImage::from_raw(to_width, to_height, pixels).ok_or(FrameError::Invalid)
}

/// Degrees counterclockwise the frame shows rotated, as phones record it.
fn rotation(frame: &frame::Video) -> i32 {
    let Some(matrix) = frame.side_data(side_data::Type::DisplayMatrix) else {
        return 0;
    };
    let data = matrix.data();
    if data.len() < 9 * 4 {
        return 0;
    }
    // SAFETY: a display matrix is nine 32-bit integers.
    let degrees = unsafe { ffmpeg_next::ffi::av_display_rotation_get(data.as_ptr().cast::<i32>()) };
    if degrees.is_nan() {
        return 0;
    }
    (degrees.round() as i32).rem_euclid(360)
}

/// `image` turned upright from `rotation` degrees counterclockwise.
fn upright(image: RgbaImage, rotation: i32) -> RgbaImage {
    match rotation {
        90 => image::imageops::rotate270(&image),
        180 => image::imageops::rotate180(&image),
        270 => image::imageops::rotate90(&image),
        _ => image,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read(path).unwrap()
    }

    fn decoded(name: &str) -> RgbaImage {
        let png = first_frame(fixture(name)).unwrap();
        image::load_from_memory_with_format(&png, ImageFormat::Png)
            .unwrap()
            .into_rgba8()
    }

    fn reddish(image: &RgbaImage) -> bool {
        let [r, g, b, _] = image.get_pixel(image.width() / 2, image.height() / 2).0;
        r > 200 && g < 60 && b < 60
    }

    #[test]
    fn h264_and_hevc_give_their_first_frame() {
        let red = decoded("red-64x48.mp4");
        assert_eq!(red.dimensions(), (64, 48));
        assert!(reddish(&red));

        let green = decoded("green-hevc.mp4");
        assert_eq!(green.dimensions(), (64, 48));
        let [r, g, _, _] = green.get_pixel(32, 24).0;
        assert!(g > 100 && r < 60, "{:?}", green.get_pixel(32, 24));
    }

    #[test]
    fn rotated_videos_come_out_upright() {
        assert_eq!(decoded("red-rotated.mp4").dimensions(), (48, 64));
    }

    #[test]
    fn other_codecs_and_files_are_refused() {
        assert_eq!(
            first_frame(fixture("red-mpeg4.mp4")),
            Err(FrameError::Unsupported)
        );
        assert_eq!(
            first_frame(b"%PDF-1.7 not a video".to_vec()),
            Err(FrameError::Unsupported)
        );
        let mut truncated = fixture("red-64x48.mp4");
        truncated.truncate(200);
        assert!(first_frame(truncated).is_err());
    }
}
