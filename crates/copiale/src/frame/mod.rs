//! `copiale --video-frame`: the first frame of a video as a PNG, for the
//! log's preview.
//!
//! Others' videos are untrusted and FFmpeg is C, so this runs as a process
//! of its own, sandboxed before it reads the video (`sandbox`): a decoder
//! exploit reaches no keys, files or network. The core runs it
//! (`copiale_core::video`).

mod decode;
mod sandbox;

use std::io::{Read as _, Write as _};
use std::process::ExitCode;

pub use decode::first_frame;

/// The flag that runs the helper.
pub const FLAG: &str = "--video-frame";

/// Largest video read, in bytes: Signal's attachment limit.
const MAX_VIDEO: u64 = 100 * 1024 * 1024;

/// Reads a video on stdin, writes its first frame as a PNG on stdout;
/// fails without output.
pub fn run() -> ExitCode {
    decode::init();
    if let Err(error) = sandbox::enter() {
        // Never decodes unconfined.
        eprintln!("can't sandbox the video frame helper: {error}");
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
    let Ok(png) = first_frame(video) else {
        return ExitCode::FAILURE;
    };
    let mut stdout = std::io::stdout().lock();
    match stdout.write_all(&png).and_then(|()| stdout.flush()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}
