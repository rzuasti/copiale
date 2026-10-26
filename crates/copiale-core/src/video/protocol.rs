//! The playback helper's pipes (`copiale --video-play`), little-endian.
//!
//! In: [`Start`], the video's length and bytes, then [`Control`]s; closing
//! it stops the helper. Out: [`Message::Info`], then frames, sound and
//! ends, each tagged with the seek they follow.

use std::io::{self, Read, Write};

/// Frames fit in this square.
pub const MAX_SIDE: u32 = 1920;
/// Most samples in one [`Message::Audio`], all channels.
pub const MAX_SAMPLES: u32 = 1 << 20;
/// Sample rates and channel counts asked for.
pub const RATES: std::ops::RangeInclusive<u32> = 8_000..=192_000;
pub const MAX_CHANNELS: u16 = 8;
/// Largest video sent, in bytes: Signal's attachment limit.
pub const MAX_VIDEO: u64 = 100 * 1024 * 1024;

/// What the helper is asked for before the video.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Start {
    /// The sound's format; `None` for no sound.
    pub audio: Option<AudioFormat>,
}

/// Interleaved `f32` samples at `rate`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioFormat {
    pub rate: u32,
    pub channels: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    /// Go on from `position` microseconds; what follows carries
    /// `generation`.
    Seek { position: u64, generation: u32 },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    /// First: the frames' size, upright, and the length if known, in
    /// microseconds.
    Info {
        width: u32,
        height: u32,
        duration: Option<u64>,
        audio: bool,
    },
    /// A frame in BGRA, `width * height * 4` bytes, shown at `pts`
    /// microseconds.
    Video {
        generation: u32,
        pts: u64,
        pixels: Vec<u8>,
    },
    /// Samples in [`Start::audio`]'s format, starting at `pts`.
    Audio {
        generation: u32,
        pts: u64,
        samples: Vec<f32>,
    },
    /// No more until the next seek.
    End { generation: u32 },
}

const INFO: u8 = 1;
const VIDEO: u8 = 2;
const AUDIO: u8 = 3;
const END: u8 = 4;
const SEEK: u8 = 1;

pub fn write_start(out: &mut impl Write, start: Start, video: &[u8]) -> io::Result<()> {
    let (rate, channels) = start.audio.map_or((0, 0), |a| (a.rate, a.channels));
    out.write_all(&rate.to_le_bytes())?;
    out.write_all(&channels.to_le_bytes())?;
    out.write_all(&(video.len() as u64).to_le_bytes())?;
    out.write_all(video)
}

