//! Plays a video through the playback helper (`copiale --video-play`):
//! frames and sound come decoded over its pipe; this buffers them, plays
//! the sound and keeps the clock the frames follow.
//!
//! The sound card's clock drives playback; without sound, the wall clock.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use super::protocol::{self, AudioFormat, Control, Message, Reader, Start};
use super::{Child, VideoHelper};

/// Frames kept ahead, in bytes; at least [`MIN_FRAMES`].
const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;
const MIN_FRAMES: usize = 2;
/// Sound kept ahead.
const MAX_AUDIO: Duration = Duration::from_secs(8);
/// How far sound may drift from the clock before it's dropped or waited
/// for, in microseconds.
const DRIFT: f64 = 40_000.;

/// A frame to show, in BGRA.
#[derive(Debug, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Where playback is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status {
    pub position: Duration,
    /// `None` until known.
    pub duration: Option<Duration>,
    pub playing: bool,
    pub muted: bool,
    /// Played to the end.
    pub ended: bool,
    /// The helper gave nothing to play.
    pub failed: bool,
    /// A first frame, the start's or a seek's, is on its way.
    pub waiting: bool,
}

pub struct Player {
    shared: Arc<Shared>,
    controls: Option<Sender<Control>>,
    child: Option<Child>,
    /// Kept to play; `None` without a sound card.
    _output: Option<cpal::Stream>,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    /// Signalled when frames or sound are taken, or on a seek.
    room: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[derive(Default)]
struct State {
    width: u32,
    height: u32,
    duration: Option<u64>,
    /// The sound's format, if it plays.
    audio: Option<AudioFormat>,
    failed: bool,
    /// The helper is gone.
    closed: bool,
    /// The last seek; older frames and sound are dropped.
    generation: u32,
    frames: VecDeque<(u64, Vec<u8>)>,
    frame_bytes: usize,
    /// A frame of this generation has been shown.
    shown: bool,
    samples: VecDeque<f32>,
    /// When the first queued sample plays, in microseconds.
    samples_pts: f64,
    /// The helper sent all of this generation.
    complete: bool,
    ended: bool,
    playing: bool,
    muted: bool,
    /// The clock: the position when it last started or stopped, plus the
    /// sample frames played since, or the time since without sound.
    base: u64,
    played: u64,
    since: Option<Instant>,
}

impl State {
    /// Microseconds into the video.
    fn position(&self, now: Instant) -> u64 {
        let elapsed = match (self.audio, self.since) {
            (Some(format), _) => self.played * 1_000_000 / u64::from(format.rate),
            (None, Some(since)) => now.saturating_duration_since(since).as_micros() as u64,
            (None, None) => 0,
        };
        let position = self.base + elapsed;
        self.duration.map_or(position, |d| position.min(d))
    }

    /// Whether the clock runs: playing, with this generation's frames
    /// started.
    fn running(&self) -> bool {
        self.playing && self.shown && !self.ended
    }

    /// Stops the clock where it is.
    fn hold(&mut self, now: Instant) {
        self.base = self.position(now);
        self.played = 0;
        self.since = None;
    }

    fn sound_full(&self) -> bool {
        self.audio.is_some_and(|format| {
            let max = MAX_AUDIO.as_secs() as usize * format.rate as usize;
            self.samples.len() >= max * usize::from(format.channels)
        })
    }

    fn frames_full(&self) -> bool {
        self.frames.len() >= MIN_FRAMES && self.frame_bytes >= MAX_FRAME_BYTES
    }
}

impl Player {
    /// Plays `video` through `helper`, with `sound` on the default output
    /// if there is one; starts playing once frames arrive.
    pub fn start(helper: &VideoHelper, video: Vec<u8>, sound: bool) -> std::io::Result<Self> {
        let shared = Arc::new(Shared::default());
        let output = match sound.then(|| open_output(&shared)) {
            Some(Ok(output)) => Some(output),
            Some(Err(error)) => {
                tracing::info!(%error, "videos play without sound");
                None
            }
            None => None,
        };
        let audio = output.as_ref().map(|(_, format)| *format);
        let (child, stdin, stdout) = helper.spawn_player()?;
        let mut player = Self::connect(shared, stdout, stdin, audio, video);
        player.child = Some(child);
        player._output = output.map(|(stream, _)| stream);
        Ok(player)
    }

