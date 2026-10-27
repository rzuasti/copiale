//! Log output: stderr, plus a log file where stderr goes nowhere (Windows,
//! where Copiale is a GUI program).

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};

use tracing_subscriber::fmt::MakeWriter;

/// Most a run writes to its log file; later lines are dropped.
pub const MAX_LOG_FILE: u64 = 8 * 1024 * 1024;

const TRUNCATED: &[u8] = b"log file full: later lines dropped\n";

static FILE: OnceLock<Mutex<Capped<File>>> = OnceLock::new();

/// Also logs to `path` from now on. The previous run's log is kept beside
/// it, `.old` appended. Only the running instance may call this: another
/// process would move its log away.
pub fn log_to_file(path: &Path) -> io::Result<()> {
    let file = Capped::new(open_rotated(path)?, MAX_LOG_FILE);
    FILE.set(Mutex::new(file))
        .map_err(|_| io::Error::other("already logging to a file"))
}

fn open_rotated(path: &Path) -> io::Result<File> {
    match fs::rename(path, old_path(path)) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    File::create(path)
}

fn old_path(path: &Path) -> PathBuf {
    let mut old = OsString::from(path);
    old.push(".old");
    old.into()
}

/// Writes up to a limit, then a notice, then nothing.
struct Capped<W> {
    inner: W,
    /// Left before the notice, which there is always room for; `None` once
    /// written.
    left: Option<u64>,
}

impl<W: Write> Capped<W> {
    fn new(inner: W, limit: u64) -> Self {
        Self {
            inner,
            left: Some(limit.saturating_sub(TRUNCATED.len() as u64)),
        }
    }

    /// Writes all of `line` or none of it.
    fn write_line(&mut self, line: &[u8]) -> io::Result<()> {
        let Some(left) = self.left else {
            return Ok(());
        };
        if line.len() as u64 <= left {
            self.left = Some(left - line.len() as u64);
            return self.inner.write_all(line);
        }
        self.left = None;
        self.inner.write_all(TRUNCATED)
    }
}

/// Stderr, and the log file once [`log_to_file`] opens it.
pub(super) struct Output;

impl<'a> MakeWriter<'a> for Output {
    type Writer = OutputWriter;

    fn make_writer(&'a self) -> Self::Writer {
        OutputWriter
    }
}

pub(super) struct OutputWriter;

impl Write for OutputWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // Stderr may be closed; the file still gets the line.
        io::stderr().write_all(buf).ok();
        if let Some(file) = FILE.get() {
            file.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .write_line(buf)?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        io::stderr().flush().ok();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_past_the_limit_are_dropped_after_a_notice() {
        let limit = TRUNCATED.len() as u64 + 10;
        let mut capped = Capped::new(Vec::new(), limit);
        capped.write_line(b"12345\n").unwrap();
        capped.write_line(b"123\n").unwrap();
        capped.write_line(b"1\n").unwrap();
        capped.write_line(b"\n").unwrap();
        let mut expected = b"12345\n123\n".to_vec();
        expected.extend_from_slice(TRUNCATED);
        assert_eq!(capped.inner, expected);
        assert!(capped.inner.len() as u64 <= limit);
    }

    #[test]
    fn each_run_keeps_the_previous_log() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("copiale.log");
        for run in ["first", "second", "third"] {
            let mut file = open_rotated(&path).unwrap();
            file.write_all(run.as_bytes()).unwrap();
        }
        assert_eq!(fs::read_to_string(&path).unwrap(), "third");
        assert_eq!(fs::read_to_string(old_path(&path)).unwrap(), "second");
    }
}
