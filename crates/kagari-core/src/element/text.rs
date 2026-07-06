//! The [`Text`] leaf element (#34): shaped at build, rasterized at paint.

use kagari_base::{Color, NodeId, Px, Rect, SharedString};
use kagari_layout::LayoutStyle;
use kagari_style::ColorRole;
use kagari_text::{ShapedText, TextStyle, fontdb};

use super::reactive_prop::ReactiveProp;
use super::{AnyElement, Element, Event, EventCx, IntoElement, LayoutCx, PaintCx};
use crate::arena::Node;
use crate::reactive::Prop;

/// A text leaf. Shapes its content once at `request_layout` (caching the result and reporting its
/// intrinsic size via a [`MeasureFn`](kagari_layout::MeasureFn)), and rasterizes the cached
/// shaping into glyph sprites at `paint`.
pub struct Text {
    text: SharedString,
    style: TextStyle,
    color: Color,
    /// Semantic color **role** (static or reactive, #245) — the theme-reactive counterpart of `color`:
    /// the resolved role is stored, and `paint` resolves role → color via `cx.theme`, so it follows a
    /// theme swap (and, for a reactive `rx(..)`, a state signal). Takes precedence over `color` when set.
    color_role: ReactiveProp<ColorRole>,
    /// Whether to wrap to the available width (#260). When set, the measure re-shapes at the width taffy
    /// allots each `compute` (single line when the width is unconstrained); otherwise the text is shaped
    /// once at build and stays single-line.
    wrap: bool,
    id: Option<NodeId>,
    shaped: Option<ShapedText>,
    /// The width `shaped` was last shaped at, for a wrapping text (#260) — so `paint` re-shapes only when
    /// the laid-out width changes, reusing the memoized shaping (a borrow) otherwise instead of cloning
    /// the cached `ShapedText` every frame. Unused on the non-wrap path (`shaped` is the build shaping).
    shaped_width: Option<f32>,
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
        color_role: ReactiveProp::default(),
        wrap: false,
        id: None,
        shaped: None,
        shaped_width: None,
    }
}

