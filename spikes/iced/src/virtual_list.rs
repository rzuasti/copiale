//! Virtualized list of variable-height rows, anchored to an item so that
//! inserting or measuring rows above the viewport never moves it.
//!
//! iced has no built-in equivalent: rows are built lazily during layout (like
//! `responsive`) and only the visible range, plus overdraw, exists per frame.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use iced::advanced::Renderer as _;
use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::{self, Quad};
use iced::advanced::widget::{self, Tree, tree};
use iced::advanced::{Clipboard, Shell, Widget, mouse};
use iced::{Background, Border, Element, Event, Length, Rectangle, Renderer, Size, Theme, window};

/// Extra height rendered above and below the viewport.
const OVERDRAW: f32 = 600.;
/// Height assumed for rows never measured.
const ESTIMATE: f32 = 80.;
const SCROLLBAR_WIDTH: f32 = 4.;
/// Pixels per wheel line.
const LINE_HEIGHT: f32 = 40.;

/// Scroll position shared between the app model and the widget.
#[derive(Clone, Default)]
pub struct ListHandle(Rc<RefCell<ListState>>);

impl std::fmt::Debug for ListHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ListHandle")
    }
}

#[derive(Default)]
struct ListState {
    /// Measured height per item; `None` until rendered at the current width.
    heights: Vec<Option<f32>>,
    width: f32,
    /// Topmost visible item and how far it is scrolled out of view.
    anchor_ix: usize,
    anchor_offset: f32,
    /// Stick to the end as items are appended.
    following: bool,
    visible: Range<usize>,
    published: Option<Range<usize>>,
}

impl ListState {
    fn height(&self, ix: usize) -> f32 {
        self.heights.get(ix).copied().flatten().unwrap_or(ESTIMATE)
    }

    fn count(&self) -> usize {
        self.heights.len()
    }

    /// Moves the anchor by `delta` pixels (positive scrolls towards the end).
    fn scroll(&mut self, delta: f32) {
        if delta < 0. {
            self.following = false;
        }
        let mut offset = self.anchor_offset + delta;
        let mut ix = self.anchor_ix;
        while offset < 0. && ix > 0 {
            ix -= 1;
            offset += self.height(ix);
        }
        while ix + 1 < self.count() && offset >= self.height(ix) {
            offset -= self.height(ix);
            ix += 1;
        }
        self.anchor_ix = ix;
        self.anchor_offset = offset.max(0.);
    }
}

impl ListHandle {
    /// Starts with `count` items, following the end.
    pub fn reset(&self, count: usize) {
        let mut state = self.0.borrow_mut();
        state.heights = vec![None; count];
        state.anchor_ix = count.saturating_sub(1);
        state.anchor_offset = 0.;
        state.following = true;
        state.published = None;
    }

    /// Replaces `range` with `count` unmeasured items, keeping the anchor on
    /// the same item.
    pub fn splice(&self, range: Range<usize>, count: usize) {
        let mut state = self.0.borrow_mut();
        let removed = range.len();
        if state.anchor_ix >= range.end {
            state.anchor_ix = state.anchor_ix + count - removed;
        } else if state.anchor_ix >= range.start {
            state.anchor_ix = range.start;
            state.anchor_offset = 0.;
        }
        state
            .heights
            .splice(range, std::iter::repeat_n(None, count));
    }

    pub fn count(&self) -> usize {
        self.0.borrow().count()
    }

    /// Puts item `ix` at the top of the viewport.
    pub fn scroll_to(&self, ix: usize) {
        let mut state = self.0.borrow_mut();
        state.anchor_ix = ix.min(state.count().saturating_sub(1));
        state.anchor_offset = 0.;
        state.following = false;
    }

    pub fn scroll_to_end(&self) {
        let mut state = self.0.borrow_mut();
        state.following = true;
        state.anchor_ix = state.count().saturating_sub(1);
    }

    pub fn scroll_by(&self, delta: f32) {
        self.0.borrow_mut().scroll(delta);
    }

    /// Index of the topmost visible item.
    pub fn top(&self) -> usize {
        self.0.borrow().anchor_ix
    }

    pub fn is_at_top(&self) -> bool {
        let state = self.0.borrow();
        state.anchor_ix == 0 && state.anchor_offset <= 0.
    }

    pub fn is_following(&self) -> bool {
        self.0.borrow().following
    }
}

type ViewFn<'a, Message> = Box<dyn Fn(usize) -> Element<'a, Message> + 'a>;

