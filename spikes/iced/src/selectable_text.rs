//! Plain text with drag selection, copy and clickable links.
//!
//! iced's `text` and `rich_text` cannot be selected. Paragraph hit tests
//! return an offset within a source line only, so each line gets its own
//! paragraph; the selection is drawn as highlighted spans.

use std::ops::Range;

use iced::advanced::Renderer as _;
use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::{self, Quad};
use iced::advanced::text::{Paragraph as _, Renderer as _, Span};
use iced::advanced::widget::{Tree, tree};
use iced::advanced::{Clipboard, Shell, Widget, clipboard, mouse, text};
use iced::alignment;
use iced::keyboard::{self, key};
use iced::{
    Background, Color, Element, Event, Length, Pixels, Point, Rectangle, Renderer, Size, Theme,
    Vector,
};

use crate::text::link_ranges;

type Paragraph = <Renderer as text::Renderer>::Paragraph;

pub struct SelectableText<'a, Message> {
    content: &'a str,
    size: Option<Pixels>,
    color: Color,
    link_color: Color,
    selection_color: Color,
    on_link: Option<Box<dyn Fn(String) -> Message + 'a>>,
}

impl<'a, Message> SelectableText<'a, Message> {
    pub fn new(content: &'a str) -> Self {
        Self {
            content,
            size: None,
            color: Color::BLACK,
            link_color: Color::from_rgb(0.2, 0.4, 0.9),
            selection_color: Color::from_rgba(0.3, 0.5, 1.0, 0.35),
            on_link: None,
        }
    }

    pub fn size(mut self, size: impl Into<Pixels>) -> Self {
        self.size = Some(size.into());
        self
    }

    pub fn color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }

    pub fn link_color(mut self, color: Color) -> Self {
        self.link_color = color;
        self
    }

    pub fn selection_color(mut self, color: Color) -> Self {
        self.selection_color = color;
        self
    }

    pub fn on_link(mut self, f: impl Fn(String) -> Message + 'a) -> Self {
        self.on_link = Some(Box::new(f));
        self
    }
}

struct Line {
    /// Byte range in the content.
    range: Range<usize>,
    paragraph: Paragraph,
    /// Links as `(range within line, span index)`.
    links: Vec<(Range<usize>, usize)>,
    selected_spans: Vec<usize>,
    y: f32,
    height: f32,
}

#[derive(Default)]
struct State {
    content: String,
    width: f32,
    lines: Vec<Line>,
    /// `(anchor, focus)` byte offsets.
    selection: Option<(usize, usize)>,
    dragging: bool,
    pressed_link: Option<String>,
}

impl State {
    fn selected(&self) -> Option<Range<usize>> {
        let (a, b) = self.selection?;
        (a != b).then(|| a.min(b)..a.max(b))
    }

    /// Byte offset under `point`, clamped into the text.
    fn offset_at(&self, point: Point) -> usize {
        let Some(line) = self
            .lines
            .iter()
            .find(|l| point.y < l.y + l.height)
            .or(self.lines.last())
        else {
            return 0;
        };
        if point.y < 0. {
            return 0;
        }
        let local = Point::new(point.x, point.y - line.y);
        let index = line
            .paragraph
            .hit_test(local)
            .map_or(line.range.len(), |hit| hit.cursor());
        line.range.start + index.min(line.range.len())
    }

    fn link_at(&self, content: &str, point: Point) -> Option<String> {
        let line = self
            .lines
            .iter()
            .find(|l| point.y >= l.y && point.y < l.y + l.height)?;
        let span = line
            .paragraph
            .hit_span(Point::new(point.x, point.y - line.y))?;
        let (range, _) = line.links.iter().find(|(_, s)| *s == span)?;
        Some(content[line.range.start + range.start..line.range.start + range.end].to_string())
    }
}