impl Text {
    /// Sets the glyph color from a raw [`Color`].
    pub fn color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }

    /// Sets the glyph color from a semantic **role** — a static token or a reactive [`Prop`] (#245).
    /// Resolved at paint via `cx.theme`, so it follows a theme swap; a reactive `rx(..)` also follows a
    /// state signal. Takes precedence over the raw [`color`](Self::color) when set. **Damage: paint-dirty.**
    pub fn color_role(mut self, role: impl Into<Prop<ColorRole>>) -> Self {
        self.color_role.set(role);
        self
    }

    /// Sets the font size.
    pub fn size(mut self, size: Px) -> Self {
        self.style.size = size;
        self
    }

    /// Wraps the text to its available width (#260): line-breaks to the width taffy allots (the container
    /// width), and stays single-line when the width is unconstrained. Off by default. **Damage: layout**
    /// (a width change re-measures via the shaper at `compute` time).
    pub fn wrap(mut self, wrap: bool) -> Self {
        self.wrap = wrap;
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
        self.color_role.bind(&cx.damage, id);

        if self.wrap {
            // Wrapping text (#260): the measure re-shapes at the available width each `compute` via a
            // cloned `Shaper` handle — `avail.width == None` (unconstrained) → single line, `Some(w)` →
            // wrap at `w`. `paint` re-shapes at the laid-out width (cache-deduped), so `shaped` stays `None`.
            let shaper = cx.text.shaper();
            let text = self.text.clone();
            let style = self.style.clone();
            cx.layout.set_measure(
                id,
                Box::new(move |avail| shaper.shape(&text, &style, avail.width).size),
            );
        } else {
            // Shape once at build: cache the result, report its intrinsic size as a constant measure (so
            // `compute` needs no TextSystem), and reuse the cached shaping at paint.
            let shaped = cx.text.shape(&self.text, &self.style, None);
            let size = shaped.size;
            self.shaped = Some(shaped);
            cx.layout.set_measure(id, Box::new(move |_avail| size));
        }
        // A wrapping text needs `min-width: 0` (flexbox's default `min-width: auto` = min-content keeps a
        // flex item at its content width, so it would never shrink to wrap). Per-axis, so `min-height`
        // stays `auto` (no vertical clipping in a height-constrained flex column). #260.
        let style = if self.wrap {
            LayoutStyle {
                min_width: Some(0.0),
                ..LayoutStyle::default()
            }
        } else {
            LayoutStyle::default()
        };
        cx.layout.set_style(id, &style);
        id
    }

    fn paint(&mut self, bounds: Rect, cx: &mut PaintCx) {
        // Resolve the glyph color: a semantic role (resolved via `cx.theme`, so it follows a theme
        // swap / state signal) takes precedence over the raw color. Computed before the atlas/shaping
        // borrows so `cx.theme` and `cx.atlas`/`cx.scene` stay disjoint.
        let color = self
            .color_role
            .current()
            .map(|role| cx.theme.resolve_color(role))
            .unwrap_or(self.color);
        // Without an atlas (GPU-free Scene-structure tests) text emits no glyphs.
        let Some(atlas) = cx.atlas.as_deref_mut() else {
            return;
        };
        // Wrapping text re-shapes at the laid-out width, but **memoized**: only when the width changed
        // since the last paint (otherwise the build/prev `shaped` is reused by borrow — no per-frame
        // `ShapedText` clone). Non-wrap reuses the build-time single-line shaping unchanged.
        if self.wrap && self.shaped_width != Some(bounds.size.w) {
            self.shaped = Some(cx.text.shape(&self.text, &self.style, Some(bounds.size.w)));
            self.shaped_width = Some(bounds.size.w);
        }
        let Some(shaped) = self.shaped.as_ref() else {
            return;
        };
        // Rasterize directly into the scene's glyph buffer (reused across frames) — no per-frame
        // temp Vec — then fix up only the newly-appended sprites.
        let start = cx.scene.glyphs.len();
        cx.text
            .rasterize_into(shaped, color, atlas, &mut cx.scene.glyphs);
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::arena::Arena;
    use crate::damage::DamageState;
    use crate::element::DamageSink;
    use crate::paint::render_tree;
    use crate::reactive::rx;
    use kagari_base::Size as BSize;
    use kagari_layout::LayoutTree;
    use kagari_render::Scene;
    use kagari_style::{ColorRole, Theme};
    use kagari_text::FontDb;
    use kagari_text::TextSystem;
    use reactive_graph::owner::Owner;
    use reactive_graph::prelude::*;
    use reactive_graph::signal::signal;

    struct NoopDamage;
    impl DamageSink for NoopDamage {
        fn mark_paint_dirty(&self, _id: NodeId) {}
    }

    fn build(t: &mut Text) {
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut ts = TextSystem::new(FontDb::new());
        let damage: Arc<dyn DamageSink> = Arc::new(NoopDamage);
        let theme = Theme::default();
        t.request_layout(&mut LayoutCx {
            arena: &mut arena,
            layout: &mut layout,
            text: &mut ts,
            damage,
            theme: &theme,
            focus: None,
            cursor: None,
        });
    }

    #[test]
    fn text_color_role_should_take_precedence_over_raw_color() {
        // A static `.color_role(role)` stores the role at build and wins over a raw `.color(..)` (paint
        // does `color_role.current().map(resolve).unwrap_or(self.color)`); `paint` resolves the role via
        // `cx.theme` (theme-reactive) — a role is enough for a GPU-free assertion (glyph color needs an atlas).
        let mut t = text("hi")
            .color(Color::new(1.0, 0.0, 0.0, 1.0))
            .color_role(ColorRole::Accent);
        build(&mut t);
        assert_eq!(
            t.color_role.current(),
            Some(ColorRole::Accent),
            "color_role is set and takes precedence over the raw color at paint"
        );
    }

    #[test]
    fn text_reactive_color_role_should_update_on_signal() {
        // A reactive `.color_role(rx(..))` re-resolves on a signal write and flags **paint-dirty**
        // (RK-003/005: synchronous effect, keep `owner` alive; RK-009: paint damage, not layout).
        let owner = Owner::new();
        owner.set();
        let (read, write) = signal(false);
        let mut t = text("hi").color_role(rx(move || {
            if read.get() {
                ColorRole::Accent
            } else {
                ColorRole::Text
            }
        }));

        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut ts = TextSystem::new(FontDb::new());
        let damage = Arc::new(DamageState::default());
        let theme = Theme::default();
        let sink: Arc<dyn DamageSink> = damage.clone();
        t.request_layout(&mut LayoutCx {
            arena: &mut arena,
            layout: &mut layout,
            text: &mut ts,
            damage: sink,
            theme: &theme,
            focus: None,
            cursor: None,
        });

        assert_eq!(t.color_role.current(), Some(ColorRole::Text));
        write.set(true);
        assert_eq!(
            t.color_role.current(),
            Some(ColorRole::Accent),
            "the signal write re-resolves the color role"
        );
        assert!(
            damage.is_dirty(),
            "the reactive color-role write flags paint damage"
        );
        drop(owner);
    }

    /// Lays out `t` inside a fixed-width (`parent_w` × tall) flex parent whose cross-axis is not stretched
    /// (`items_start`), so the text sizes to its wrapped content, and returns the text's computed size. The
    /// `render_tree` pass exercises the wrap **measure** (at `compute`); the paint re-shape is not hit here
    /// (paint early-returns without an atlas, before the re-shape). This is the realistic case: a flex-item
    /// text needs `min-width: 0` to shrink and wrap to its container (#260).
    fn text_in_width(t: Text, parent_w: f32) -> BSize {
        let mut root = crate::element::div()
            .size(BSize {
                w: parent_w,
                h: 400.0,
            })
            .items_start()
            .child(t)
            .into_element();
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut ts = TextSystem::new(FontDb::new());
        let mut scene = Scene::new();
        let damage = Arc::new(DamageState::default());
        let root_id = render_tree(
            &mut root,
            &mut arena,
            &mut layout,
            &mut ts,
            None,
            None,
            None,
            None,
            None,
            None,
            &mut scene,
            BSize { w: 400.0, h: 800.0 },
            &damage,
            &Theme::default(),
        )
        .unwrap();
        let text_id = arena
            .get(root_id)
            .and_then(|n| n.children.first().copied())
            .expect("the parent has the text as its child");
        layout.layout(text_id).size
    }

    #[test]
    fn text_wrap_should_break_to_available_width() {
        // In a fixed-width parent, `.wrap(true)` line-breaks the (long) text — laying out taller
        // (multi-line) and within the parent width — while `.wrap(false)` stays single-line, wider than
        // the parent (proving `min-width: 0` let the flex item shrink and wrap). #260.
        let long = "The quick brown fox jumps over the lazy dog";
        let wrapped = text_in_width(text(long).wrap(true), 80.0);
        let single = text_in_width(text(long).wrap(false), 80.0);
        assert!(
            wrapped.h > single.h,
            "wrapped text is taller (multi-line) than single-line: wrapped={wrapped:?} single={single:?}"
        );
        assert!(
            wrapped.w <= 80.5 && single.w > 80.5,
            "wrapped text fits the parent width while single-line overflows: wrapped={wrapped:?} single={single:?}"
        );
    }

    #[test]
    fn text_wrap_should_not_clip_height_in_short_flex_col() {
        // #260 regression: `min-width: 0` for wrapping must be **per-axis** (min-height stays `auto`), or a
        // wrapping text in a height-constrained overflowing flex column gets vertically shrunk/clipped. The
        // wrapped text keeps its full (multi-line) height, overflowing the short column.
        let long = "The quick brown fox jumps over the lazy dog wraps to several lines here";
        let short_h = 24.0; // ~ one line — the wrapped text is much taller
        let mut root = crate::element::div()
            .flex_col()
            .size(BSize {
                w: 90.0,
                h: short_h,
            })
            .child(text(long).wrap(true))
            .into_element();
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut ts = TextSystem::new(FontDb::new());
        let mut scene = Scene::new();
        let damage = Arc::new(DamageState::default());
        let root_id = render_tree(
            &mut root,
            &mut arena,
            &mut layout,
            &mut ts,
            None,
            None,
            None,
            None,
            None,
            None,
            &mut scene,
            BSize { w: 400.0, h: 400.0 },
            &damage,
            &Theme::default(),
        )
        .unwrap();
        let text_id = arena
            .get(root_id)
            .and_then(|n| n.children.first().copied())
            .expect("the column has the text as its child");
        let text_h = layout.layout(text_id).size.h;
        assert!(
            text_h > short_h,
            "the wrapped text keeps its full multi-line height (min-height stays auto), not clipped to the \
             short column: text_h={text_h} col_h={short_h}"
        );
    }
}