pub struct VirtualList<'a, Message> {
    handle: ListHandle,
    key: Box<dyn Fn(usize) -> u64 + 'a>,
    view: ViewFn<'a, Message>,
    on_visible: Option<Box<dyn Fn(Range<usize>) -> Message + 'a>>,
    scrollbar_color: iced::Color,
    /// Rows built by the last layout: `(index, key, element)`.
    rows: Vec<(usize, u64, Element<'a, Message>)>,
}

/// Per-row widget trees, keyed so they survive index shifts.
#[derive(Default)]
struct Rows(HashMap<u64, Tree>);

impl<'a, Message> VirtualList<'a, Message> {
    pub fn new(
        handle: &ListHandle,
        key: impl Fn(usize) -> u64 + 'a,
        view: impl Fn(usize) -> Element<'a, Message> + 'a,
    ) -> Self {
        Self {
            handle: handle.clone(),
            key: Box::new(key),
            view: Box::new(view),
            on_visible: None,
            scrollbar_color: iced::Color::TRANSPARENT,
            rows: Vec::new(),
        }
    }

    /// Reports the visible item range after it changes.
    pub fn on_visible(mut self, f: impl Fn(Range<usize>) -> Message + 'a) -> Self {
        self.on_visible = Some(Box::new(f));
        self
    }

    pub fn scrollbar_color(mut self, color: iced::Color) -> Self {
        self.scrollbar_color = color;
        self
    }

    fn build(
        &self,
        rows: &mut Rows,
        ix: usize,
        width: f32,
        renderer: &Renderer,
    ) -> Row<'a, Message> {
        let key = (self.key)(ix);
        let mut element = (self.view)(ix);
        let tree = rows.0.entry(key).or_insert_with(|| Tree::new(&element));
        tree.diff(&element);
        let limits = layout::Limits::new(Size::ZERO, Size::new(width, f32::INFINITY));
        let node = element.as_widget_mut().layout(tree, renderer, &limits);
        Row {
            ix,
            key,
            element,
            node,
            y: 0.,
        }
    }
}

struct Row<'a, Message> {
    ix: usize,
    key: u64,
    element: Element<'a, Message>,
    node: layout::Node,
    y: f32,
}

