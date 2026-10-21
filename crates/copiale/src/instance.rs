//! Single instance: the first process holds a lock and listens on a local
//! socket; later ones pass it their request and exit.
//!
//! One request per connection: a line such as `toggle <token>`, answered
//! with `ok` or `error`.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, BufRead as _, BufReader, Read as _, Write as _};
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use copiale_ui::Request;
use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{ListenerOptions, Name, Stream};

#[cfg(windows)]
mod windows;

/// Longest request line read.
const MAX_LINE: u64 = 1024;
/// Longest activation token passed on.
const MAX_TOKEN: usize = 512;
/// How long a later process waits for the first one to start listening.
const CONNECT_WAIT: Duration = Duration::from_secs(2);

pub enum Claim {
    /// This is the first process.
    First(Instance),
    /// Another process is running; send it requests.
    Running,
}

/// The first process's lock and socket.
pub struct Instance {
    _lock: File,
    listener: LocalSocketListener,
}

/// Becomes the running instance, unless another process already is.
pub fn claim(dir: &Path) -> io::Result<Claim> {
    copiale_core::create_private_dir(dir)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("copiale.lock"))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Ok(Claim::Running),
        Err(TryLockError::Error(error)) => return Err(error),
    }
    // Holding the lock, any socket left behind is stale.
    let options = ListenerOptions::new().name(name(dir)?).try_overwrite(true);
    #[cfg(unix)]
    let options = {
        use interprocess::os::unix::local_socket::ListenerOptionsExt as _;
        options.mode(0o600)
    };
    #[cfg(windows)]
    let options = windows::private(options)?;
    Ok(Claim::First(Instance {
        _lock: lock,
        listener: options.create_sync()?,
    }))
}

impl Instance {
    /// Answers requests on a background thread for as long as the process
    /// runs.
    pub fn serve(self, on_request: impl Fn(Request) + Send + Sync + 'static) {
        let on_request = Arc::new(on_request);
        let spawned = thread::Builder::new()
            .name("instance".into())
            .spawn(move || {
                let _lock = self._lock;
                for connection in self.listener.incoming() {
                    let connection = match connection {
                        Ok(connection) => connection,
                        Err(error) => {
                            tracing::warn!(%error, "instance connection failed");
                            continue;
                        }
                    };
                    let on_request = on_request.clone();
                    // A client that never finishes its line holds only its own thread.
                    thread::spawn(move || {
                        if let Err(error) = answer(connection, &*on_request) {
                            tracing::warn!(%error, "instance request failed");
                        }
                    });
                }
            });
        if let Err(error) = spawned {
            tracing::error!(%error, "can't listen for other instances");
        }
    }
}

fn answer(connection: Stream, on_request: &dyn Fn(Request)) -> io::Result<()> {
    let mut reader = BufReader::new(connection);
    let mut line = String::new();
    reader.by_ref().take(MAX_LINE).read_line(&mut line)?;
    let request = decode(&line);
    let reply: &[u8] = if request.is_some() {
        b"ok\n"
    } else {
        b"error\n"
    };
    reader.get_mut().write_all(reply)?;
    if let Some(request) = request {
        on_request(request);
    }
    Ok(())
}

/// Passes `request` to the running instance.
pub fn send(dir: &Path, request: &Request) -> io::Result<()> {
    let name = name(dir)?;
    let mut waited = Duration::ZERO;
    let connection = loop {
        match Stream::connect(name.borrow()) {
            Ok(connection) => break connection,
            // The first process may not be listening yet.
            Err(_) if waited < CONNECT_WAIT => {
                thread::sleep(Duration::from_millis(100));
                waited += Duration::from_millis(100);
            }
            Err(error) => return Err(error),
        }
    };
    #[cfg(windows)]
    windows::check_server(&connection)?;
    let mut reader = BufReader::new(connection);
    reader
        .get_mut()
        .write_all(format!("{}\n", encode(request)).as_bytes())?;
    let mut reply = String::new();
    reader.take(MAX_LINE).read_line(&mut reply)?;
    match reply.trim_end() {
        "ok" => Ok(()),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the running instance refused the request",
        )),
    }
}

