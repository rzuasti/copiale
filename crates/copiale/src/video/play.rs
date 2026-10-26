//! `copiale --video-play`: decodes a video for the player
//! (`copiale_core::video::Player`), frames and sound as fast as it reads
//! them, seeking when asked; see `copiale_core::video::protocol`.

use std::io::Write;
use std::sync::{Arc, Condvar, Mutex, PoisonError};

use copiale_core::video::protocol::{self, AudioFormat, Control, Message, Start};
use ffmpeg_next::format::context::Input;
use ffmpeg_next::format::{Pixel, Sample, sample};
use ffmpeg_next::software::resampling;
use ffmpeg_next::{ChannelLayout, Packet, Rational, Rescale as _, codec, decoder, frame, media};

use super::decode::{self, Scaler, VideoError};

/// Audio decoders FFmpeg may open, besides the video ones.
const AUDIO_CODECS: &str = "aac";
/// Decoding threads at most.
const MAX_THREADS: usize = 4;
/// Sound kept while the first frame's size isn't known.
const MAX_PENDING: usize = 512;
/// Microseconds, FFmpeg's `AV_TIME_BASE`.
const MICROS: Rational = Rational(1, 1_000_000);

/// What the player asked for since last looked at.
#[derive(Default)]
pub struct Controls {
    seek: Option<(u64, u32)>,
    /// The player closed its end: stop.
    closed: bool,
}

pub type SharedControls = Arc<(Mutex<Controls>, Condvar)>;

