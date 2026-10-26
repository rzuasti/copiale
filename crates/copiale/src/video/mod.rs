//! The video helper: `copiale --video-frame` gives a video's first frame as
//! a PNG, for the log's preview; `copiale --video-play` streams it for the
//! viewer's player.
//!
//! Others' videos are untrusted and FFmpeg is C, so this runs as a process
//! of its own, sandboxed before it reads the video (`sandbox`): a decoder
//! exploit reaches no keys, files or network. The app runs it
//! (`copiale_core::video`).

mod decode;
mod play;
mod sandbox;

use std::io::{Read as _, Write as _};
use std::process::ExitCode;
use std::sync::Arc;

use copiale_core::video::protocol::{self, MAX_VIDEO};
use sandbox::Mode;

/// Reads a video on stdin, writes its first frame as a PNG on stdout;
/// fails without output.
pub fn run_frame() -> ExitCode {
    if !confine(Mode::Frame) {
        return ExitCode::FAILURE;
    }
    let mut video = Vec::new();
    let read = std::io::stdin()
        .lock()
        .take(MAX_VIDEO + 1)
        .read_to_end(&mut video);
    if read.is_err() || video.len() as u64 > MAX_VIDEO {
        return ExitCode::FAILURE;
    }
    let Ok(png) = decode::first_frame(video) else {
        return ExitCode::FAILURE;
    };
    let mut stdout = std::io::stdout().lock();
    match stdout.write_all(&png).and_then(|()| stdout.flush()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

/// Plays the video sent on stdin to stdout until stdin closes.
pub fn run_play() -> ExitCode {
    if !confine(Mode::Play) {
        return ExitCode::FAILURE;
    }
    let Ok((start, video)) = protocol::read_start(&mut std::io::stdin().lock()) else {
        return ExitCode::FAILURE;
    };
    let controls = play::SharedControls::default();
    let reading = Arc::clone(&controls);
    std::thread::spawn(move || play::read_controls(&mut std::io::stdin().lock(), &reading));
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    match play::play(start, video, &controls, &mut out) {
        Ok(()) | Err(play::PlayError::Closed) => ExitCode::SUCCESS,
        Err(play::PlayError::Video) => ExitCode::FAILURE,
    }
}

/// Quiets FFmpeg, then enters the sandbox; `false` if it can't.
fn confine(mode: Mode) -> bool {
    decode::init();
    match sandbox::enter(mode) {
        Ok(()) => true,
        Err(error) => {
            // Never decodes unconfined.
            eprintln!("can't sandbox the video helper: {error}");
            false
        }
    }
}