impl<Message> SelectableText<'_, Message> {
    /// Builds one paragraph per source line with link and selection spans.
    fn rebuild(&self, state: &mut State, renderer: &Renderer) {
        let size = self.size.unwrap_or_else(|| renderer.default_size());
        let font = renderer.default_font();
        let links = link_ranges(&state.content);
        let selected = state.selected();
        let mut lines = Vec::new();
        let mut start = 0;
        let mut y = 0.;
        for source in state.content.split('\n') {
            let range = start..start + source.len();
            start = range.end + 1;

            // Cut points: link edges and selection edges within this line.
            let mut cuts = vec![0, source.len()];
            for link in &links {
                for edge in [link.start, link.end] {
                    if range.contains(&edge) {
                        cuts.push(edge - range.start);
                    }
                }
            }
            if let Some(sel) = &selected {
                for edge in [sel.start, sel.end] {
                    if edge >= range.start && edge <= range.end {
                        cuts.push(edge - range.start);
                    }
                }
            }
            cuts.sort_unstable();
            cuts.dedup();

            let mut spans: Vec<Span<'_, (), iced::Font>> = Vec::new();
            let mut line_links = Vec::new();
            let mut selected_spans = Vec::new();
            for pair in cuts.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                if a == b {
                    continue;
                }
                let global = range.start + a..range.start + b;
                let is_link = links
                    .iter()
                    .any(|l| l.start <= global.start && global.end <= l.end);
                let is_selected = selected
                    .as_ref()
                    .is_some_and(|s| s.start <= global.start && global.end <= s.end);
                let mut span = Span::new(&source[a..b]).color(if is_link {
                    self.link_color
                } else {
                    self.color
                });
                if is_link {
                    span = span.underline(true).link(());
                    line_links.push((a..b, spans.len()));
                }
                if is_selected {
                    selected_spans.push(spans.len());
                }
                spans.push(span);
            }
            if spans.is_empty() {
                // Keep empty lines one line tall.
                spans.push(Span::new(" "));
            }

            let paragraph = Paragraph::with_spans(text::Text {
                content: spans.as_slice(),
                bounds: Size::new(state.width, f32::INFINITY),
                size,
                line_height: text::LineHeight::default(),
                font,
                align_x: text::Alignment::Default,
                align_y: alignment::Vertical::Top,
                shaping: text::Shaping::Advanced,
                wrapping: text::Wrapping::WordOrGlyph,
            });
            let height = paragraph.min_bounds().height;
            lines.push(Line {
                range,
                paragraph,
                links: line_links,
                selected_spans,
                y,
                height,
            });
            y += height;
        }
        state.lines = lines;
    }
}

impl<Message> Widget<Message, Theme, Renderer> for SelectableText<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Shrink, Length::Shrink)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let state = tree.state.downcast_mut::<State>();
        let width = limits.max().width;
        if state.content != self.content || state.width != width || state.lines.is_empty() {
            if state.content != self.content {
                state.selection = None;
            }
            state.content = self.content.to_string();
            state.width = width;
            self.rebuild(state, renderer);
        }
        let content_width = state
            .lines
            .iter()
            .map(|l| l.paragraph.min_bounds().width)
            .fold(0., f32::max);
        let height = state.lines.iter().map(|l| l.height).sum();
        layout::Node::new(limits.resolve(
            Length::Shrink,
            Length::Shrink,
            Size::new(content_width, height),
        ))
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        if !bounds.intersects(viewport) {
            return;
        }
        let state = tree.state.downcast_ref::<State>();
        for line in &state.lines {
            let origin = bounds.position() + Vector::new(0., line.y);
            for span in &line.selected_spans {
                for region in line.paragraph.span_bounds(*span) {
                    renderer.fill_quad(
                        Quad {
                            bounds: region + (origin - Point::ORIGIN),
                            ..Quad::default()
                        },
                        Background::Color(self.selection_color),
                    );
                }
            }
            for (_, span) in &line.links {
                for region in line.paragraph.span_bounds(*span) {
                    let size = self.size.unwrap_or_else(|| renderer.default_size()).0;
                    renderer.fill_quad(
                        Quad {
                            bounds: Rectangle::new(
                                origin + Vector::new(region.x, region.y + size * 1.15),
                                Size::new(region.width, 1.),
                            ),
                            ..Quad::default()
                        },
                        Background::Color(self.link_color),
                    );
                }
            }
            renderer.fill_paragraph(&line.paragraph, origin, self.color, *viewport);
        }
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
        let state = tree.state.downcast_mut::<State>();
        let local = |position: Point| position - Vector::new(bounds.x, bounds.y);

        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if let Some(position) = cursor.position_in(bounds) {
                    let offset = state.offset_at(position);
                    state.pressed_link = state.link_at(self.content, position);
                    state.selection = Some((offset, offset));
                    state.dragging = true;
                    self.rebuild(state, renderer);
                    shell.request_redraw();
                    shell.capture_event();
                } else if state.selection.take().is_some() {
                    self.rebuild(state, renderer);
                    shell.request_redraw();
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) if state.dragging => {
                let offset = state.offset_at(local(*position));
                if let Some((_, focus)) = state.selection.as_mut()
                    && *focus != offset
                {
                    *focus = offset;
                    self.rebuild(state, renderer);
                    shell.request_redraw();
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) if state.dragging => {
                state.dragging = false;
                if state.selected().is_none()
                    && let (Some(url), Some(on_link)) = (state.pressed_link.take(), &self.on_link)
                {
                    shell.publish(on_link(url));
                }
            }
            Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. })
                if modifiers.command() && *key == keyboard::Key::Character("c".into()) =>
            {
                if let Some(range) = state.selected() {
                    clipboard.write(clipboard::Kind::Standard, state.content[range].to_string());
                    shell.capture_event();
                }
            }
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(key::Named::Escape),
                ..
            }) if state.selection.take().is_some() => {
                self.rebuild(state, renderer);
                shell.request_redraw();
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
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        let Some(position) = cursor.position_in(layout.bounds()) else {
            return mouse::Interaction::None;
        };
        let state = tree.state.downcast_ref::<State>();
        if state.link_at(self.content, position).is_some() {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::Text
        }
    }
}

impl<'a, Message: 'a> From<SelectableText<'a, Message>> for Element<'a, Message> {
    fn from(text: SelectableText<'a, Message>) -> Self {
        Element::new(text)
    }
}
