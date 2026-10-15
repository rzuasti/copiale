//! A drawer that slides its content down from the top as it opens.

use gpui_kit::{
    AnyElement, App, AvailableSpace, Bounds, ContentMask, Element, ElementId, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, Pixels, Style, Window, point, px, relative, size,
};

/// Shows `progress` (0 to 1) of `child`'s height, keeping its bottom edge
/// at the drawer's: the content moves with the edge instead of being
/// uncovered.
pub fn drawer(id: impl Into<ElementId>, progress: f32, child: impl IntoElement) -> Drawer {
    Drawer {
        id: id.into(),
        progress: progress.clamp(0., 1.),
        child: child.into_any_element(),
    }
}

pub struct Drawer {
    id: ElementId,
    progress: f32,
    child: AnyElement,
}

impl IntoElement for Drawer {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for Drawer {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        // The content's height from the last frame; the first frame measures.
        let height = window.with_element_state(
            id.expect("Drawer has an id"),
            |height: Option<Option<Pixels>>, _| {
                let height = height.flatten();
                (height, height)
            },
        );
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = height
            .map_or(px(0.), |height| height * self.progress)
            .into();
        (window.request_layout(style, None, cx), ())
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let measured = self
            .child
            .layout_as_root(
                size(
                    AvailableSpace::Definite(bounds.size.width),
                    AvailableSpace::MinContent,
                ),
                window,
                cx,
            )
            .height;
        let changed = window.with_element_state(
            id.expect("Drawer has an id"),
            |height: Option<Option<Pixels>>, _| {
                (height.flatten() != Some(measured), Some(measured))
            },
        );
        if changed {
            window.request_animation_frame();
        }
        let origin = point(bounds.origin.x, bounds.bottom() - measured);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            self.child.prepaint_at(origin, window, cx);
        });
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            self.child.paint(window, cx);
        });
    }
}
