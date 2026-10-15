//! iOS-style scroll physics: 1:1 tracking, momentum after release, rubber
//! band past the edges and a spring back.
//!
//! Offsets grow towards the end of the content.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Momentum decay after a touchpad fling, per second: `UIScrollView`'s
/// normal deceleration (0.998 per millisecond).
const FLING_DECAY: f32 = 2.002;
/// Momentum decay after wheel notches: `UIScrollView`'s fast deceleration
/// (0.99 per millisecond), so a notch settles in about 300 ms.
const WHEEL_DECAY: f32 = 10.05;
/// Touchpad silence for this many event intervals means the fingers lifted;
/// GPUI doesn't report it on Linux. Kept short: content stands still until
/// momentum takes over.
const LIFT_INTERVALS: f32 = 2.5;
const MIN_LIFT_GAP: f32 = 0.02;
const MAX_LIFT_GAP: f32 = 0.05;
/// Input within this long before the last one sets the release velocity.
const VELOCITY_WINDOW: Duration = Duration::from_millis(60);
/// Slower releases stop instead of coasting, px/s.
const MIN_FLING: f32 = 60.;
/// Coasting stops below this, px/s.
const MIN_VELOCITY: f32 = 8.;
/// Fastest edge impact a bounce takes, px/s; caps how far it stretches.
const MAX_BOUNCE: f32 = 2500.;
/// Rubber band stiffness: `UIScrollView`'s 0.55.
const RUBBER_BAND: f32 = 0.55;
/// Undamped angular frequencies of the critically damped bounce, rad/s: a
/// stiff stop past the edge, then an unhurried return (about 600 ms).
const SPRING_OUT: f32 = 24.;
const SPRING_BACK: f32 = 10.5;
/// How fast settling to the end and shifts ease out, per second: most of
/// the way in about 250 ms.
const SETTLE: f32 = 12.;
/// Settling and shifts stop within this many pixels.
const SETTLED: f32 = 0.5;
/// Settling runs at least this long: new content at the end is measured a
/// frame after it's added.
const SETTLE_GRACE: Duration = Duration::from_millis(50);
/// Longest step simulated at once, so a stalled frame doesn't jump.
const MAX_STEP: f32 = 1. / 30.;

/// Scroll position and limits, px.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Extent {
    pub offset: f32,
    pub max: f32,
    pub viewport: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    Idle,
    /// Fingers on the touchpad; `stretch` is their travel past an edge.
    Tracking {
        stretch: f32,
    },
    Coasting {
        velocity: f32,
        decay: f32,
    },
    /// Springing back to the edge.
    Bouncing {
        velocity: f32,
    },
    /// Easing to the end, e.g. to reveal new content there.
    Settling {
        since: Instant,
    },
}

#[derive(Debug)]
pub struct Physics {
    mode: Mode,
    /// Displacement past an edge: negative before the start, positive after
    /// the end.
    overscroll: f32,
    /// Offset the content is drawn at, easing to 0: positive below where
    /// it is.
    shift: f32,
    /// Recent touchpad deltas.
    samples: VecDeque<(Instant, f32)>,
    /// Typical time between touchpad events, s.
    interval: Option<f32>,
    stepped_at: Option<Instant>,
}

impl Default for Physics {
    fn default() -> Self {
        Self {
            mode: Mode::Idle,
            overscroll: 0.,
            shift: 0.,
            samples: VecDeque::new(),
            interval: None,
            stepped_at: None,
        }
    }
}

impl Physics {
    pub fn overscroll(&self) -> f32 {
        self.overscroll
    }

    pub fn shift(&self) -> f32 {
        self.shift
    }

    pub fn is_animating(&self) -> bool {
        self.mode != Mode::Idle || self.shift != 0.
    }

    /// Eases to the end, unless the user is scrolling.
    pub fn settle_to_end(&mut self, now: Instant) {
        match self.mode {
            Mode::Idle => {
                self.stepped_at = Some(now);
                self.mode = Mode::Settling { since: now };
            }
            Mode::Settling { .. } | Mode::Coasting { .. } => {
                self.mode = Mode::Settling { since: now };
            }
            Mode::Tracking { .. } | Mode::Bouncing { .. } => {}
        }
    }