    /// Plays from the helper's pipes; sound in `audio`'s format, played by
    /// whoever fills from `shared`.
    fn connect(
        shared: Arc<Shared>,
        output: impl Read + Send + 'static,
        input: impl Write + Send + 'static,
        audio: Option<AudioFormat>,
        video: Vec<u8>,
    ) -> Self {
        {
            let mut state = shared.lock();
            state.audio = audio;
            state.playing = true;
        }
        let (controls, received) = channel();
        let mut input = std::io::BufWriter::new(input);
        std::thread::spawn(move || {
            let start = Start { audio };
            if protocol::write_start(&mut input, start, &video).is_err() {
                return;
            }
            drop(video);
            if input.flush().is_err() {
                return;
            }
            // Ends when the player is dropped, closing the helper's input.
            for control in received {
                if protocol::write_control(&mut input, control).is_err() {
                    return;
                }
            }
        });
        let reading = shared.clone();
        std::thread::spawn(move || read(&reading, Reader::new(std::io::BufReader::new(output))));
        Self {
            shared,
            controls: Some(controls),
            child: None,
            _output: None,
        }
    }

    /// The frame due at `now`, if it's new.
    pub fn frame(&mut self, now: Instant) -> Option<Frame> {
        let mut state = self.shared.lock();
        let position = state.position(now);
        let mut due = None;
        while state
            .frames
            .front()
            .is_some_and(|(pts, _)| *pts <= position)
        {
            due = state.frames.pop_front();
        }
        // A seek's or the start's first frame shows at once.
        if due.is_none() && !state.shown {
            due = state.frames.pop_front();
        }
        if let Some((_, pixels)) = &due {
            state.frame_bytes -= pixels.len();
            if !state.shown {
                state.shown = true;
                if state.audio.is_none() && state.playing {
                    state.since = Some(now);
                }
            }
            self.shared.room.notify_all();
        }
        let drained = state.frames.is_empty() && state.samples.is_empty();
        let over = state.closed || state.duration.is_none_or(|d| position >= d);
        if state.complete && drained && over && !state.ended && state.shown {
            state.hold(now);
            state.ended = true;
            state.playing = false;
        }
        due.map(|(_, pixels)| Frame {
            width: state.width,
            height: state.height,
            pixels,
        })
    }

    pub fn status(&self, now: Instant) -> Status {
        let state = self.shared.lock();
        Status {
            position: Duration::from_micros(state.position(now)),
            duration: state.duration.map(Duration::from_micros),
            playing: state.playing,
            muted: state.muted,
            ended: state.ended,
            failed: state.failed,
            waiting: !state.shown && !state.failed && !(state.closed && state.frames.is_empty()),
        }
    }

    /// Plays, from the start once ended; or pauses.
    pub fn toggle(&mut self, now: Instant) {
        let (playing, ended) = {
            let state = self.shared.lock();
            (state.playing, state.ended)
        };
        if ended {
            self.seek(Duration::ZERO);
        }
        let mut state = self.shared.lock();
        state.hold(now);
        state.playing = !playing || ended;
        if state.playing && state.shown && state.audio.is_none() {
            state.since = Some(now);
        }
    }

    /// Goes on from `position`, clamped to the video.
    pub fn seek(&mut self, position: Duration) {
        let mut state = self.shared.lock();
        let mut position = position.as_micros() as u64;
        if let Some(duration) = state.duration {
            position = position.min(duration);
        }
        if state.ended {
            state.playing = true;
        }
        state.generation = state.generation.wrapping_add(1);
        state.frames.clear();
        state.frame_bytes = 0;
        state.samples.clear();
        state.shown = false;
        state.complete = false;
        state.ended = false;
        state.base = position;
        state.played = 0;
        state.since = None;
        let generation = state.generation;
        drop(state);
        self.shared.room.notify_all();
        if let Some(controls) = &self.controls {
            controls
                .send(Control::Seek {
                    position,
                    generation,
                })
                .ok();
        }
    }

    /// Moves `by` from where it is, within the video.
    pub fn skip(&mut self, by: i64, now: Instant) {
        let position = self.status(now).position;
        let moved = if by < 0 {
            position.saturating_sub(Duration::from_secs(by.unsigned_abs()))
        } else {
            position + Duration::from_secs(by.unsigned_abs())
        };
        self.seek(moved);
    }

