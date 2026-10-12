//! UI toolkit spike: chat prototype on iced.
//!
//! Flags:
//! - `--feed`: start the live feed (50 msg/s) on the open conversation.
//! - `--jump <id>`: jump to a message on startup.
//! - `--metrics`: print startup timing and RSS samples to stderr.
//! - `--theme <name>`: start with a named theme instead of following the system.
//! - `--bench-scroll [px]`: fling from newest to oldest message at `px` per
//!   frame (default 200), print frame times and RSS, then quit.

use fixtures::Dataset;
use spike_iced::app::{App, Options};

fn parse_args() -> Options {
    let mut options = Options {
        live_feed: true,
        ..Options::default()
    };
    let mut iter = std::env::args().skip(1).peekable();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--feed" => options.feed = true,
            "--metrics" => options.metrics = true,
            "--jump" => options.jump = iter.next().and_then(|id| id.parse().ok()),
            "--theme" => options.theme = iter.next(),
            "--bench-scroll" => {
                let step = iter.next_if(|next| !next.starts_with("--"));
                options.bench_scroll = Some(step.and_then(|s| s.parse().ok()).unwrap_or(200.));
            }
            other => eprintln!("ignoring unknown argument {other}"),
        }
    }
    options
}

fn main() -> iced::Result {
    let started = std::time::Instant::now();
    let options = Options {
        started: Some(started),
        ..parse_args()
    };
    iced::application(
        move || App::new(Dataset::standard(), options.clone()),
        App::update,
        App::view,
    )
    .title("Copiale · iced spike")
    .subscription(App::subscription)
    .theme(App::theme)
    .window_size((1200., 800.))
    .run()
}