    /// Draws the content `by` away from where it is, then eases it back:
    /// content that jumped looks like it moves.
    pub fn shift_by(&mut self, by: f32, now: Instant) {
        if !self.is_animating() {
            self.stepped_at = Some(now);
        }
        self.shift += by;
    }

    /// Stops all motion and snaps back to the edge.
    pub fn stop(&mut self) {
        *self = Self::default();
    }

    /// Touchpad movement of `delta`; returns how far to scroll.
    pub fn drag(&mut self, delta: f32, now: Instant, extent: Extent) -> f32 {
        let stretch = match self.mode {
            Mode::Tracking { stretch } => stretch,
            // Catching a bounce keeps hold of the stretched edge.
            _ => unrubber(self.overscroll, extent.viewport),
        };
        if let Some((last, _)) = self.samples.back() {
            let gap = now.duration_since(*last).as_secs_f32();
            if gap < MAX_LIFT_GAP {
                self.interval = Some(self.interval.map_or(gap, |i| i * 0.8 + gap * 0.2));
            }
        }
        self.samples.push_back((now, delta));
        while self
            .samples
            .front()
            .is_some_and(|(at, _)| now.duration_since(*at) > VELOCITY_WINDOW)
        {
            self.samples.pop_front();
        }
        self.stepped_at = Some(now);

        let target = extent.offset + stretch + delta;
        let offset = target.clamp(0., extent.max.max(0.));
        let stretch = target - offset;
        self.overscroll = rubber(stretch, extent.viewport);
        self.mode = Mode::Tracking { stretch };
        offset - extent.offset
    }

    /// A wheel movement of `delta`: an impulse that coasts about that far.
    pub fn wheel(&mut self, delta: f32, now: Instant, extent: Extent) {
        let impulse = delta * WHEEL_DECAY;
        let at_edge =
            (delta < 0. && extent.offset <= 0.) || (delta > 0. && extent.offset >= extent.max);
        if self.mode == Mode::Idle {
            self.stepped_at = Some(now);
        }
        self.mode = match self.mode {
            Mode::Bouncing { .. } | Mode::Tracking { .. } => return,
            _ if at_edge => Mode::Bouncing {
                velocity: impulse.clamp(-MAX_BOUNCE, MAX_BOUNCE),
            },
            Mode::Coasting { velocity, .. } if velocity.signum() == delta.signum() => {
                Mode::Coasting {
                    velocity: velocity + impulse,
                    decay: WHEEL_DECAY,
                }
            }
            _ => Mode::Coasting {
                velocity: impulse,
                decay: WHEEL_DECAY,
            },
        };
    }

