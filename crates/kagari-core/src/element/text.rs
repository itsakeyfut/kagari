//! The [`Text`] leaf element (#34): shaped at build, rasterized at paint.

use kagari_base::{Color, NodeId, Px, Rect, SharedString};
use kagari_layout::LayoutStyle;
use kagari_text::{ShapedText, TextStyle, fontdb};

use super::{AnyElement, Element, Event, EventCx, IntoElement, LayoutCx, PaintCx};
use crate::arena::Node;

/// A text leaf. Shapes its content once at `request_layout` (caching the result and reporting its
/// intrinsic size via a [`MeasureFn`](kagari_layout::MeasureFn)), and rasterizes the cached
/// shaping into glyph sprites at `paint`.
pub struct Text {
    text: SharedString,
    style: TextStyle,
    color: Color,
    id: Option<NodeId>,
    shaped: Option<ShapedText>,
}

/// Creates a text leaf with the default style (bundled "Noto Sans", 16px) and a light color.
pub fn text(content: impl Into<SharedString>) -> Text {
    Text {
        text: content.into(),
        style: TextStyle {
            family: "Noto Sans".into(),
            size: Px(16.0),
            weight: fontdb::Weight::NORMAL,
            line_height: None,
        },
        color: Color::new(0.9, 0.9, 0.9, 1.0),
        id: None,
        shaped: None,
    }
}

impl Text {
    /// Sets the glyph color.
    pub fn color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }

    /// Sets the font size.
    pub fn size(mut self, size: Px) -> Self {
        self.style.size = size;
        self
    }
}

impl Element for Text {
    fn request_layout(&mut self, cx: &mut LayoutCx) -> NodeId {
        if let Some(id) = self.id {
            return id;
        }
        let id = cx.arena.insert(Node::default());
        self.id = Some(id);
        cx.layout.insert(id, None);

        // Shape once at build: cache the result, report its intrinsic size as the measure (so
        // `compute` needs no TextSystem), and reuse the cached shaping at paint.
        let shaped = cx.text.shape(&self.text, &self.style, None);
        let size = shaped.size;
        self.shaped = Some(shaped);
        cx.layout.set_measure(id, Box::new(move |_avail| size));
        cx.layout.set_style(id, &LayoutStyle::default());
        id
    }

    fn paint(&mut self, bounds: Rect, cx: &mut PaintCx) {
        // Without an atlas (GPU-free Scene-structure tests) text emits no glyphs.
        let Some(shaped) = self.shaped.as_ref() else {
            return;
        };
        let Some(atlas) = cx.atlas.as_deref_mut() else {
            return;
        };
        // Rasterize directly into the scene's glyph buffer (reused across frames) — no per-frame
        // temp Vec — then fix up only the newly-appended sprites.
        let start = cx.scene.glyphs.len();
        cx.text
            .rasterize_into(shaped, self.color, atlas, &mut cx.scene.glyphs);
        // Glyph sprites are emitted in text-local coordinates at order 0; offset by the laid-out
        // origin and stamp the traversal painter's order so the text draws above its background.
        // Under a scroll ancestor, mask each glyph to the visible viewport (`rasterize_into` left
        // them unclipped); unclipped text keeps that no-op mask.
        let clip = cx.clip();
        let order = cx.next_order();
        for sprite in &mut cx.scene.glyphs[start..] {
            sprite.bounds.origin.x += bounds.origin.x;
            sprite.bounds.origin.y += bounds.origin.y;
            sprite.order = order;
            if let Some(clip) = clip {
                sprite.content_mask.rect = clip;
            }
        }
    }

    fn handle_event(&mut self, _ev: &Event, _cx: &mut EventCx) {}
}

impl IntoElement for Text {
    fn into_element(self) -> AnyElement {
        Box::new(self)
    }
}
