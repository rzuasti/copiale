//! Runs the helper that decodes a video's first frame (`copiale
//! --video-frame`), a sandboxed process of its own: the video goes in on
//! stdin, a PNG comes back on stdout.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::process::Command;

/// How long the helper may take.
const TIMEOUT: Duration = Duration::from_secs(30);
/// Largest frame accepted, in bytes.
const MAX_FRAME: u64 = 16 * 1024 * 1024;
/// How every PNG starts.
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

/// The program and arguments that run the helper.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameHelper {
    program: PathBuf,
    args: Vec<OsString>,
}

impl FrameHelper {
    pub fn new(
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
        let mut child = Command::new(&self.program)
            .args(&self.args)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let mut stdin = child.stdin.take().ok_or_else(|| missing("stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| missing("stdout"))?;
        let run = async {
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
            Ok::<_, std::io::Error>((status, frame))
        };
        let (status, frame) = tokio::time::timeout(TIMEOUT, run)
            .await
            .map_err(|_| std::io::Error::other("the video frame helper timed out"))??;
        if !status.success() {
            return Err(std::io::Error::other("no frame from the video"));
        }
        if frame.len() as u64 > MAX_FRAME || !frame.starts_with(PNG_SIGNATURE) {
            return Err(std::io::Error::other("the video frame helper gave no PNG"));
        }
        Ok(frame)
    }
}

fn missing(what: &str) -> std::io::Error {
    std::io::Error::other(format!("no {what} for the video frame helper"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// Runs `script` in `sh`; the helper gets no PATH, so `{name}` stands
    /// for a tool's full path.
    fn shell(script: &str) -> FrameHelper {
        let mut script = script.to_owned();
        for name in ["cat", "wc", "tr"] {
            script = script.replace(&format!("{{{name}}}"), &tool(name));
        }
        FrameHelper::new("/bin/sh", ["-c".to_owned(), script])
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

        let missing = FrameHelper::new("/nonexistent/copiale", ["--video-frame"]);
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