/// The start and the video; refuses formats out of range.
pub fn read_start(input: &mut impl Read) -> io::Result<(Start, Vec<u8>)> {
    let rate = read_u32(input)?;
    let channels = read_u16(input)?;
    let audio = match (rate, channels) {
        (0, 0) => None,
        (rate, channels) if RATES.contains(&rate) && (1..=MAX_CHANNELS).contains(&channels) => {
            Some(AudioFormat { rate, channels })
        }
        _ => return Err(invalid("audio format")),
    };
    let length = read_u64(input)?;
    if length > MAX_VIDEO {
        return Err(invalid("video too large"));
    }
    let mut video = Vec::new();
    input.take(length).read_to_end(&mut video)?;
    if video.len() as u64 != length {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    Ok((Start { audio }, video))
}

pub fn write_control(out: &mut impl Write, control: Control) -> io::Result<()> {
    match control {
        Control::Seek {
            position,
            generation,
        } => {
            let mut buffer = [0; 13];
            buffer[0] = SEEK;
            buffer[1..9].copy_from_slice(&position.to_le_bytes());
            buffer[9..].copy_from_slice(&generation.to_le_bytes());
            out.write_all(&buffer)?;
        }
    }
    out.flush()
}

/// The next control; `None` once the input is closed.
pub fn read_control(input: &mut impl Read) -> io::Result<Option<Control>> {
    let mut tag = [0];
    if input.read(&mut tag)? == 0 {
        return Ok(None);
    }
    match tag[0] {
        SEEK => Ok(Some(Control::Seek {
            position: read_u64(input)?,
            generation: read_u32(input)?,
        })),
        _ => Err(invalid("control")),
    }
}

pub fn write_message(out: &mut impl Write, message: &Message) -> io::Result<()> {
    match message {
        Message::Info {
            width,
            height,
            duration,
            audio,
        } => {
            out.write_all(&[INFO])?;
            out.write_all(&width.to_le_bytes())?;
            out.write_all(&height.to_le_bytes())?;
            out.write_all(&duration.unwrap_or(u64::MAX).to_le_bytes())?;
            out.write_all(&[u8::from(*audio)])?;
        }
        Message::Video {
            generation,
            pts,
            pixels,
        } => {
            out.write_all(&[VIDEO])?;
            out.write_all(&generation.to_le_bytes())?;
            out.write_all(&pts.to_le_bytes())?;
            out.write_all(pixels)?;
        }
        Message::Audio {
            generation,
            pts,
            samples,
        } => {
            out.write_all(&[AUDIO])?;
            out.write_all(&generation.to_le_bytes())?;
            out.write_all(&pts.to_le_bytes())?;
            out.write_all(&(samples.len() as u32).to_le_bytes())?;
            let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
            out.write_all(&bytes)?;
        }
        Message::End { generation } => {
            out.write_all(&[END])?;
            out.write_all(&generation.to_le_bytes())?;
        }
    }
    Ok(())
}

/// Reads the helper's messages, checking each against the limits and the
/// [`Message::Info`] that must come first.
pub struct Reader<R> {
    input: R,
    size: Option<(u32, u32)>,
}

impl<R: Read> Reader<R> {
    pub fn new(input: R) -> Self {
        Self { input, size: None }
    }

    /// The next message; `None` once the helper is done.
    pub fn message(&mut self) -> io::Result<Option<Message>> {
        let mut tag = [0];
        if self.input.read(&mut tag)? == 0 {
            return Ok(None);
        }
        let input = &mut self.input;
        let message = match (tag[0], self.size) {
            (INFO, None) => {
                let width = read_u32(input)?;
                let height = read_u32(input)?;
                if !(1..=MAX_SIDE).contains(&width) || !(1..=MAX_SIDE).contains(&height) {
                    return Err(invalid("frame size"));
                }
                let duration = Some(read_u64(input)?).filter(|d| *d != u64::MAX);
                let audio = read_u8(input)? != 0;
                self.size = Some((width, height));
                Message::Info {
                    width,
                    height,
                    duration,
                    audio,
                }
            }
            (VIDEO, Some((width, height))) => {
                let generation = read_u32(input)?;
                let pts = read_u64(input)?;
                let mut pixels = vec![0; width as usize * height as usize * 4];
                input.read_exact(&mut pixels)?;
                Message::Video {
                    generation,
                    pts,
                    pixels,
                }
            }
            (AUDIO, Some(_)) => {
                let generation = read_u32(input)?;
                let pts = read_u64(input)?;
                let count = read_u32(input)?;
                if count > MAX_SAMPLES {
                    return Err(invalid("too many samples"));
                }
                let mut bytes = vec![0; count as usize * 4];
                input.read_exact(&mut bytes)?;
                let samples = bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|b| f32::from_le_bytes(*b))
                    .collect();
                Message::Audio {
                    generation,
                    pts,
                    samples,
                }
            }
            (END, Some(_)) => Message::End {
                generation: read_u32(input)?,
            },
            _ => return Err(invalid("message")),
        };
        Ok(Some(message))
    }
}

fn read_u8(input: &mut impl Read) -> io::Result<u8> {
    let mut bytes = [0; 1];
    input.read_exact(&mut bytes)?;
    Ok(bytes[0])
}

fn read_u16(input: &mut impl Read) -> io::Result<u16> {
    let mut bytes = [0; 2];
    input.read_exact(&mut bytes)?;
    Ok(u16::from_le_bytes(bytes))
}

