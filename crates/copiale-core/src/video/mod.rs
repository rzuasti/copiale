//! Runs the helper that decodes videos, a sandboxed process of its own:
//! `copiale --video-frame` takes a video on stdin and gives its first
//! frame as a PNG; `copiale --video-play` streams it for [`Player`].

mod player;
pub mod protocol;
#[cfg(windows)]
mod sandbox;

pub use player::{Frame, Player, Status};

use std::ffi::OsString;
use std::path::PathBuf;
#[cfg(unix)]
use std::process::Stdio;
use std::time::Duration;

#[cfg(unix)]
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// Runs the helper for a first frame.
pub const FRAME_FLAG: &str = "--video-frame";
/// Runs the helper for playback.
pub const PLAY_FLAG: &str = "--video-play";

/// How long the helper may take for a first frame.
const TIMEOUT: Duration = Duration::from_secs(30);
/// Largest frame accepted, in bytes.
const MAX_FRAME: u64 = 16 * 1024 * 1024;
/// How every PNG starts.
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

/// The program and arguments that run the helper, before its mode's flag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoHelper {
    program: PathBuf,
    args: Vec<OsString>,
}

impl VideoHelper {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self::with_args(program, Vec::<OsString>::new())
    }

    pub fn with_args(
        program: impl Into<PathBuf>,
        args: impl IntoIterator<Item = impl Into<OsString>>,
    ) -> Self {
        Self {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
        }
    }

    /// `video`'s first frame as a PNG. The helper gets no environment and
    /// no stderr, and is killed past [`TIMEOUT`].
    pub async fn first_frame(&self, video: Vec<u8>) -> std::io::Result<Vec<u8>> {
        let (success, frame) = tokio::time::timeout(TIMEOUT, self.run_frame(video))
            .await
            .map_err(|_| std::io::Error::other("the video frame helper timed out"))??;
        if !success {
            return Err(std::io::Error::other("no frame from the video"));
        }
        if frame.len() as u64 > MAX_FRAME || !frame.starts_with(PNG_SIGNATURE) {
            return Err(std::io::Error::other("the video frame helper gave no PNG"));
        }
        Ok(frame)
    }

    /// Whether the helper succeeded, and what it gave.
    #[cfg(unix)]
    async fn run_frame(&self, video: Vec<u8>) -> std::io::Result<(bool, Vec<u8>)> {
        let mut child = tokio::process::Command::new(&self.program)
            .args(&self.args)
            .arg(FRAME_FLAG)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let mut stdin = child.stdin.take().ok_or_else(|| missing("stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| missing("stdout"))?;
        let write = async {
            // A helper that gives up early closes its end: not an error
            // here, its exit status says.
            stdin.write_all(&video).await.ok();
            drop(stdin);
        };
        let mut frame = Vec::new();
        let mut limited = stdout.take(MAX_FRAME + 1);
        let read = limited.read_to_end(&mut frame);
        let ((), read) = tokio::join!(write, read);
        read?;
        let status = child.wait().await?;
        Ok((status.success(), frame))
    }

    /// Whether the helper succeeded, and what it gave; it's killed once
    /// this is done or dropped.
    #[cfg(windows)]
    async fn run_frame(&self, video: Vec<u8>) -> std::io::Result<(bool, Vec<u8>)> {
        struct Killing(std::sync::Arc<sandbox::Process>);
        impl Drop for Killing {
            fn drop(&mut self) {
                self.0.kill().ok();
            }
        }

        let (process, stdin, stdout) = self.spawn(FRAME_FLAG, sandbox::FRAME_CPU)?;
        let killing = Killing(std::sync::Arc::new(process));
        let process = std::sync::Arc::clone(&killing.0);
        // Its pipes block.
        tokio::task::spawn_blocking(move || frame_from(&process, stdin, stdout, video))
            .await
            .map_err(std::io::Error::other)?
    }
}

/// Writes `video` to the helper while reading what it gives.
#[cfg(windows)]
fn frame_from(
    process: &sandbox::Process,
    mut stdin: std::fs::File,
    stdout: std::fs::File,
    video: Vec<u8>,
) -> std::io::Result<(bool, Vec<u8>)> {
    use std::io::{Read as _, Write as _};

    let writing = std::thread::spawn(move || {
        // As on Unix, its exit status says.
        stdin.write_all(&video).ok();
    });
    let mut frame = Vec::new();
    stdout.take(MAX_FRAME + 1).read_to_end(&mut frame)?;
    writing.join().ok();
    Ok((process.wait()? == 0, frame))
}

/// A running helper.
#[cfg(unix)]
type Child = std::process::Child;
#[cfg(windows)]
type Child = sandbox::Process;

impl VideoHelper {
    /// The helper streaming a video, with no environment or stderr; with
    /// its stdin and stdout.
    #[cfg(unix)]
    fn spawn_player(
        &self,
    ) -> std::io::Result<(Child, std::process::ChildStdin, std::process::ChildStdout)> {
        let mut child = std::process::Command::new(&self.program)
            .args(&self.args)
            .arg(PLAY_FLAG)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child.stdin.take().ok_or_else(|| missing("stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| missing("stdout"))?;
        Ok((child, stdin, stdout))
    }

    #[cfg(windows)]
    fn spawn_player(&self) -> std::io::Result<(Child, std::fs::File, std::fs::File)> {
        self.spawn(PLAY_FLAG, sandbox::PLAY_CPU)
    }

    /// The helper in its sandbox, with `flag`.
    #[cfg(windows)]
    fn spawn(
        &self,
        flag: &str,
        cpu: Duration,
    ) -> std::io::Result<(Child, std::fs::File, std::fs::File)> {
        let args: Vec<&std::ffi::OsStr> = self
            .args
            .iter()
            .map(OsString::as_os_str)
            .chain([std::ffi::OsStr::new(flag)])
            .collect();
        sandbox::spawn(sandbox::PROFILE, &self.program, &args, cpu)
    }

    /// Removes what the sandbox keeps between runs: Windows' AppContainer
    /// profile.
    #[cfg(windows)]
    pub fn remove_sandbox(&self) -> std::io::Result<()> {
        sandbox::remove(sandbox::PROFILE)
    }

    /// Nothing kept here.
    #[cfg(not(windows))]
    pub fn remove_sandbox(&self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Stops `child` in the background.
#[cfg(unix)]
fn stop(mut child: Child) {
    child.kill().ok();
    std::thread::spawn(move || child.wait().ok());
}

/// Stops `child`: its job kills it once closed.
#[cfg(windows)]
fn stop(child: Child) {
    drop(child);
}

#[cfg(unix)]
fn missing(what: &str) -> std::io::Error {
    std::io::Error::other(format!("no {what} for the video helper"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// Runs `script` in `sh`; the helper gets no PATH, so `{name}` stands
    /// for a tool's full path.
    fn shell(script: &str) -> VideoHelper {
        let mut script = script.to_owned();
        for name in ["cat", "wc", "tr"] {
            script = script.replace(&format!("{{{name}}}"), &tool(name));
        }
        VideoHelper::with_args("/bin/sh", ["-c".to_owned(), script])
    }

    fn tool(name: &str) -> String {
        std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|dir| dir.join(name))
            .find(|path| path.is_file())
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    #[tokio::test]
    async fn the_helper_reads_the_video_and_gives_its_png() {
        // Echoes the input's size after a PNG signature.
        let helper = shell(r"printf '\211PNG\r\n\032\n'; {wc} -c | {tr} -d ' '");

        let frame = helper.first_frame(vec![7; 100_000]).await.unwrap();

        assert_eq!(&frame[..8], PNG_SIGNATURE);
        assert_eq!(String::from_utf8_lossy(&frame[8..]).trim(), "100000");
    }

    #[tokio::test]
    async fn failures_give_no_frame() {
        let failed = shell(r"{cat} >/dev/null; printf '\211PNG\r\n\032\n'; exit 1");
        assert!(failed.first_frame(vec![1; 10]).await.is_err());

        let not_png = shell("{cat} >/dev/null; echo hello");
        assert!(not_png.first_frame(vec![1; 10]).await.is_err());

        let early = shell(r"exit 1");
        assert!(early.first_frame(vec![1; 1_000_000]).await.is_err());

        let missing = VideoHelper::new("/nonexistent/copiale");
        assert!(missing.first_frame(vec![1; 10]).await.is_err());
    }

    #[tokio::test]
    async fn the_helper_gets_no_environment() {
        // SAFETY: tests here don't read the environment concurrently.
        unsafe { std::env::set_var("COPIALE_SECRET", "x") };
        let helper = shell(
            r#"{cat} >/dev/null; printf '\211PNG\r\n\032\n'; printf "${COPIALE_SECRET:-none}""#,
        );

        let frame = helper.first_frame(Vec::new()).await.unwrap();

        assert_eq!(&frame[8..], b"none");
    }
}