impl<Message> Widget<Message, Theme, Renderer> for VirtualList<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Rows>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(Rows::default())
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn diff(&self, _tree: &mut Tree) {
        // Rows are diffed during layout.
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let size = limits.max();
        let rows = tree.state.downcast_mut::<Rows>();
        let handle = self.handle.clone();
        let mut state = handle.0.borrow_mut();
        self.rows.clear();

        if state.width != size.width {
            state.width = size.width;
            state.heights.iter_mut().for_each(|h| *h = None);
        }
        let count = state.count();
        if count == 0 {
            rows.0.clear();
            state.visible = 0..0;
            return layout::Node::new(size);
        }
        if state.following {
            state.anchor_ix = count - 1;
            state.anchor_offset = 0.;
        }

        // Lay out from the anchor down, then extend upwards.
        let mut below: Vec<Row<'_, Message>> = Vec::new();
        let mut y = -state.anchor_offset;
        let mut ix = state.anchor_ix;
        while ix < count && y < size.height + OVERDRAW {
            let mut row = self.build(rows, ix, size.width, renderer);
            let h = row.node.size().height;
            state.heights[ix] = Some(h);
            row.y = y;
            below.push(row);
            y += h;
            ix += 1;
        }
        // Pin the end to the bottom rather than leave a gap below it.
        let mut shift = if ix == count && y < size.height {
            size.height - y
        } else {
            0.
        };

        let mut above: Vec<Row<'_, Message>> = Vec::new();
        let mut top = -state.anchor_offset;
        let mut ix = state.anchor_ix;
        while ix > 0 && top + shift > -OVERDRAW {
            ix -= 1;
            let mut row = self.build(rows, ix, size.width, renderer);
            let h = row.node.size().height;
            state.heights[ix] = Some(h);
            top -= h;
            row.y = top;
            above.push(row);
        }
        // Content shorter than the viewport starts at the top.
        if ix == 0 && top + shift > 0. {
            shift = -top;
        }

        let mut nodes = Vec::with_capacity(above.len() + below.len());
        let mut visible: Option<Range<usize>> = None;
        let mut anchor = None;
        let mut end_visible = false;
        for row in above.into_iter().rev().chain(below) {
            let y = row.y + shift;
            let h = row.node.size().height;
            if y + h > 0. && y < size.height {
                visible = Some(visible.map_or(row.ix..row.ix + 1, |v| v.start..row.ix + 1));
                anchor.get_or_insert((row.ix, -y));
                end_visible = row.ix + 1 == count && y + h <= size.height + 0.5;
            }
            nodes.push(row.node.move_to((0., y)));
            self.rows.push((row.ix, row.key, row.element));
        }
        if let Some((ix, offset)) = anchor {
            state.anchor_ix = ix;
            state.anchor_offset = offset.max(0.);
        }
        // Scrolled to the end: resume following.
        if end_visible {
            state.following = true;
        }
        state.visible = visible.unwrap_or(0..0);

        let keys: Vec<u64> = self.rows.iter().map(|(_, key, _)| *key).collect();
        rows.0.retain(|key, _| keys.contains(key));

        layout::Node::with_children(size, nodes)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let rows = tree.state.downcast_ref::<Rows>();
        renderer.with_layer(bounds, |renderer| {
            for ((_, key, element), child) in self.rows.iter().zip(layout.children()) {
                if !child.bounds().intersects(&bounds) {
                    continue;
                }
                element.as_widget().draw(
                    &rows.0[key],
                    renderer,
                    theme,
                    style,
                    child,
                    cursor,
                    &bounds,
                );
            }
        });
        self.draw_scrollbar(renderer, bounds);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let rows = tree.state.downcast_mut::<Rows>();
        // Rows extend past the viewport; presses there belong to other widgets.
        let child_cursor = match event {
            Event::Mouse(mouse::Event::ButtonPressed(_)) if !cursor.is_over(bounds) => {
                mouse::Cursor::Unavailable
            }
            _ => cursor,
        };
        for ((_, key, element), child) in self.rows.iter_mut().zip(layout.children()) {
            if let Some(tree) = rows.0.get_mut(key) {
                element.as_widget_mut().update(
                    tree,
                    event,
                    child,
                    child_cursor,
                    renderer,
                    clipboard,
                    shell,
                    &bounds,
                );
            }
        }
        if shell.is_event_captured() {
            return;
        }

        match event {
            Event::Mouse(mouse::Event::WheelScrolled { delta }) if cursor.is_over(bounds) => {
                let dy = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => y * LINE_HEIGHT,
                    mouse::ScrollDelta::Pixels { y, .. } => *y,
                };
                self.handle.0.borrow_mut().scroll(-dy);
                shell.invalidate_layout();
                shell.request_redraw();
                shell.capture_event();
            }
            Event::Window(window::Event::RedrawRequested(_)) => {
                let mut state = self.handle.0.borrow_mut();
                if state.published.as_ref() != Some(&state.visible) {
                    state.published = Some(state.visible.clone());
                    if let Some(on_visible) = &self.on_visible {
                        shell.publish(on_visible(state.visible.clone()));
                    }
                }
            }
            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        let bounds = layout.bounds();
        if !cursor.is_over(bounds) {
            return mouse::Interaction::None;
        }
        let rows = tree.state.downcast_ref::<Rows>();
        self.rows
            .iter()
            .zip(layout.children())
            .map(|((_, key, element), child)| {
                element.as_widget().mouse_interaction(
                    &rows.0[key],
                    child,
                    cursor,
                    &bounds,
                    renderer,
                )
            })
            .max()
            .unwrap_or_default()
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn widget::Operation,
    ) {
        let rows = tree.state.downcast_mut::<Rows>();
        for ((_, key, element), child) in self.rows.iter_mut().zip(layout.children()) {
            if let Some(tree) = rows.0.get_mut(key) {
                element
                    .as_widget_mut()
                    .operate(tree, child, renderer, operation);
            }
        }
    }
}

impl<Message> VirtualList<'_, Message> {
    /// Thumb sized and placed from measured heights plus estimates.
    fn draw_scrollbar(&self, renderer: &mut Renderer, bounds: Rectangle) {
        let state = self.handle.0.borrow();
        let total: f32 = (0..state.count()).map(|ix| state.height(ix)).sum();
        if total <= bounds.height {
            return;
        }
        let above: f32 =
            (0..state.anchor_ix).map(|ix| state.height(ix)).sum::<f32>() + state.anchor_offset;
        let thumb = (bounds.height * bounds.height / total).max(24.);
        let y = (above / (total - bounds.height)).clamp(0., 1.) * (bounds.height - thumb);
        renderer.fill_quad(
            Quad {
                bounds: Rectangle {
                    x: bounds.x + bounds.width - SCROLLBAR_WIDTH - 2.,
                    y: bounds.y + y,
                    width: SCROLLBAR_WIDTH,
                    height: thumb,
                },
                border: Border::default().rounded(SCROLLBAR_WIDTH / 2.),
                ..Quad::default()
            },
            Background::Color(self.scrollbar_color),
        );
    }
}

impl<'a, Message: 'a> From<VirtualList<'a, Message>> for Element<'a, Message> {
    fn from(list: VirtualList<'a, Message>) -> Self {
        Element::new(list)
    }
}