/// The request as a line, without the newline.
pub fn encode(request: &Request) -> String {
    let (verb, token) = match request {
        Request::Show { activation_token } => ("show", activation_token),
        Request::Toggle { activation_token } => ("toggle", activation_token),
        Request::Quit => ("quit", &None),
    };
    match token.as_deref().filter(|token| valid_token(token)) {
        Some(token) => format!("{verb} {token}"),
        None => verb.to_owned(),
    }
}

/// `None` for anything but a well-formed request.
pub fn decode(line: &str) -> Option<Request> {
    let line = line.strip_suffix('\n').unwrap_or(line);
    let (verb, token) = match line.split_once(' ') {
        Some((verb, token)) => (verb, Some(token)),
        None => (line, None),
    };
    if token.is_some_and(|token| !valid_token(token)) {
        return None;
    }
    let activation_token = token.map(str::to_owned);
    match verb {
        "show" => Some(Request::Show { activation_token }),
        "toggle" => Some(Request::Toggle { activation_token }),
        "quit" if activation_token.is_none() => Some(Request::Quit),
        _ => None,
    }
}

/// Tokens are opaque; keep them to one printable word.
fn valid_token(token: &str) -> bool {
    !token.is_empty() && token.len() <= MAX_TOKEN && token.bytes().all(|b| b.is_ascii_graphic())
}

#[cfg(unix)]
fn name(dir: &Path) -> io::Result<Name<'static>> {
    use interprocess::local_socket::GenericFilePath;
    dir.join("copiale.sock")
        .to_fs_name::<GenericFilePath>()
        .map(Name::into_owned)
}

#[cfg(windows)]
fn name(_: &Path) -> io::Result<Name<'static>> {
    windows::name()
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    fn show(token: Option<&str>) -> Request {
        Request::Show {
            activation_token: token.map(Into::into),
        }
    }

    #[test]
    fn requests_round_trip() {
        for request in [
            show(None),
            show(Some("gnome-shell/Copiale/1234-0-host_TIME5678")),
            Request::Toggle {
                activation_token: Some("abc".into()),
            },
            Request::Toggle {
                activation_token: None,
            },
            Request::Quit,
        ] {
            assert_eq!(decode(&format!("{}\n", encode(&request))), Some(request));
        }
    }

    #[test]
    fn rejects_malformed_requests() {
        for line in ["", "\n", "hide", "show ", "show a b", "quit token", "SHOW"] {
            assert_eq!(decode(line), None, "{line:?}");
        }
        assert_eq!(decode(&format!("show {}", "x".repeat(MAX_TOKEN + 1))), None);
        assert_eq!(decode("show tök"), None);
    }

    #[test]
    fn drops_tokens_that_cant_be_sent() {
        assert_eq!(encode(&show(Some("two words"))), "show");
        assert_eq!(encode(&show(Some(""))), "show");
    }

    #[test]
    fn later_processes_reach_the_first() {
        let dir = tempfile::tempdir().unwrap();
        let Claim::First(instance) = claim(dir.path()).unwrap() else {
            panic!("first claim");
        };
        assert!(matches!(claim(dir.path()).unwrap(), Claim::Running));

        let (received, requests) = mpsc::channel();
        instance.serve(move |request| received.send(request).unwrap());
        send(dir.path(), &show(Some("token"))).unwrap();
        send(dir.path(), &Request::Quit).unwrap();

        let timeout = Duration::from_secs(5);
        assert_eq!(requests.recv_timeout(timeout).unwrap(), show(Some("token")));
        assert_eq!(requests.recv_timeout(timeout).unwrap(), Request::Quit);
    }

    #[test]
    fn a_stale_socket_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let Claim::First(first) = claim(dir.path()).unwrap() else {
            panic!("first claim");
        };
        // A crashed instance leaves its socket file but not its lock.
        std::mem::forget(first.listener);
        let _spawning = crate::spawn_guard();
        drop(first._lock);

        let Claim::First(instance) = claim(dir.path()).unwrap() else {
            panic!("claim after a crash");
        };
        let (received, requests) = mpsc::channel();
        instance.serve(move |request| received.send(request).unwrap());
        send(dir.path(), &Request::Quit).unwrap();
        assert_eq!(
            requests.recv_timeout(Duration::from_secs(5)).unwrap(),
            Request::Quit
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_directory_is_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path().join("copiale");
        let _claim = claim(&dir).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }
}
