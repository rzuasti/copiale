//! UI toolkit spike: chat prototype on GPUI + GPUI Kit.
//!
//! Flags:
//! - `--feed`: start the live feed (50 msg/s) on the open conversation.
//! - `--jump <id>`: jump to a message on startup.
//! - `--metrics`: print startup timing and RSS samples to stderr.
//! - `--theme <name>`: start with a named theme instead of following the system.
//! - `--bench-scroll [px]`: fling from newest to oldest message at `px` per
//!   frame (default 200), print frame times and RSS, then quit.

use std::time::{Duration, Instant};

use fixtures::{Dataset, MessageId};
use gpui_kit::{AppContext as _, Bounds, WindowBounds, WindowOptions, px, size};
use spike_gpui::{app::ChatApp, bench, store::ChatStore, theme};

struct Args {
    feed: bool,
    jump: Option<u64>,
    metrics: bool,
    theme: Option<String>,
    bench_scroll: Option<f32>,
}

fn parse_args() -> Args {
    let mut args = Args {
        feed: false,
        jump: None,
        metrics: false,
        theme: None,
        bench_scroll: None,
    };
    let mut iter = std::env::args().skip(1).peekable();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--feed" => args.feed = true,
            "--metrics" => args.metrics = true,
            "--jump" => args.jump = iter.next().and_then(|id| id.parse().ok()),
            "--theme" => args.theme = iter.next(),
            "--bench-scroll" => {
                let step = iter.next_if(|next| !next.starts_with("--"));
                args.bench_scroll = Some(step.and_then(|s| s.parse().ok()).unwrap_or(200.));
            }
            other => eprintln!("ignoring unknown argument {other}"),
        }
    }
    args
}

fn main() {
    let started = Instant::now();
    let args = parse_args();

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);

            let dataset = Dataset::standard();
            let store = cx.new(|cx| ChatStore::new(dataset, cx));
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(1200.), px(800.)),
                    cx,
                ))),
                app_id: Some("copiale-spike-gpui".into()),
                ..Default::default()
            };

            gpui_kit::open_window(options, cx, |window, cx| {
                theme::init(window, cx);
                if let Some(name) = &args.theme {
                    let ix = theme::Themes::global(cx)
                        .themes()
                        .iter()
                        .position(|t| t.name.eq_ignore_ascii_case(name));
                    match ix {
                        Some(ix) => theme::select(theme::ThemeChoice::Named(ix), window, cx),
                        None => eprintln!("unknown theme {name}"),
                    }
                }
                let app = cx.new(|cx| ChatApp::new(store.clone(), window, cx));
                if args.metrics {
                    window.on_next_frame(move |_, _| {
                        eprintln!("metrics: first frame {:?}", started.elapsed());
                        spawn_rss_sampler();
                    });
                }
                if let Some(id) = args.jump {
                    app.update(cx, |app, cx| app.jump_to(MessageId(id), cx));
                }
                if let Some(step) = args.bench_scroll {
                    let view = app.read(cx).conversation_view().clone();
                    bench::start(view, step, window, |cx| {
                        if let Some(kib) = rss_kib() {
                            eprintln!("bench: rss after full scroll {} MiB", kib / 1024);
                        }
                        cx.quit();
                    });
                }
                if args.feed {
                    let open = app.read(cx).conversation_view().read(cx).conversation();
                    store.update(cx, |store, cx| store.set_feed_target(Some(open), cx));
                }
                app
            })
            .expect("open window");
        });
}

/// Prints RSS every 10 s (Linux only).
fn spawn_rss_sampler() {
    std::thread::spawn(|| {
        loop {
            std::thread::sleep(Duration::from_secs(10));
            if let Some(kib) = rss_kib() {
                eprintln!("metrics: rss {} MiB", kib / 1024);
            }
        }
    });
}

fn rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|value| value.split_whitespace().next()?.parse().ok())
}
