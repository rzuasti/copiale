//! Automated fling through the open conversation, recording frame times.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::{App, Entity, Pixels, Window, px};

use crate::conversation::ConversationView;

type Done = Box<dyn FnOnce(&mut App)>;

struct Run {
    view: Entity<ConversationView>,
    step: Pixels,
    started: Instant,
    last_frame: Instant,
    frames: Vec<Duration>,
    done: Option<Done>,
}

/// Scrolls up by `step` per frame until the oldest message is visible, then
/// prints frame-time percentiles and calls `done`.
pub fn start(
    view: Entity<ConversationView>,
    step: f32,
    window: &mut Window,
    done: impl FnOnce(&mut App) + 'static,
) {
    let now = Instant::now();
    let run = Rc::new(RefCell::new(Run {
        view,
        step: px(step),
        started: now,
        last_frame: now,
        frames: Vec::new(),
        done: Some(Box::new(done)),
    }));
    window.on_next_frame(move |window, cx| frame(run, window, cx));
}

fn frame(run: Rc<RefCell<Run>>, window: &mut Window, cx: &mut App) {
    let (view, step, finished) = {
        let mut state = run.borrow_mut();
        let now = Instant::now();
        let elapsed = now - state.last_frame;
        state.last_frame = now;
        state.frames.push(elapsed);
        let finished = state.view.read(cx).is_at_top();
        (state.view.clone(), state.step, finished)
    };

    if finished {
        report(&run.borrow());
        let done = run.borrow_mut().done.take();
        if let Some(done) = done {
            done(cx);
        }
        return;
    }
    view.update(cx, |view, cx| view.scroll_by(-step, cx));
    window.on_next_frame(move |window, cx| frame(run, window, cx));
}

fn report(run: &Run) {
    // The first sample spans startup, not a frame.
    let mut frames: Vec<f64> = run.frames[1..]
        .iter()
        .map(|d| d.as_secs_f64() * 1000.0)
        .collect();
    if frames.is_empty() {
        return;
    }
    frames.sort_by(f64::total_cmp);
    let pct = |p: f64| frames[((frames.len() - 1) as f64 * p).round() as usize];
    let slow = frames.iter().filter(|ms| **ms > 25.0).count();
    eprintln!(
        "bench: {} frames in {:.1?}; frame ms p50 {:.1} p95 {:.1} p99 {:.1} max {:.1}; {} frames > 25 ms",
        frames.len(),
        run.started.elapsed(),
        pct(0.5),
        pct(0.95),
        pct(0.99),
        pct(1.0),
        slow,
    );
}
