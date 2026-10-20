//! The panic hook logs through the scrubbing logger. A separate test binary,
//! with a single test: the hook is process-wide.

use std::io::Write;
use std::sync::{Arc, Mutex};

use copiale_core::logging::{DEFAULT_FILTER, Options, install_panic_hook, subscriber};
use tracing_subscriber::fmt::MakeWriter;

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Capture {
    type Writer = Capture;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Panics with `panic` under a hook installed with `unsafe_log` and returns
/// the logged output.
fn logged_panic(unsafe_log: bool, panic: impl FnOnce() + std::panic::UnwindSafe) -> String {
    let capture = Capture::default();
    let options = Options {
        filter: DEFAULT_FILTER.into(),
        unsafe_log,
    };
    install_panic_hook(unsafe_log);
    let result = tracing::subscriber::with_default(subscriber(&options, capture.clone()), || {
        std::panic::catch_unwind(panic)
    });
    assert!(result.is_err());
    String::from_utf8(capture.0.lock().unwrap().clone()).unwrap()
}

#[test]
fn panics_are_logged_without_their_data() {
    let text = String::from("meet at the old mill");

    let out = logged_panic(false, || {
        let _ = &text[..text.len() + 1];
    });
    assert!(out.contains("panic at "), "{out}");
    assert!(out.contains("panic_hook.rs:"), "{out}");
    assert!(
        out.contains("message hidden; see COPIALE_UNSAFE_LOG"),
        "{out}"
    );
    assert!(!out.contains("old mill"), "{out}");

    let out = logged_panic(false, || panic!("literal message"));
    assert!(out.contains("panic_hook.rs:"), "{out}");
    assert!(out.contains("literal message"), "{out}");

    let out = logged_panic(true, || {
        panic!("lost track of 0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0 in {text}");
    });
    assert!(out.contains("panic: "), "{out}");
    assert!(out.contains("old mill"), "{out}");
    assert!(!out.contains("0f1e2d3c"), "scrubbed still: {out}");
}
