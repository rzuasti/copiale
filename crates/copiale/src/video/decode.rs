//! Opens videos with FFmpeg: MP4 and MOV only, with the codecs Signal's
//! clients send; and a video's first frame, for the log's preview.

use std::io::Cursor;

use ffmpeg_next::codec::{self, decoder, threading};
use ffmpeg_next::format::context::{Input, StreamIo};
use ffmpeg_next::format::{self, Pixel};
use ffmpeg_next::frame::{self, side_data};
use ffmpeg_next::software::scaling;
use ffmpeg_next::{Dictionary, media};
use image::{ImageFormat, RgbaImage};

/// Containers FFmpeg may probe.
const FORMATS: &str = "mov,mp4,m4a,3gp,3g2,mj2";
/// Video decoders FFmpeg may open.
pub const VIDEO_CODECS: &str = "h264,hevc,vp9,av1,libdav1d";
/// Widest or tallest video decoded.
const MAX_SIDE: u32 = 8192;
/// Packets read looking for a first frame.
const MAX_PACKETS: usize = 500;
/// The first frame is scaled down to fit this square, as the log's
/// thumbnails.
const FIT: u32 = 720;

#[derive(Debug, PartialEq, Eq)]
pub enum VideoError {
    /// Not a video this decodes.
    Unsupported,
    /// Corrupt, or over the limits.
    Invalid,
}

impl From<ffmpeg_next::Error> for VideoError {
    fn from(_: ffmpeg_next::Error) -> Self {
        Self::Invalid
    }
}

/// Silences FFmpeg's logging; before the sandbox, which allows no files.
pub fn init() {
    ffmpeg_next::util::log::set_level(ffmpeg_next::util::log::Level::Quiet);
}

/// `video` opened, with only `codecs` allowed.
pub fn open(video: Vec<u8>, codecs: &str) -> Result<Input, VideoError> {
    let io = StreamIo::from_read_seek(Cursor::new(video))?;
    let mut options = Dictionary::new();
    options.set("format_whitelist", FORMATS);
    options.set("codec_whitelist", codecs);
    options.set("protocol_whitelist", "");
    format::input_from_stream(io, None, Some(options)).map_err(|_| VideoError::Unsupported)
}

/// The best video stream's index and a decoder for it on `threads`.
pub fn video_decoder(input: &Input, threads: usize) -> Result<(usize, decoder::Video), VideoError> {
    let stream = input
        .streams()
        .best(media::Type::Video)
        .ok_or(VideoError::Unsupported)?;
    let parameters = stream.parameters();
    if !matches!(
        parameters.id(),
        codec::Id::H264 | codec::Id::HEVC | codec::Id::VP9 | codec::Id::AV1
    ) {
        return Err(VideoError::Unsupported);
    }
    let mut context = codec::context::Context::from_parameters(parameters)?;
    context.set_threading(match threads {
        0 | 1 => threading::Config {
            kind: threading::Type::None,
            count: 1,
        },
        count => threading::Config {
            kind: threading::Type::Frame,
            count,
        },
    });
    let decoder = context.decoder().video()?;
    if decoder.width() > MAX_SIDE || decoder.height() > MAX_SIDE {
        return Err(VideoError::Invalid);
    }
    Ok((stream.index(), decoder))
}

/// `video`'s first frame, upright and scaled to fit [`FIT`], as a PNG.
pub fn first_frame(video: Vec<u8>) -> Result<Vec<u8>, VideoError> {
    let mut input = open(video, VIDEO_CODECS)?;
    let (index, mut decoder) = video_decoder(&input, 1)?;
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
    let (width, height) = fit(decoded.width(), decoded.height(), FIT)?;
    let pixels = Scaler::new(Pixel::RGBA).convert(&decoded, width, height)?;
    let image = upright(pixels, rotation(&decoded));
    let mut png = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
        .map_err(|_| VideoError::Invalid)?;
    Ok(png)
}

/// `width`×`height` scaled down to fit a `max` square.
pub fn fit(width: u32, height: u32, max: u32) -> Result<(u32, u32), VideoError> {
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return Err(VideoError::Invalid);
    }
    let scale = (f64::from(max) / f64::from(width.max(height))).min(1.);
    let fit = |side: u32| ((f64::from(side) * scale).round() as u32).max(1);
    Ok((fit(width), fit(height)))
}

/// Converts frames to a 4-byte pixel format at a size, keeping FFmpeg's
/// context while frames stay alike.
pub struct Scaler {
    format: Pixel,
    context: Option<scaling::Context>,
}

impl Scaler {
    pub fn new(format: Pixel) -> Self {
        Self {
            format,
            context: None,
        }
    }

    /// `frame` as `width`×`height` pixels, rows packed.
    pub fn convert(
        &mut self,
        frame: &frame::Video,
        width: u32,
        height: u32,
    ) -> Result<RgbaImage, VideoError> {
        if frame.width() == 0 || frame.height() == 0 {
            return Err(VideoError::Invalid);
        }
        let alike = self.context.as_ref().is_some_and(|context| {
            let input = context.input();
            let output = context.output();
            (input.format, input.width, input.height)
                == (frame.format(), frame.width(), frame.height())
                && (output.width, output.height) == (width, height)
        });
        if !alike {
            self.context = Some(scaling::Context::get(
                frame.format(),
                frame.width(),
                frame.height(),
                self.format,
                width,
                height,
                scaling::Flags::AREA,
            )?);
        }
        let Some(context) = &mut self.context else {
            return Err(VideoError::Invalid);
        };
        let mut scaled = frame::Video::empty();
        context.run(frame, &mut scaled)?;
        let stride = scaled.stride(0);
        let row = width as usize * 4;
        let mut pixels = Vec::with_capacity(row * height as usize);
        for line in scaled.data(0).chunks(stride).take(height as usize) {
            pixels.extend_from_slice(line.get(..row).ok_or(VideoError::Invalid)?);
        }
        RgbaImage::from_raw(width, height, pixels).ok_or(VideoError::Invalid)
    }
}

/// Degrees counterclockwise the frame shows rotated, as phones record it.
pub fn rotation(frame: &frame::Video) -> i32 {
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
pub fn upright(image: RgbaImage, rotation: i32) -> RgbaImage {
    match rotation {
        90 => image::imageops::rotate270(&image),
        180 => image::imageops::rotate180(&image),
        270 => image::imageops::rotate90(&image),
        _ => image,
    }
}

/// `(width, height)` once turned upright from `rotation`.
pub fn upright_size((width, height): (u32, u32), rotation: i32) -> (u32, u32) {
    match rotation {
        90 | 270 => (height, width),
        _ => (width, height),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn fixture(name: &str) -> Vec<u8> {
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
            Err(VideoError::Unsupported)
        );
        assert_eq!(
            first_frame(b"%PDF-1.7 not a video".to_vec()),
            Err(VideoError::Unsupported)
        );
        let mut truncated = fixture("red-64x48.mp4");
        truncated.truncate(200);
        assert!(first_frame(truncated).is_err());
    }

    #[test]
    fn sizes_fit_and_turn() {
        assert_eq!(fit(3840, 2160, 1920), Ok((1920, 1080)));
        assert_eq!(fit(64, 48, 1920), Ok((64, 48)));
        assert_eq!(fit(0, 48, 1920), Err(VideoError::Invalid));
        assert_eq!(upright_size((64, 48), 90), (48, 64));
        assert_eq!(upright_size((64, 48), 180), (64, 48));
    }
}