/// Takes the player's controls from `input` until it closes.
pub fn read_controls(input: &mut impl std::io::Read, controls: &SharedControls) {
    let (lock, changed) = &**controls;
    loop {
        let control = protocol::read_control(input);
        let mut state = lock.lock().unwrap_or_else(PoisonError::into_inner);
        match control {
            Ok(Some(Control::Seek {
                position,
                generation,
            })) => state.seek = Some((position, generation)),
            Ok(None) | Err(_) => state.closed = true,
        }
        changed.notify_all();
        if state.closed {
            return;
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum PlayError {
    /// Not a video this plays.
    Video,
    /// The player is gone.
    Closed,
}

impl From<VideoError> for PlayError {
    fn from(_: VideoError) -> Self {
        Self::Video
    }
}

impl From<ffmpeg_next::Error> for PlayError {
    fn from(_: ffmpeg_next::Error) -> Self {
        Self::Video
    }
}

impl From<std::io::Error> for PlayError {
    fn from(_: std::io::Error) -> Self {
        Self::Closed
    }
}

/// Plays `video` to `out` until the player closes.
pub fn play(
    start: Start,
    video: Vec<u8>,
    controls: &SharedControls,
    out: &mut impl Write,
) -> Result<(), PlayError> {
    let codecs = format!("{},{AUDIO_CODECS}", decode::VIDEO_CODECS);
    let mut input = decode::open(video, &codecs)?;
    let threads = std::thread::available_parallelism()
        .map_or(1, |count| count.get())
        .min(MAX_THREADS);
    let (video_index, video) = decode::video_decoder(&input, threads)?;
    let video_base = time_base(&input, video_index);
    let audio = start.audio.and_then(|format| Sound::open(&input, format));
    // SAFETY: an open input's context.
    let begins = match unsafe { (*input.as_ptr()).start_time } {
        ffmpeg_next::ffi::AV_NOPTS_VALUE => 0,
        start => start,
    };
    let duration = u64::try_from(input.duration()).ok().filter(|d| *d > 0);
    let mut playback = Playback {
        out,
        begins,
        video,
        video_index,
        video_base,
        audio,
        duration,
        size: None,
        scaler: Scaler::new(Pixel::BGRA),
        pending: Vec::new(),
        generation: 0,
        from: 0,
    };
    playback.run(&mut input, controls)
}

struct Playback<'a, W> {
    out: &'a mut W,
    /// When the video starts, in microseconds.
    begins: i64,
    video: decoder::Video,
    video_index: usize,
    video_base: Rational,
    audio: Option<Sound>,
    duration: Option<u64>,
    /// The frames' size and rotation, from the first frame.
    size: Option<Size>,
    scaler: Scaler,
    /// Sound before the first frame, which [`Message::Info`] waits for.
    pending: Vec<Message>,
    generation: u32,
    /// What comes before this, in microseconds, is skipped: a seek's
    /// target.
    from: u64,
}

#[derive(Clone, Copy)]
struct Size {
    /// Scaled, before turning.
    scaled: (u32, u32),
    rotation: i32,
}

impl<W: Write> Playback<'_, W> {
    fn run(&mut self, input: &mut Input, controls: &SharedControls) -> Result<(), PlayError> {
        let mut ended = false;
        let mut packet = Packet::empty();
        loop {
            match next_control(controls, ended) {
                Next::Close => return Ok(()),
                Next::Seek(position, generation) => {
                    self.seek(input, position, generation);
                    ended = false;
                }
                Next::Go => {}
            }
            match packet.read(input) {
                Ok(()) => self.decode(&packet)?,
                // Corrupt data ends it too.
                Err(_) => {
                    self.finish()?;
                    ended = true;
                }
            }
        }
    }

    fn seek(&mut self, input: &mut Input, position: u64, generation: u32) {
        let target = self.begins.saturating_add(position as i64);
        // Lands on a keyframe before; what's before `from` is skipped.
        input.seek(target, ..target).ok();
        self.video.flush();
        if let Some(sound) = &mut self.audio {
            sound.decoder.flush();
            sound.resampler = None;
        }
        self.generation = generation;
        self.from = position;
    }

    fn decode(&mut self, packet: &Packet) -> Result<(), PlayError> {
        let index = packet.stream();
        if index == self.video_index {
            // A bad packet may be followed by good ones.
            if self.video.send_packet(packet).is_ok() {
                self.drain_video()?;
            }
        } else if self
            .audio
            .as_ref()
            .is_some_and(|sound| sound.index == index)
            && let Some(sound) = &mut self.audio
            && sound.decoder.send_packet(packet).is_ok()
        {
            self.drain_audio()?;
        }
        Ok(())
    }

    /// Plays out what the decoders hold, then marks the end.
    fn finish(&mut self) -> Result<(), PlayError> {
        if self.video.send_eof().is_ok() {
            self.drain_video()?;
        }
        if let Some(sound) = &mut self.audio
            && sound.decoder.send_eof().is_ok()
        {
            self.drain_audio()?;
        }
        if self.size.is_none() {
            // Not one frame: nothing to play.
            return Err(VideoError::Invalid.into());
        }
        self.send(&Message::End {
            generation: self.generation,
        })
    }

    fn drain_video(&mut self) -> Result<(), PlayError> {
        let mut decoded = frame::Video::empty();
        while self.video.receive_frame(&mut decoded).is_ok() {
            let Some(pts) = self.micros(decoded.timestamp(), self.video_base) else {
                continue;
            };
            if pts < self.from {
                continue;
            }
            let size = match self.size {
                Some(size) => size,
                None => self.start(&decoded)?,
            };
            let (width, height) = size.scaled;
            let pixels = self.scaler.convert(&decoded, width, height)?;
            let pixels = decode::upright(pixels, size.rotation).into_raw();
            self.send(&Message::Video {
                generation: self.generation,
                pts,
                pixels,
            })?;
        }
        Ok(())
    }

    /// Sends [`Message::Info`] from the first frame, then the sound held.
    fn start(&mut self, first: &frame::Video) -> Result<Size, PlayError> {
        let scaled = decode::fit(first.width(), first.height(), protocol::MAX_SIDE)?;
        let size = Size {
            scaled,
            rotation: decode::rotation(first),
        };
        let (width, height) = decode::upright_size(scaled, size.rotation);
        self.size = Some(size);
        self.send(&Message::Info {
            width,
            height,
            duration: self.duration,
            audio: self.audio.is_some(),
        })?;
        for message in std::mem::take(&mut self.pending) {
            self.send(&message)?;
        }
        Ok(size)
    }

    fn drain_audio(&mut self) -> Result<(), PlayError> {
        let mut decoded = frame::Audio::empty();
        loop {
            let Some(sound) = &mut self.audio else {
                return Ok(());
            };
            if sound.decoder.receive_frame(&mut decoded).is_err() {
                return Ok(());
            }
            let base = sound.base;
            let Some(pts) = self.micros(decoded.timestamp(), base) else {
                continue;
            };
            let length = decoded.samples() as u64 * 1_000_000 / u64::from(decoded.rate().max(1));
            if pts + length < self.from {
                continue;
            }
            let Some(sound) = &mut self.audio else {
                return Ok(());
            };
            let Ok(samples) = sound.resample(&decoded) else {
                continue;
            };
            if samples.is_empty() {
                continue;
            }
            let message = Message::Audio {
                generation: self.generation,
                pts,
                samples,
            };
            if self.size.is_some() {
                self.send(&message)?;
            } else if self.pending.len() < MAX_PENDING {
                self.pending.push(message);
            }
        }
    }

    /// `timestamp` in `base` as microseconds into the video.
    fn micros(&self, timestamp: Option<i64>, base: Rational) -> Option<u64> {
        let micros = timestamp?.rescale(base, MICROS);
        Some(micros.saturating_sub(self.begins).max(0) as u64)
    }

    fn send(&mut self, message: &Message) -> Result<(), PlayError> {
        protocol::write_message(self.out, message)?;
        self.out.flush()?;
        Ok(())
    }
}

enum Next {
    Go,
    Seek(u64, u32),
    Close,
}

/// What to do next; once `ended`, waits for a seek or the close.
fn next_control(controls: &SharedControls, ended: bool) -> Next {
    let (lock, changed) = &**controls;
    let mut state = lock.lock().unwrap_or_else(PoisonError::into_inner);
    loop {
        if state.closed {
            return Next::Close;
        }
        if let Some((position, generation)) = state.seek.take() {
            return Next::Seek(position, generation);
        }
        if !ended {
            return Next::Go;
        }
        state = changed.wait(state).unwrap_or_else(PoisonError::into_inner);
    }
}

fn time_base(input: &Input, index: usize) -> Rational {
    input
        .stream(index)
        .map_or(MICROS, |stream| stream.time_base())
}

/// The sound: AAC, resampled to the player's format.
struct Sound {
    index: usize,
    base: Rational,
    decoder: decoder::Audio,
    format: AudioFormat,
    resampler: Option<resampling::Context>,
}

impl Sound {
    fn open(input: &Input, format: AudioFormat) -> Option<Self> {
        let stream = input.streams().best(media::Type::Audio)?;
        let parameters = stream.parameters();
        if parameters.id() != codec::Id::AAC {
            return None;
        }
        let decoder = codec::context::Context::from_parameters(parameters)
            .ok()?
            .decoder()
            .audio()
            .ok()?;
        Some(Self {
            index: stream.index(),
            base: stream.time_base(),
            decoder,
            format,
            resampler: None,
        })
    }

    /// `decoded` as interleaved `f32` samples in the player's format.
    fn resample(&mut self, decoded: &frame::Audio) -> Result<Vec<f32>, VideoError> {
        let layout = match decoded.channel_layout() {
            layout if layout.channels() > 0 => layout,
            _ => ChannelLayout::default(i32::from(decoded.channels())),
        };
        let alike = self.resampler.as_ref().is_some_and(|resampler| {
            let input = resampler.input();
            (input.format, input.channel_layout, input.rate)
                == (decoded.format(), layout, decoded.rate())
        });
        if !alike {
            self.resampler = Some(resampling::Context::get(
                decoded.format(),
                layout,
                decoded.rate(),
                Sample::F32(sample::Type::Packed),
                ChannelLayout::default(i32::from(self.format.channels)),
                self.format.rate,
            )?);
        }
        let Some(resampler) = &mut self.resampler else {
            return Err(VideoError::Invalid);
        };
        let channels = usize::from(self.format.channels);
        let samples = i32::try_from(decoded.samples()).map_err(|_| VideoError::Invalid)?;
        // SAFETY: an initialised resampler, a buffer for as many samples as
        // it says it may give, and the frame's own planes.
        unsafe {
            let context = resampler.as_mut_ptr();
            let room = ffmpeg_next::ffi::swr_get_out_samples(context, samples);
            if room < 0 {
                return Err(VideoError::Invalid);
            }
            let mut buffer = vec![0f32; room as usize * channels];
            let mut planes = [buffer.as_mut_ptr().cast::<u8>()];
            let converted = ffmpeg_next::ffi::swr_convert(
                context,
                planes.as_mut_ptr(),
                room,
                (*decoded.as_ptr()).extended_data as *const *const u8,
                samples,
            );
            if converted < 0 {
                return Err(VideoError::Invalid);
            }
            buffer.truncate(converted as usize * channels);
            Ok(buffer)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use copiale_core::video::protocol::Reader;

    fn fixture(name: &str) -> Vec<u8> {
        let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read(path).unwrap()
    }

    const STEREO: AudioFormat = AudioFormat {
        rate: 48_000,
        channels: 2,
    };

    /// Plays `name` with `seek` asked first, if any, to the end; the
    /// messages sent.
    fn messages(name: &str, audio: Option<AudioFormat>, seek: Option<u64>) -> Vec<Message> {
        let controls = SharedControls::default();
        {
            let mut state = controls.0.lock().unwrap();
            state.seek = seek.map(|position| (position, 1));
        }
        // Closes once the end is sent.
        let mut out = UntilEnd::default();
        let closing = controls.clone();
        out.closing = Some(closing);
        play(Start { audio }, fixture(name), &controls, &mut out).unwrap();
        let mut reader = Reader::new(out.bytes.as_slice());
        let mut messages = Vec::new();
        while let Some(message) = reader.message().unwrap() {
            messages.push(message);
        }
        messages
    }

    /// Keeps what's written; closes the controls after an end.
    #[derive(Default)]
    struct UntilEnd {
        bytes: Vec<u8>,
        closing: Option<SharedControls>,
    }

    impl Write for UntilEnd {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            // An end's tag and generation, written in one go.
            if buf.len() == 1
                && buf[0] == 4
                && let Some(controls) = &self.closing
            {
                controls.0.lock().unwrap().closed = true;
            }
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn frames(messages: &[Message]) -> Vec<(u64, [u8; 4])> {
        messages
            .iter()
            .filter_map(|message| match message {
                Message::Video { pts, pixels, .. } => {
                    let middle = pixels.len() / 2 / 4 * 4;
                    Some((*pts, pixels[middle..middle + 4].try_into().unwrap()))
                }
                _ => None,
            })
            .collect()
    }

    fn sound(messages: &[Message]) -> Vec<f32> {
        messages
            .iter()
            .flat_map(|message| match message {
                Message::Audio { samples, .. } => samples.clone(),
                _ => Vec::new(),
            })
            .collect()
    }

    #[test]
    fn plays_frames_and_sound_to_the_end() {
        let messages = messages("red-blue-aac.mp4", Some(STEREO), None);

        let Message::Info {
            width,
            height,
            duration,
            audio,
        } = messages[0]
        else {
            panic!("{:?}", messages[0]);
        };
        assert_eq!((width, height), (64, 48));
        assert!(audio);
        let duration = duration.unwrap();
        assert!((1_900_000..=2_100_000).contains(&duration), "{duration}");
        assert!(matches!(
            messages.last(),
            Some(Message::End { generation: 0 })
        ));

        let frames = frames(&messages);
        assert_eq!(frames.len(), 60);
        assert!(frames.windows(2).all(|w| w[0].0 < w[1].0));
        // BGRA: red, then blue after a second.
        let [b, _, r, _] = frames[0].1;
        assert!(r > 200 && b < 60, "{:?}", frames[0]);
        let [b, _, r, _] = frames[45].1;
        assert!(b > 200 && r < 60, "{:?}", frames[45]);

        // Two seconds of a tone, both channels.
        let sound = sound(&messages);
        let seconds = sound.len() as f64 / 2. / 48_000.;
        assert!((1.9..=2.1).contains(&seconds), "{seconds}");
        assert!(sound.iter().any(|s| s.abs() > 0.1));
    }

    #[test]
    fn seeking_skips_to_the_position() {
        let messages = messages("red-blue-aac.mp4", Some(STEREO), Some(1_500_000));

        let frames = frames(&messages);
        assert!(frames[0].0 >= 1_500_000, "{:?}", frames[0]);
        assert!((14..=16).contains(&frames.len()), "{}", frames.len());
        let [b, _, _, _] = frames[0].1;
        assert!(b > 200);
        assert!(matches!(
            messages.last(),
            Some(Message::End { generation: 1 })
        ));
        let seconds = sound(&messages).len() as f64 / 2. / 48_000.;
        assert!((0.45..=0.6).contains(&seconds), "{seconds}");
    }

    #[test]
    fn without_sound_asked_none_comes() {
        let messages = messages("red-blue-aac.mp4", None, None);

        assert!(matches!(messages[0], Message::Info { audio: false, .. }));
        assert!(sound(&messages).is_empty());
        assert_eq!(frames(&messages).len(), 60);
    }

    #[test]
    fn rotated_videos_play_upright() {
        let messages = messages("red-rotated.mp4", Some(STEREO), None);

        assert!(matches!(
            messages[0],
            Message::Info {
                width: 48,
                height: 64,
                audio: false,
                ..
            }
        ));
        let Some(Message::Video { pixels, .. }) = messages.get(1) else {
            panic!("{:?}", messages.get(1));
        };
        assert_eq!(pixels.len(), 48 * 64 * 4);
    }

    #[test]
    fn other_codecs_are_refused() {
        let controls = SharedControls::default();
        let mut out = Vec::new();
        let played = play(
            Start { audio: None },
            fixture("red-mpeg4.mp4"),
            &controls,
            &mut out,
        );
        assert_eq!(played, Err(PlayError::Video));
        assert!(out.is_empty());
    }
}