    pub fn set_muted(&mut self, muted: bool) {
        self.shared.lock().muted = muted;
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // Closes the helper's input: it stops.
        self.controls = None;
        self.shared.lock().closed = true;
        self.shared.room.notify_all();
        if let Some(child) = self.child.take() {
            super::stop(child);
        }
    }
}

/// Takes the helper's messages until it's done or the player is dropped.
fn read(shared: &Shared, mut reader: Reader<impl Read>) {
    while let Ok(Some(message)) = reader.message() {
        let mut state = shared.lock();
        match message {
            Message::Info {
                width,
                height,
                duration,
                audio,
            } => {
                state.width = width;
                state.height = height;
                state.duration = duration;
                if !audio {
                    // The sound card's clock still drives the frames.
                    state.samples.clear();
                }
            }
            Message::Video {
                generation,
                pts,
                pixels,
            } => {
                while generation == state.generation && state.frames_full() && !state.closed {
                    state = shared
                        .room
                        .wait(state)
                        .unwrap_or_else(PoisonError::into_inner);
                }
                if generation == state.generation && !state.closed {
                    state.frame_bytes += pixels.len();
                    state.frames.push_back((pts, pixels));
                }
            }
            Message::Audio {
                generation,
                pts,
                samples,
            } => {
                while generation == state.generation && state.sound_full() && !state.closed {
                    state = shared
                        .room
                        .wait(state)
                        .unwrap_or_else(PoisonError::into_inner);
                }
                if generation == state.generation && !state.closed && state.audio.is_some() {
                    if state.samples.is_empty() {
                        state.samples_pts = pts as f64;
                    }
                    state.samples.extend(samples);
                }
            }
            Message::End { generation } => {
                if generation == state.generation {
                    state.complete = true;
                }
            }
        }
        if state.closed {
            return;
        }
    }
    let mut state = shared.lock();
    if state.width == 0 {
        state.failed = true;
        state.playing = false;
    }
    // Plays out what came.
    state.complete = true;
    state.closed = true;
}

/// Fills `out` with the sound due, `channels` interleaved at `rate`,
/// advancing the clock while it runs.
fn fill(shared: &Shared, out: &mut [f32]) {
    let mut state = shared.lock();
    let Some(format) = state.audio else {
        out.fill(0.);
        return;
    };
    if !state.running() {
        out.fill(0.);
        return;
    }
    let channels = usize::from(format.channels);
    let step = 1_000_000. / f64::from(format.rate);
    let mut taken = false;
    for frame in out.chunks_mut(channels) {
        let clock = state.base as f64 + state.played as f64 * step;
        // Late sound is dropped; early sound waits.
        while !state.samples.is_empty() && state.samples_pts + DRIFT < clock {
            let dropped = channels.min(state.samples.len());
            state.samples.drain(..dropped);
            state.samples_pts += step;
            taken = true;
        }
        if state.samples.len() >= channels && state.samples_pts <= clock + DRIFT {
            for sample in frame.iter_mut() {
                let value = state.samples.pop_front().unwrap_or(0.);
                *sample = if state.muted { 0. } else { value };
            }
            state.samples_pts += step;
            taken = true;
        } else {
            frame.fill(0.);
        }
        state.played += 1;
    }
    drop(state);
    if taken {
        shared.room.notify_all();
    }
}

/// A stream on the default output filling from `shared`, and the format
/// it plays.
fn open_output(shared: &Arc<Shared>) -> Result<(cpal::Stream, AudioFormat), String> {
    use cpal::traits::{DeviceTrait as _, HostTrait as _, StreamTrait as _};

    let device = cpal::default_host()
        .default_output_device()
        .ok_or("no sound output")?;
    let config = device
        .default_output_config()
        .map_err(|error| error.to_string())?;
    let format = AudioFormat {
        rate: config.sample_rate(),
        channels: config.channels(),
    };
    if !protocol::RATES.contains(&format.rate)
        || !(1..=protocol::MAX_CHANNELS).contains(&format.channels)
    {
        return Err(format!("unsupported output {format:?}"));
    }
    let sample_format = config.sample_format();
    let config: cpal::StreamConfig = config.into();
    let stream = match sample_format {
        cpal::SampleFormat::F32 => build::<f32>(&device, &config, shared),
        cpal::SampleFormat::I16 => build::<i16>(&device, &config, shared),
        cpal::SampleFormat::U16 => build::<u16>(&device, &config, shared),
        cpal::SampleFormat::I32 => build::<i32>(&device, &config, shared),
        other => return Err(format!("unsupported sample format {other}")),
    }
    .map_err(|error| error.to_string())?;
    stream.play().map_err(|error| error.to_string())?;
    Ok((stream, format))
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    shared: &Arc<Shared>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    use cpal::traits::DeviceTrait as _;

    let shared = shared.clone();
    let mut buffer = Vec::new();
    device.build_output_stream(
        *config,
        move |out: &mut [T], _| {
            buffer.resize(out.len(), 0.);
            fill(&shared, &mut buffer);
            for (out, sample) in out.iter_mut().zip(&buffer) {
                *out = T::from_sample(*sample);
            }
        },
        |error| tracing::warn!(%error, "sound output failed"),
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000;

    /// A player on in-memory pipes; the test plays the helper.
    struct Fake {
        player: Player,
        helper_out: std::io::PipeWriter,
        helper_in: std::io::PipeReader,
        start: Instant,
    }

    impl Fake {
        fn new(audio: Option<AudioFormat>) -> Self {
            let (from_helper, helper_out) = std::io::pipe().unwrap();
            let (helper_in, to_helper) = std::io::pipe().unwrap();
            let player = Player::connect(
                Arc::new(Shared::default()),
                from_helper,
                to_helper,
                audio,
                b"video".to_vec(),
            );
            Self {
                player,
                helper_out,
                helper_in,
                start: Instant::now(),
            }
        }

        fn send(&mut self, message: Message) {
            protocol::write_message(&mut self.helper_out, &message).unwrap();
        }

        fn info(&mut self, duration: u64, audio: bool) {
            self.send(Message::Info {
                width: 1,
                height: 1,
                duration: Some(duration),
                audio,
            });
        }

        /// A 1×1 frame of `shade`.
        fn frame(&mut self, generation: u32, pts: u64, shade: u8) {
            self.send(Message::Video {
                generation,
                pts,
                pixels: vec![shade; 4],
            });
        }

        fn at(&self, ms: u64) -> Instant {
            self.start + Duration::from_millis(ms)
        }

        /// The frame due at `ms`, waiting for the reader thread.
        fn shade_at(&mut self, ms: u64) -> Option<u8> {
            let now = self.at(ms);
            for _ in 0..200 {
                if let Some(frame) = self.player.frame(now) {
                    return Some(frame.pixels[0]);
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            None
        }

        /// Waits until `check` holds for the state.
        fn until(&self, check: impl Fn(&State) -> bool) {
            for _ in 0..500 {
                if check(&self.player.shared.lock()) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            panic!("timed out");
        }

        fn controls(&mut self) -> (Start, Vec<u8>, Vec<Control>) {
            self.player.controls.take();
            let (start, video) = protocol::read_start(&mut self.helper_in).unwrap();
            let mut controls = Vec::new();
            while let Some(control) = protocol::read_control(&mut self.helper_in).unwrap() {
                controls.push(control);
            }
            (start, video, controls)
        }
    }

    #[test]
    fn the_helper_gets_the_video_and_seeks() {
        let silent = Some(AudioFormat {
            rate: 48_000,
            channels: 2,
        });
        let mut fake = Fake::new(silent);
        fake.info(10_000 * MS, true);
        fake.until(|s| s.duration.is_some());
        fake.player.seek(Duration::from_secs(3));
        fake.player.skip(-5, fake.at(0));

        let (start, video, controls) = fake.controls();

        assert_eq!(start.audio, silent);
        assert_eq!(video, b"video");
        assert_eq!(
            controls,
            [
                Control::Seek {
                    position: 3_000 * MS,
                    generation: 1
                },
                Control::Seek {
                    position: 0,
                    generation: 2
                }
            ]
        );
    }

    #[test]
    fn frames_follow_the_clock_from_the_first() {
        let mut fake = Fake::new(None);
        fake.info(100 * MS, false);
        fake.frame(0, 0, 10);
        fake.frame(0, 40 * MS, 20);
        fake.frame(0, 80 * MS, 30);

        // The first frame at once; the clock starts with it.
        assert_eq!(fake.shade_at(1_000), Some(10));
        assert_eq!(fake.player.frame(fake.at(1_020)), None);
        assert_eq!(fake.shade_at(1_045), Some(20));
        // Late: the newest due one, skipping the rest.
        fake.frame(0, 90 * MS, 40);
        fake.until(|s| s.frames.len() == 2);
        assert_eq!(fake.shade_at(1_095), Some(40));
        assert_eq!(
            fake.player.status(fake.at(1_095)).position,
            Duration::from_millis(95)
        );
    }

    #[test]
    fn pausing_holds_the_clock() {
        let mut fake = Fake::new(None);
        fake.info(1_000 * MS, false);
        fake.frame(0, 0, 10);
        fake.frame(0, 100 * MS, 20);
        assert_eq!(fake.shade_at(0), Some(10));

        fake.player.toggle(fake.at(50));
        let status = fake.player.status(fake.at(500));
        assert!(!status.playing);
        assert_eq!(status.position, Duration::from_millis(50));
        assert_eq!(fake.player.frame(fake.at(500)), None);

        fake.player.toggle(fake.at(500));
        assert_eq!(fake.shade_at(550), Some(20));
    }

    #[test]
    fn seeking_drops_older_frames_and_shows_the_new_first() {
        let mut fake = Fake::new(None);
        fake.info(10_000 * MS, false);
        fake.frame(0, 0, 10);
        assert_eq!(fake.shade_at(0), Some(10));

        fake.player.toggle(fake.at(0));
        fake.player.seek(Duration::from_secs(5));
        // Sent before the helper saw the seek.
        fake.frame(0, 40 * MS, 20);
        fake.frame(1, 5_020 * MS, 30);

        // Paused, yet the seek's first frame shows.
        assert!(fake.player.status(fake.at(10)).waiting);
        assert_eq!(fake.shade_at(10), Some(30));
        assert!(!fake.player.status(fake.at(10)).waiting);
        assert_eq!(
            fake.player.status(fake.at(10)).position,
            Duration::from_secs(5)
        );
    }

    #[test]
    fn playback_ends_once_all_is_shown_and_restarts() {
        let mut fake = Fake::new(None);
        fake.info(80 * MS, false);
        fake.frame(0, 0, 10);
        fake.frame(0, 40 * MS, 20);
        fake.send(Message::End { generation: 0 });
        assert_eq!(fake.shade_at(0), Some(10));
        assert_eq!(fake.shade_at(50), Some(20));
        fake.until(|s| s.complete);
        fake.player.frame(fake.at(90));

        let status = fake.player.status(fake.at(200));
        assert!(status.ended && !status.playing);
        assert_eq!(status.position, Duration::from_millis(80));

        fake.player.toggle(fake.at(200));
        let status = fake.player.status(fake.at(200));
        assert!(status.playing && !status.ended);
        assert_eq!(status.position, Duration::ZERO);
    }

    #[test]
    fn a_helper_that_gives_nothing_fails() {
        let mut fake = Fake::new(None);
        let (_, elsewhere) = std::io::pipe().unwrap();
        drop(std::mem::replace(&mut fake.helper_out, elsewhere));
        fake.until(|s| s.closed);

        let status = fake.player.status(fake.at(0));
        assert!(status.failed && !status.playing && !status.waiting);
        assert_eq!(fake.player.frame(fake.at(0)), None);
    }

    #[test]
    fn sound_plays_in_step_with_the_clock() {
        let format = AudioFormat {
            rate: 1_000,
            channels: 1,
        };
        let mut fake = Fake::new(Some(format));
        fake.info(10_000 * MS, true);
        fake.frame(0, 0, 10);
        // Starts 100 ms in: early sound waits.
        fake.send(Message::Audio {
            generation: 0,
            pts: 100 * MS,
            samples: vec![0.5; 100],
        });
        fake.until(|s| s.samples.len() == 100);
        let shared = fake.player.shared.clone();

        // Not running before the first frame shows.
        let mut out = vec![1.; 50];
        fill(&shared, &mut out);
        assert!(out.iter().all(|s| *s == 0.));
        assert_eq!(shared.lock().played, 0);

        assert_eq!(fake.shade_at(0), Some(10));
        let mut out = vec![1.; 100];
        fill(&shared, &mut out);
        // Silence until 60 ms (100 less the drift), then sound.
        assert!(out[..55].iter().all(|s| *s == 0.));
        assert!(out[65..].iter().all(|s| *s == 0.5));
        assert_eq!(
            fake.player.status(fake.at(0)).position,
            Duration::from_millis(100)
        );

        fake.player.set_muted(true);
        let mut out = vec![1.; 10];
        fill(&shared, &mut out);
        assert!(out.iter().all(|s| *s == 0.));
    }

    #[test]
    fn late_sound_is_dropped() {
        let format = AudioFormat {
            rate: 1_000,
            channels: 2,
        };
        let mut fake = Fake::new(Some(format));
        fake.info(10_000 * MS, true);
        fake.frame(0, 0, 10);
        assert_eq!(fake.shade_at(0), Some(10));
        fake.player.seek(Duration::from_millis(500));
        fake.frame(1, 500 * MS, 20);
        // From 300 ms: 200 ms behind the clock.
        let samples = (0..600).map(|i| (i / 2) as f32).collect();
        fake.send(Message::Audio {
            generation: 1,
            pts: 300 * MS,
            samples,
        });
        fake.until(|s| s.samples.len() == 600);
        assert_eq!(fake.shade_at(0), Some(20));

        let mut out = vec![0.; 2];
        fill(&fake.player.shared, &mut out);

        // The sample due at 500 ms, within the drift.
        assert!((160. ..=200.).contains(&out[0]), "{out:?}");
        assert_eq!(out[0], out[1]);
    }
}