    /// Advances to `now`; returns how far to scroll.
    pub fn step(&mut self, now: Instant, extent: Extent) -> f32 {
        let dt = self
            .stepped_at
            .replace(now)
            .map_or(0., |at| now.duration_since(at).as_secs_f32())
            .min(MAX_STEP);
        self.shift *= (-SETTLE * dt).exp();
        if self.shift.abs() < SETTLED {
            self.shift = 0.;
        }
        match self.mode {
            Mode::Idle => 0.,
            Mode::Settling { since } => {
                let remaining = extent.max.max(0.) - extent.offset;
                if remaining.abs() < SETTLED && now.duration_since(since) >= SETTLE_GRACE {
                    self.mode = Mode::Idle;
                    remaining
                } else {
                    remaining * (1. - (-SETTLE * dt).exp())
                }
            }
            Mode::Tracking { stretch } => {
                let gap = self.interval.map_or(MAX_LIFT_GAP, |interval| {
                    (interval * LIFT_INTERVALS).clamp(MIN_LIFT_GAP, MAX_LIFT_GAP)
                });
                let lifted = self
                    .samples
                    .back()
                    .is_none_or(|(at, _)| now.duration_since(*at).as_secs_f32() >= gap);
                if lifted {
                    self.release(stretch);
                }
                0.
            }
            Mode::Coasting { velocity, decay } => {
                let fade = (-decay * dt).exp();
                let distance = velocity / decay * (1. - fade);
                let velocity = velocity * fade;
                let target = extent.offset + distance;
                let max = extent.max.max(0.);
                if target < 0. || target > max {
                    let edge = target.clamp(0., max);
                    self.mode = Mode::Bouncing {
                        velocity: velocity.clamp(-MAX_BOUNCE, MAX_BOUNCE),
                    };
                    return edge - extent.offset;
                }
                if velocity.abs() < MIN_VELOCITY {
                    self.mode = Mode::Idle;
                } else {
                    self.mode = Mode::Coasting { velocity, decay };
                }
                distance
            }
            Mode::Bouncing { velocity } => {
                // Critically damped spring towards 0.
                let x = self.overscroll;
                let outward = if x == 0. {
                    velocity != 0.
                } else {
                    x * velocity > 0.
                };
                let w = if outward { SPRING_OUT } else { SPRING_BACK };
                let fade = (-w * dt).exp();
                let c = velocity + w * x;
                let x = (x + c * dt) * fade;
                let velocity = (velocity - w * c * dt) * fade;
                if x.abs() < 0.5 && velocity.abs() < 20. {
                    self.overscroll = 0.;
                    self.mode = Mode::Idle;
                } else {
                    self.overscroll = x;
                    self.mode = Mode::Bouncing { velocity };
                }
                0.
            }
        }
    }

    fn release(&mut self, stretch: f32) {
        let velocity = self.release_velocity();
        self.samples.clear();
        self.mode = if stretch != 0. {
            Mode::Bouncing { velocity: 0. }
        } else if velocity.abs() >= MIN_FLING {
            Mode::Coasting {
                velocity,
                decay: FLING_DECAY,
            }
        } else {
            Mode::Idle
        };
    }

    /// Average speed over the recent samples, px/s.
    fn release_velocity(&self) -> f32 {
        let (Some((first, _)), Some((last, _))) = (self.samples.front(), self.samples.back())
        else {
            return 0.;
        };
        // The first sample's movement happened before it arrived; count its
        // interval as one typical event gap.
        let span = last.duration_since(*first).as_secs_f32();
        let count = self.samples.len() as f32;
        let span = if count > 1. {
            span * count / (count - 1.)
        } else {
            return 0.;
        };
        let distance: f32 = self.samples.iter().map(|(_, delta)| delta).sum();
        distance / span
    }
}

/// Visible displacement for `stretch` past an edge: follows at first, then
/// resists, never reaching `viewport`.
fn rubber(stretch: f32, viewport: f32) -> f32 {
    if viewport <= 0. {
        return 0.;
    }
    let x = stretch.abs();
    (1. - 1. / (x * RUBBER_BAND / viewport + 1.)) * viewport * stretch.signum()
}

