//! Video playback spike: replaces a `RenderImage` per frame, as a player
//! would, and prints shown frames, frame gaps, CPU and RSS.
//!
//! `cargo run --release --example video_fps -- [width height fps seconds]`
//! (default 1280 720 30 20).

use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use gpui_kit::{
    AppContext as _, Bounds, Context, IntoElement, ObjectFit, ParentElement as _, Render,
    RenderImage, Styled as _, StyledImage as _, Window, WindowBounds, WindowOptions, div, img, px,
    size,
};
use image::{Frame, RgbaImage};

struct Player {
    current: Option<Arc<RenderImage>>,
    shown: u32,
    renders: Vec<Instant>,
}

impl Render for Player {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.renders.push(Instant::now());
        let mut root = div().size_full().bg(gpui_kit::black());
        if let Some(image) = &self.current {
            root = root.child(
                img(image.clone())
                    .size_full()
                    .object_fit(ObjectFit::Contain),
            );
        }
        root
    }
}

fn main() {
    let args: Vec<u32> = std::env::args()
        .skip(1)
        .filter_map(|a| a.parse().ok())
        .collect();
    let width = args.first().copied().unwrap_or(1280);
    let height = args.get(1).copied().unwrap_or(720);
    let fps = args.get(2).copied().unwrap_or(30);
    let seconds = args.get(3).copied().unwrap_or(20);
    let interval = Duration::from_secs_f64(1. / f64::from(fps));

    // Stands in for the helper's pipe: frames ready ahead, at most two.
    let (frames, received) = mpsc::sync_channel::<RgbaImage>(2);
    std::thread::spawn(move || {
        for n in 0.. {
            if frames.send(pattern(width, height, n)).is_err() {
                break;
            }
        }
    });

    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(1280.), px(760.)),
                cx,
            ))),
            app_id: Some("copiale-spike-video".into()),
            ..Default::default()
        };
        gpui_kit::open_window(options, cx, |window, cx| {
            let player = cx.new(|_| Player {
                current: None,
                shown: 0,
                renders: Vec::new(),
            });
            let weak = player.downgrade();
            let started = Instant::now();
            let cpu_start = cpu_ticks();
            window
                .spawn(cx, async move |cx| {
                    let mut next = Instant::now();
                    while started.elapsed() < Duration::from_secs(u64::from(seconds)) {
                        next += interval;
                        let wait = next.saturating_duration_since(Instant::now());
                        cx.background_executor().timer(wait).await;
                        let Ok(pixels) = received.recv() else { break };
                        let image = Arc::new(RenderImage::new([Frame::new(pixels)]));
                        let updated = cx.update(|window, cx| {
                            weak.update(cx, |player, cx| {
                                if let Some(old) = player.current.replace(image) {
                                    cx.drop_image(old, Some(window));
                                }
                                player.shown += 1;
                                cx.notify();
                            })
                        });
                        if !matches!(updated, Ok(Ok(()))) {
                            break;
                        }
                    }
                    let _ = cx.update(|_, cx| {
                        if let Some(player) = weak.upgrade() {
                            report(player.read(cx), started, cpu_start, fps);
                        }
                        cx.quit();
                    });
                })
                .detach();
            player
        })
        .expect("open window");
    });
}

/// A moving gradient with a sweeping bar, in BGRA.
fn pattern(width: u32, height: u32, n: u32) -> RgbaImage {
    let bar = (n * 8) % width;
    RgbaImage::from_fn(width, height, |x, y| {
        if x.abs_diff(bar) < 16 {
            return image::Rgba([255, 255, 255, 255]);
        }
        let b = ((x + n * 4) % 256) as u8;
        let g = ((y + n * 2) % 256) as u8;
        image::Rgba([b, g, 96, 255])
    })
}

fn report(player: &Player, started: Instant, cpu_start: Option<u64>, fps: u32) {
    let elapsed = started.elapsed().as_secs_f64();
    let mut gaps: Vec<f64> = player
        .renders
        .windows(2)
        .map(|w| (w[1] - w[0]).as_secs_f64() * 1000.)
        .collect();
    gaps.sort_by(f64::total_cmp);
    let pct = |p: f64| gaps.get(((gaps.len() as f64 - 1.) * p) as usize).copied();
    eprintln!(
        "frames: {} shown in {elapsed:.1}s ({:.1}/s, target {fps}); renders {}",
        player.shown,
        f64::from(player.shown) / elapsed,
        player.renders.len()
    );
    eprintln!(
        "render gaps ms: p50 {:?} p95 {:?} p99 {:?} max {:?}",
        pct(0.5),
        pct(0.95),
        pct(0.99),
        gaps.last()
    );
    if let (Some(start), Some(end)) = (cpu_start, cpu_ticks()) {
        // Clock ticks are 100 Hz on Linux.
        eprintln!("cpu: {:.0}% of one core", (end - start) as f64 / elapsed);
    }
    if let Some(kib) = rss_kib() {
        eprintln!("rss: {} MiB", kib / 1024);
    }
}

/// User plus system clock ticks of this process (Linux only).
fn cpu_ticks() -> Option<u64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let fields: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    Some(fields.get(11)?.parse::<u64>().ok()? + fields.get(12)?.parse::<u64>().ok()?)
}

fn rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|value| value.split_whitespace().next()?.parse().ok())
}
