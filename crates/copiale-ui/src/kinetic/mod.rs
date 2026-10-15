//! Kinetic scrolling for a [`ListState`]: momentum, rubber band and bounce,
//! and easing when content at the end comes and goes.
//!
//! Wheel handling is off on macOS, which has native momentum. All of it is
//! off with reduced motion.

mod physics;

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::{
    AnyElement, App, Bounds, ContentMask, DispatchPhase, Element, ElementId, GlobalElementId,
    Hitbox, HitboxBehavior, InspectorElementId, IntoElement, LayoutId, ListState, Pixels,
    ScrollDelta, ScrollWheelEvent, Window, point, px,
};

use physics::{Extent, Physics};

/// Pixels per wheel line, as GPUI's list uses.
const LINE_HEIGHT: f32 = 20.;

const ENABLED: bool = !cfg!(target_os = "macos");

/// Motion state; keep one per scrolled list.
#[derive(Clone, Debug, Default)]
pub struct Kinetic(Rc<RefCell<State>>);

impl Kinetic {
    /// Stops any motion, as when the list's content is replaced.
    pub fn stop(&self) {
        let mut state = self.0.borrow_mut();
        state.physics.stop();
        state.carry = 0.;
    }

    /// Eases to the end of the list, e.g. to reveal a new last row.
    pub fn settle_to_end(&self, cx: &App) {
        let now = cx.background_executor().now();
        self.0.borrow_mut().physics.settle_to_end(now);
    }

    /// Draws the content `by` from where it now is and eases it back, so
    /// a jump looks like movement; negative is up.
    pub fn shift_by(&self, by: Pixels, cx: &App) {
        let now = cx.background_executor().now();
        self.0.borrow_mut().physics.shift_by(by.as_f32(), now);
    }
}

/// Whether to animate scrolling at all.
pub fn animates(cx: &App) -> bool {
    !cx.reduce_motion()
}

#[derive(Debug, Default)]
struct State {
    physics: Physics,
    /// Scrolling not yet applied, below a device pixel.
    carry: f32,
}

impl State {
    /// Scrolls `list` by whole device pixels, so text and lines move
    /// together without shimmering.
    fn scroll(&mut self, list: &ListState, distance: f32, scale: f32) {
        let applied;
        (applied, self.carry) = snap(self.carry + distance, scale);
        if applied != 0. {
            list.scroll_by(px(applied));
        }
    }
}

/// `distance` rounded to device pixels, and the remainder.
fn snap(distance: f32, scale: f32) -> (f32, f32) {
    let applied = (distance * scale).round() / scale;
    (applied, distance - applied)
}

/// Wraps the element showing `list` and takes over its wheel scrolling.
/// `padding`: the list's top plus bottom padding, which `ListState` leaves
/// out of its maximum offset.
pub fn kinetic(
    id: impl Into<ElementId>,
    kinetic: &Kinetic,
    list: &ListState,
    padding: Pixels,
    child: impl IntoElement,
) -> KineticScroll {
    KineticScroll {
        id: id.into(),
        state: kinetic.0.clone(),
        list: list.clone(),
        padding,
        child: child.into_any_element(),
    }
}

pub struct KineticScroll {
    id: ElementId,
    state: Rc<RefCell<State>>,
    list: ListState,
    padding: Pixels,
    child: AnyElement,
}

impl KineticScroll {
    fn extent(&self) -> Extent {
        extent(&self.list, self.padding)
    }
}

fn extent(list: &ListState, padding: Pixels) -> Extent {
    let viewport = list.viewport_bounds().size.height;
    let max = list.max_offset_for_scrollbar().y + padding;
    Extent {
        offset: -list.scroll_px_offset_for_scrollbar().y.as_f32(),
        max: if max > padding { max.as_f32() } else { 0. },
        viewport: viewport.as_f32(),
    }
}

impl IntoElement for KineticScroll {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for KineticScroll {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Hitbox {
        let scale = window.scale_factor();
        let offset = {
            let mut state = self.state.borrow_mut();
            if !animates(cx) {
                state.physics.stop();
            }
            let now = cx.background_executor().now();
            let distance = state.physics.step(now, self.extent());
            state.scroll(&self.list, distance, scale);
            if state.physics.is_animating() {
                window.request_animation_frame();
            }
            let offset = state.physics.shift() - state.physics.overscroll();
            (offset * scale).round() / scale
        };
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            window.with_element_offset(point(px(0.), px(offset)), |window| {
                self.child.prepaint(window, cx);
            });
        });
        hitbox
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        hitbox: &mut Hitbox,
        window: &mut Window,
        cx: &mut App,
    ) {
        if ENABLED && animates(cx) {
            let state = self.state.clone();
            let list = self.list.clone();
            let padding = self.padding;
            let hitbox = hitbox.id;
            let view = window.current_view();
            window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
                if phase != DispatchPhase::Capture || !hitbox.should_handle_scroll(window) {
                    return;
                }
                let now = cx.background_executor().now();
                let scale = window.scale_factor();
                let mut state = state.borrow_mut();
                // GPUI's deltas move content: positive scrolls towards the start.
                match event.delta {
                    ScrollDelta::Pixels(delta) => {
                        if delta.x.abs() > delta.y.abs() {
                            return;
                        }
                        let distance =
                            state
                                .physics
                                .drag(-delta.y.as_f32(), now, extent(&list, padding));
                        state.scroll(&list, distance, scale);
                    }
                    ScrollDelta::Lines(delta) => {
                        if delta.x.abs() > delta.y.abs() {
                            return;
                        }
                        state
                            .physics
                            .wheel(-delta.y * LINE_HEIGHT, now, extent(&list, padding));
                    }
                }
                cx.stop_propagation();
                cx.notify(view);
            });
        }
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            self.child.paint(window, cx);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapping_keeps_the_remainder() {
        let mut carry = 0.;
        let mut total = 0.;
        for _ in 0..10 {
            let applied;
            (applied, carry) = snap(carry + 0.3, 2.);
            assert_eq!((applied * 2.).fract(), 0.);
            total += applied;
        }
        assert!((total + carry - 3.).abs() < 1e-4);
    }
}