/// Inverse of [`rubber`].
fn unrubber(displacement: f32, viewport: f32) -> f32 {
    if viewport <= 0. || displacement == 0. {
        return 0.;
    }
    let y = displacement.abs().min(viewport * 0.99);
    (viewport / RUBBER_BAND) * (1. / (1. - y / viewport) - 1.) * displacement.signum()
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXTENT: Extent = Extent {
        offset: 500.,
        max: 2000.,
        viewport: 600.,
    };

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Steps at 60 Hz until idle (or `limit`); returns total scroll and the
    /// largest overscroll seen.
    fn settle(physics: &mut Physics, mut now: Instant, mut extent: Extent) -> (f32, f32, Instant) {
        let mut peak = 0f32;
        for _ in 0..600 {
            now += ms(16);
            extent.offset += physics.step(now, extent);
            peak = if physics.overscroll().abs() > peak.abs() {
                physics.overscroll()
            } else {
                peak
            };
            if !physics.is_animating() {
                break;
            }
        }
        (extent.offset, peak, now)
    }

    /// Drags `count` events of `delta` every 8 ms; returns the new extent.
    fn swipe(
        physics: &mut Physics,
        now: &mut Instant,
        mut extent: Extent,
        delta: f32,
        count: u32,
    ) -> Extent {
        for _ in 0..count {
            *now += ms(8);
            extent.offset += physics.drag(delta, *now, extent);
        }
        extent
    }

    #[test]
    fn drag_tracks_one_to_one() {
        let mut physics = Physics::default();
        let now = Instant::now();
        assert_eq!(physics.drag(12., now, EXTENT), 12.);
        assert_eq!(physics.drag(-30., now, EXTENT), -30.);
    }

    #[test]
    fn slow_release_stops_and_fling_coasts_then_stops() {
        let mut physics = Physics::default();
        let mut now = Instant::now();
        let extent = swipe(&mut physics, &mut now, EXTENT, 0.2, 5);
        let (offset, ..) = settle(&mut physics, now, extent);
        assert_eq!(offset, extent.offset);

        let mut physics = Physics::default();
        let mut now = Instant::now();
        // 20 px per 8 ms: 2500 px/s.
        let extent = swipe(&mut physics, &mut now, EXTENT, 20., 5);
        let (offset, _, end) = settle(&mut physics, now, extent);
        assert!(offset > extent.offset + 300., "coasted to {offset}");
        assert!(offset < EXTENT.max);
        assert!(!physics.is_animating());
        assert!(end.duration_since(now) > ms(500), "decelerates gradually");
    }

    #[test]
    fn fling_into_the_end_bounces_back() {
        let mut physics = Physics::default();
        let mut now = Instant::now();
        let near_end = Extent {
            offset: 1900.,
            ..EXTENT
        };
        let extent = swipe(&mut physics, &mut now, near_end, 20., 4);
        let (offset, peak, _) = settle(&mut physics, now, extent);
        assert_eq!(offset, EXTENT.max);
        assert!(peak > 10. && peak < 150., "bumped {peak} px past the end");
        assert_eq!(physics.overscroll(), 0.);
        assert!(!physics.is_animating());
    }

    #[test]
    fn dragging_past_the_start_resists_then_springs_back() {
        let mut physics = Physics::default();
        let mut now = Instant::now();
        let at_start = Extent {
            offset: 0.,
            ..EXTENT
        };
        let extent = swipe(&mut physics, &mut now, at_start, -50., 6);
        assert_eq!(extent.offset, 0.);
        let pulled = physics.overscroll();
        assert!(
            pulled < 0. && pulled > -300.,
            "stretched {pulled} for 300 px of travel"
        );
        assert!(pulled.abs() < at_start.viewport);

        // Reversing first takes back the stretch.
        assert_eq!(physics.drag(10., now, extent), 0.);

        let (offset, _, _) = settle(&mut physics, now, extent);
        assert_eq!(offset, 0.);
        assert_eq!(physics.overscroll(), 0.);
    }

    #[test]
    fn wheel_notches_coast_their_distance_and_accelerate() {
        let mut physics = Physics::default();
        let now = Instant::now();
        physics.wheel(60., now, EXTENT);
        let (one, ..) = settle(&mut physics, now, EXTENT);
        assert!(
            (one - EXTENT.offset - 60.).abs() < 6.,
            "one notch: {}",
            one - EXTENT.offset
        );

        let mut physics = Physics::default();
        let mut extent = EXTENT;
        let mut now = Instant::now();
        for _ in 0..3 {
            physics.wheel(60., now, extent);
            now += ms(16);
            extent.offset += physics.step(now, extent);
        }
        let (three, ..) = settle(&mut physics, now, extent);
        assert!(three - EXTENT.offset > 150.);
    }

    #[test]
    fn wheel_reversal_drops_momentum() {
        let mut physics = Physics::default();
        let now = Instant::now();
        physics.wheel(600., now, EXTENT);
        physics.wheel(-60., now, EXTENT);
        let (offset, ..) = settle(&mut physics, now, EXTENT);
        assert!(offset < EXTENT.offset);
    }

    #[test]
    fn wheel_at_an_edge_bumps() {
        let mut physics = Physics::default();
        let now = Instant::now();
        let at_end = Extent {
            offset: EXTENT.max,
            ..EXTENT
        };
        physics.wheel(60., now, at_end);
        let (offset, peak, _) = settle(&mut physics, now, at_end);
        assert_eq!(offset, EXTENT.max);
        assert!(peak > 2., "bumped {peak} px");
        assert_eq!(physics.overscroll(), 0.);
    }

    #[test]
    fn touching_stops_coasting() {
        let mut physics = Physics::default();
        let mut now = Instant::now();
        let mut extent = swipe(&mut physics, &mut now, EXTENT, 20., 5);
        now += ms(100);
        extent.offset += physics.step(now, extent);
        assert!(physics.is_animating());
        extent.offset += physics.drag(0., now, extent);
        let (offset, ..) = settle(&mut physics, now, extent);
        assert_eq!(offset, extent.offset);
    }

    #[test]
    fn release_is_detected_within_a_few_event_intervals() {
        let mut physics = Physics::default();
        let mut now = Instant::now();
        let mut extent = swipe(&mut physics, &mut now, EXTENT, 20., 6);
        extent.offset += physics.step(now + ms(12), extent);
        assert_eq!(physics.step(now + ms(16), extent), 0., "still touching");
        extent.offset += physics.step(now + ms(24), extent);
        assert!(physics.step(now + ms(40), extent) > 10., "coasting");
    }

    #[test]
    fn bounce_returns_unhurried() {
        let mut physics = Physics::default();
        let mut now = Instant::now();
        let at_start = Extent {
            offset: 0.,
            ..EXTENT
        };
        let extent = swipe(&mut physics, &mut now, at_start, -40., 5);
        let lift = now + ms(60);
        physics.step(lift, extent);
        let (_, _, settled) = settle(&mut physics, lift, extent);
        let back = settled.duration_since(lift);
        assert!(back > ms(450) && back < ms(1200), "returned in {back:?}");
    }

    #[test]
    fn settling_eases_to_the_end() {
        let mut physics = Physics::default();
        let now = Instant::now();
        physics.settle_to_end(now);
        let mut extent = EXTENT;
        let first = physics.step(now + ms(16), extent);
        assert!(first > 0. && first < 500., "eases in: {first}");
        extent.offset += first;
        let (offset, _, end) = settle(&mut physics, now + ms(16), extent);
        assert_eq!(offset, EXTENT.max);
        assert!(end.duration_since(now) < ms(1000));
    }

    #[test]
    fn dragging_interrupts_settling() {
        let mut physics = Physics::default();
        let now = Instant::now();
        physics.settle_to_end(now);
        physics.drag(-10., now, EXTENT);
        physics.settle_to_end(now);
        assert_eq!(physics.step(now + ms(5), EXTENT), 0., "still the user's");
    }

    #[test]
    fn shifts_ease_back_to_zero() {
        let mut physics = Physics::default();
        let now = Instant::now();
        physics.shift_by(-40., now);
        assert_eq!(physics.shift(), -40.);
        assert!(physics.is_animating());

        physics.step(now + ms(16), EXTENT);
        let after_one_frame = physics.shift();
        assert!(after_one_frame > -40. && after_one_frame < 0.);
        let (offset, _, end) = settle(&mut physics, now + ms(16), EXTENT);
        assert_eq!(offset, EXTENT.offset, "doesn't scroll");
        assert_eq!(physics.shift(), 0.);
        assert!(end.duration_since(now) < ms(1000));
    }

    #[test]
    fn rubber_band_inverts() {
        for stretch in [-900., -40., 0., 3., 250.] {
            let back = unrubber(rubber(stretch, 600.), 600.);
            assert!((back - stretch).abs() < 0.01, "{stretch} → {back}");
        }
    }
}
