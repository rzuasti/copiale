//! The panic hook logs through the scrubbing logger. A separate test binary:
//! the hook is process-wide.

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

#[test]
fn panics_are_logged_and_scrubbed() {
    let capture = Capture::default();
    let options = Options {
        filter: DEFAULT_FILTER.into(),
        unsafe_dependencies: false,
    };
    install_panic_hook();

    let result = tracing::subscriber::with_default(subscriber(&options, capture.clone()), || {
        std::panic::catch_unwind(|| {
            panic!("lost track of 0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0");
        })
    });

    assert!(result.is_err());
    let out = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(out.contains("panic: "), "{out}");
    assert!(out.contains("lost track of …e1f0"), "{out}");
    assert!(!out.contains("0f1e2d3c"), "{out}");
}