fn read_u32(input: &mut impl Read) -> io::Result<u32> {
    let mut bytes = [0; 4];
    input.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64(input: &mut impl Read) -> io::Result<u64> {
    let mut bytes = [0; 8];
    input.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("bad {what} from the video helper"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_and_controls_round_trip() {
        let start = Start {
            audio: Some(AudioFormat {
                rate: 48_000,
                channels: 2,
            }),
        };
        let mut bytes = Vec::new();
        write_start(&mut bytes, start, b"video").unwrap();
        write_control(
            &mut bytes,
            Control::Seek {
                position: 5_000_000,
                generation: 3,
            },
        )
        .unwrap();

        let mut input = bytes.as_slice();
        assert_eq!(read_start(&mut input).unwrap(), (start, b"video".to_vec()));
        assert_eq!(
            read_control(&mut input).unwrap(),
            Some(Control::Seek {
                position: 5_000_000,
                generation: 3
            })
        );
        assert_eq!(read_control(&mut input).unwrap(), None);
    }

    #[test]
    fn bad_starts_are_refused() {
        let mut silent = Vec::new();
        write_start(&mut silent, Start { audio: None }, b"").unwrap();
        assert_eq!(read_start(&mut silent.as_slice()).unwrap().0.audio, None);

        let mut odd = Vec::new();
        let odd_rate = AudioFormat {
            rate: 1,
            channels: 2,
        };
        write_start(
            &mut odd,
            Start {
                audio: Some(odd_rate),
            },
            b"",
        )
        .unwrap();
        assert!(read_start(&mut odd.as_slice()).is_err());

        let mut huge = Vec::new();
        huge.extend_from_slice(&[0; 6]);
        huge.extend_from_slice(&(MAX_VIDEO + 1).to_le_bytes());
        assert!(read_start(&mut huge.as_slice()).is_err());

        let mut short = Vec::new();
        write_start(&mut short, Start { audio: None }, b"video").unwrap();
        short.pop();
        assert!(read_start(&mut short.as_slice()).is_err());
    }

    #[test]
    fn messages_round_trip() {
        let messages = [
            Message::Info {
                width: 2,
                height: 1,
                duration: Some(1_500_000),
                audio: true,
            },
            Message::Video {
                generation: 0,
                pts: 33_333,
                pixels: vec![1, 2, 3, 4, 5, 6, 7, 8],
            },
            Message::Audio {
                generation: 1,
                pts: 0,
                samples: vec![0.5, -0.25],
            },
            Message::End { generation: 1 },
        ];
        let mut bytes = Vec::new();
        for message in &messages {
            write_message(&mut bytes, message).unwrap();
        }

        let mut reader = Reader::new(bytes.as_slice());
        for message in messages {
            assert_eq!(reader.message().unwrap(), Some(message));
        }
        assert_eq!(reader.message().unwrap(), None);
    }

    #[test]
    fn messages_out_of_order_or_limits_are_refused() {
        let read = |messages: &[Message]| {
            let mut bytes = Vec::new();
            for message in messages {
                write_message(&mut bytes, message).unwrap();
            }
            let mut reader = Reader::new(bytes.as_slice());
            loop {
                match reader.message() {
                    Ok(Some(_)) => {}
                    Ok(None) => return Ok(()),
                    Err(error) => return Err(error),
                }
            }
        };
        let info = |width, height| Message::Info {
            width,
            height,
            duration: None,
            audio: false,
        };

        assert!(read(&[Message::End { generation: 0 }]).is_err());
        assert!(read(&[info(1, 1), info(1, 1)]).is_err());
        assert!(read(&[info(MAX_SIDE + 1, 1)]).is_err());
        assert!(read(&[info(0, 1)]).is_err());
        let too_many = Message::Audio {
            generation: 0,
            pts: 0,
            samples: vec![0.; MAX_SAMPLES as usize + 1],
        };
        assert!(read(&[info(1, 1), too_many]).is_err());
        // A frame cut short.
        let mut bytes = Vec::new();
        write_message(&mut bytes, &info(2, 2)).unwrap();
        write_message(
            &mut bytes,
            &Message::Video {
                generation: 0,
                pts: 0,
                pixels: vec![0; 8],
            },
        )
        .unwrap();
        let mut reader = Reader::new(bytes.as_slice());
        reader.message().unwrap();
        assert!(reader.message().is_err());
    }
}
