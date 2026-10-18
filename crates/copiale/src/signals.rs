//! Termination signals (Ctrl+C, `kill`, logging out): the first quits as
//! the user would, so the core stores what's arriving and closes storage; a
//! second ends the process at once.

use std::io;
use std::thread;

use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::Signals;
use signal_hook::low_level;

/// Calls `quit` on the first signal, from a thread of its own.
pub fn quit_on_signals(quit: impl FnOnce() + Send + 'static) -> io::Result<()> {
    let mut signals = Signals::new([SIGINT, SIGTERM, SIGHUP])?;
    thread::Builder::new()
        .name("copiale-signals".into())
        .spawn(move || {
            let mut received = signals.forever();
            let Some(signal) = received.next() else {
                return;
            };
            tracing::info!(signal = name(signal), "quitting");
            quit();
            let Some(signal) = received.next() else {
                return;
            };
            tracing::warn!(signal = name(signal), "quitting at once");
            // The default action runs no exit handlers, so SQLCipher's can't
            // crash on connections still open.
            if low_level::emulate_default_handler(signal).is_err() {
                std::process::exit(128 + signal);
            }
        })?;
    Ok(())
}

fn name(signal: i32) -> &'static str {
    low_level::signal_name(signal).unwrap_or("unknown")
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::os::unix::process::ExitStatusExt as _;
    use std::process::Command;
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    const QUIT: &str = "quit called";

    /// Signals stay handled for the rest of the process, so this runs in a
    /// process of its own.
    #[test]
    #[ignore = "run by first_signal_quits_and_a_second_ends_the_process"]
    fn signalled_process() {
        let (quit, quits) = mpsc::channel();
        quit_on_signals(move || quit.send(()).unwrap()).unwrap();

        low_level::raise(SIGTERM).unwrap();
        quits.recv_timeout(Duration::from_secs(5)).unwrap();
        println!("{QUIT}");
        io::stdout().flush().unwrap();

        low_level::raise(SIGHUP).unwrap();
        thread::sleep(Duration::from_secs(5));
        panic!("still running after a second signal");
    }

    #[test]
    fn first_signal_quits_and_a_second_ends_the_process() {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "signals::tests::signalled_process",
                "--ignored",
                "--nocapture",
            ])
            .output()
            .unwrap();

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains(QUIT), "{stdout}");
        assert_eq!(output.status.signal(), Some(SIGHUP), "{output:?}");
    }
}
