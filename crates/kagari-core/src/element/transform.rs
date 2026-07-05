//! The [`Transformed`] element (#221): applies a paint-space uniform scale + translate to a subtree.
//! taffy lays out the child at base scale; the transform maps the child's painted primitives + hit
//! regions in [`PaintCx`] (`push_transform`/`pop_transform`), so zooming re-paints without relayout
//! (specs §4.3). Scale/offset are reactive (gesture-driven via #237 `ZoomPanView`); nesting composes.

use std::sync::{Arc, Mutex};

use kagari_base::{NodeId, Point, Rect, Transform};
use kagari_layout::{Display, LayoutStyle};

use super::{AnyElement, DamageSink, Element, Event, EventCx, IntoElement, LayoutCx, PaintCx};
use crate::arena::Node;
use crate::reactive::{Prop, create_effect};

/// The smallest scale the element applies — keeps the transform invertible (window→content) and avoids a
/// zero-area subtree. A caller asking for `<= 0` is clamped to this.
const MIN_SCALE: f32 = 1.0e-3;

/// Where a scale pivots (#266). The default [`WindowOrigin`](Self::WindowOrigin) is the legacy zoom pivot
/// (paint-space origin); an [`Anchor`](Self::Anchor) makes the scale pivot about a point of the child's own
/// laid-out box, so a widget can *pop* / *press* about its own center without moving off-place.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum TransformOrigin {
    /// Scale about window `(0, 0)` — `map(p) = p·scale + offset`. The `offset` (pan) then positions the
    /// result. This is the #221 / #237 zoom behavior and stays the default so zoom is unchanged.
    #[default]
    WindowOrigin,
    /// Scale about a **normalized point of the child's laid-out box**: `(0,0)` = top-left, `(0.5,0.5)` =
    /// center, `(1,1)` = bottom-right. Resolved to a window-space pivot from the child's absolute bounds at
    /// paint, so the child grows/shrinks in place.
    Anchor(Point),
}

impl TransformOrigin {
    /// Pivot about the child's center — the common widget pop/press origin.
    pub const CENTER: Self = TransformOrigin::Anchor(Point { x: 0.5, y: 0.5 });
    /// Pivot about the child's top-left (the child's own origin).
    pub const TOP_LEFT: Self = TransformOrigin::Anchor(Point { x: 0.0, y: 0.0 });
}

/// A subtree painted + hit-tested under a uniform scale + translate (#221). Build with [`transform`] and
/// set `.scale(..)` / `.offset(..)` (static or reactive). The child lays out at base scale; the mapping
/// is paint/hit only, so zoom never relayouts. Nesting composes (`outer(inner(p))`).
///
/// **Composition boundary (MVP):** the transform maps *everything its child emits* on pop, so content that
/// should escape the transform does not. Two cases the consumer must arrange around (proper composition is
/// deferred to #237 `ZoomPanView`, which owns the viewport):
/// - A **clipping ancestor** (e.g. a [`Scroll`](super::scroll::Scroll)) with `scale != 1`: its window-space
///   clip is folded into each primitive's content mask *before* this maps it, so the clip is scaled too —
///   content can leak past the viewport. Keep the transform the outermost paint under its viewport (no
///   clipping ancestor between the viewport and the transform).
/// - An **overlay** (menu / tooltip / modal backdrop) declared *inside* the child scales/moves with the
///   zoom. Declare overlays at the window level (via the overlay substrate, #219) so they stay screen-fixed.
pub struct Transformed {
    child: Option<AnyElement>,
    scale: Option<Prop<f32>>,
    offset: Option<Prop<Point>>,
    /// Resolved scale/offset: written by the bound effect (or once, for a static prop), read by `paint`.
    /// Shared + `'static` so the effect can own a handle (mirrors `Scroll`'s `resolved_offset`).
    resolved_scale: Arc<Mutex<Option<f32>>>,
    resolved_offset: Arc<Mutex<Option<Point>>>,
    /// Where the scale pivots (#266). Structural (not animated) → a plain value, not a `Prop`.
    origin: TransformOrigin,
    id: Option<NodeId>,
    child_id: Option<NodeId>,
}

/// Wraps `child` in a transform element (identity until `.scale`/`.offset` are set).
pub fn transform(child: impl IntoElement) -> Transformed {
    Transformed {
        child: Some(child.into_element()),
        scale: None,
        offset: None,
        resolved_scale: Arc::new(Mutex::new(None)),
        resolved_offset: Arc::new(Mutex::new(None)),
        origin: TransformOrigin::WindowOrigin,
        id: None,
        child_id: None,
    }
}

impl Transformed {
    /// Sets the uniform zoom scale (static `f32` or reactive). `<= 0` clamps to a small positive min so
    /// the transform stays invertible. Gestures drive this via #237.
    pub fn scale(mut self, scale: impl Into<Prop<f32>>) -> Self {
        self.scale = Some(scale.into());
        self
    }

    /// Sets the pan offset (translate; static [`Point`] or reactive), applied after scaling.
    pub fn offset(mut self, offset: impl Into<Prop<Point>>) -> Self {
        self.offset = Some(offset.into());
        self
    }

    /// Sets the scale pivot (#266). The default [`WindowOrigin`](TransformOrigin::WindowOrigin) keeps the
    /// legacy zoom behavior; [`CENTER`](TransformOrigin::CENTER) makes a scale pop/press about the child's
    /// own center. Structural (not animated), so it takes a plain value — only `scale`/`offset` are reactive.
    pub fn origin(mut self, origin: TransformOrigin) -> Self {
        self.origin = origin;
        self
    }

    /// Binds the reactive scale prop into `resolved_scale` (mirrors `Scroll::bind_offset`): a static prop
    /// writes once; a reactive prop registers a synchronous effect that re-resolves, and on a change
    /// writes the cell + flags **paint**-damage for `id` (paint-only zoom, no relayout — RK-005/008/009).
    fn bind_scale(&mut self, damage: &Arc<dyn DamageSink>, id: NodeId) {
        match self.scale.take() {
            Some(Prop::Static(s)) => self.store_scale(s),
            Some(Prop::Reactive(read)) => {
                let cell = Arc::clone(&self.resolved_scale);
                let damage = Arc::clone(damage);
                create_effect(move || {
                    let s = read().max(MIN_SCALE);
                    if let Ok(mut slot) = cell.lock() {
                        if *slot != Some(s) {
                            *slot = Some(s);
                            damage.mark_paint_dirty(id);
                        }
                    }
                });
            }
            None => {}
        }
    }

    /// Binds the reactive offset prop (as [`bind_scale`](Self::bind_scale), for the pan translate).
    fn bind_offset(&mut self, damage: &Arc<dyn DamageSink>, id: NodeId) {
        match self.offset.take() {
            Some(Prop::Static(o)) => self.store_offset(o),
            Some(Prop::Reactive(read)) => {
                let cell = Arc::clone(&self.resolved_offset);
                let damage = Arc::clone(damage);
                create_effect(move || {
                    let o = read();
                    if let Ok(mut slot) = cell.lock() {
                        if *slot != Some(o) {
                            *slot = Some(o);
                            damage.mark_paint_dirty(id);
                        }
                    }
                });
            }
            None => {}
        }
    }

    fn store_scale(&self, s: f32) {
        if let Ok(mut slot) = self.resolved_scale.lock() {
            *slot = Some(s.max(MIN_SCALE));
        }
    }

    fn store_offset(&self, o: Point) {
        if let Ok(mut slot) = self.resolved_offset.lock() {
            *slot = Some(o);
        }
    }

    /// The paint transform (resolved scale + offset) with the [`origin`](Self::origin) applied against the
    /// child's absolute `child_bounds`. For [`WindowOrigin`](TransformOrigin::WindowOrigin) this is the
    /// legacy `p·scale + offset`; for an [`Anchor`](TransformOrigin::Anchor) the scale pivots about the
    /// anchor point of the child's box (`(p − pivot)·scale + pivot = p·scale + pivot·(1 − scale)`), so the
    /// child grows/shrinks in place — plus any user offset. Identity defaults when scale/offset are unset.
    fn effective_transform(&self, child_bounds: Rect) -> Transform {
        let scale = self
            .resolved_scale
            .lock()
            .ok()
            .and_then(|s| *s)
            .unwrap_or(1.0);
        let user = self
            .resolved_offset
            .lock()
            .ok()
            .and_then(|o| *o)
            .unwrap_or(Point::new(0.0, 0.0));
        match self.origin {
            TransformOrigin::WindowOrigin => Transform::new(scale, user),
            TransformOrigin::Anchor(frac) => {
                let pivot = Point::new(
                    child_bounds.origin.x + frac.x * child_bounds.size.w,
                    child_bounds.origin.y + frac.y * child_bounds.size.h,
                );
                let offset = Point::new(
                    pivot.x * (1.0 - scale) + user.x,
                    pivot.y * (1.0 - scale) + user.y,
                );
                Transform::new(scale, offset)
            }
        }
    }
}

impl Element for Transformed {
    fn request_layout(&mut self, cx: &mut LayoutCx) -> NodeId {
        let id = match self.id {
            Some(id) => id,
            None => {
                let id = cx.arena.insert(Node::default());
                self.id = Some(id);
                cx.layout.insert(id, None);
                // Block container: the child lays out at base scale; the transform maps paint/hit only.
                cx.layout.set_style(
                    id,
                    &LayoutStyle {
                        display: Display::Block,
                        ..LayoutStyle::default()
                    },
                );
                self.bind_scale(&cx.damage, id);
                self.bind_offset(&cx.damage, id);
                id
            }
        };

        if let Some(child) = self.child.as_mut() {
            let child_id = child.request_layout(cx);
            if let Some(node) = cx.arena.get_mut(child_id) {
                node.parent = Some(id);
            }
            if let Some(node) = cx.arena.get_mut(id) {
                node.children = vec![child_id];
            }
            cx.layout.set_children(id, &[child_id]);
            self.child_id = Some(child_id);
        }
        id
    }

    fn paint(&mut self, bounds: Rect, cx: &mut PaintCx) {
        let Some(child_id) = self.child_id else {
            return;
        };
        // Paint the child at its base (untransformed) absolute bounds (RK-007: parent origin + child
        // local origin); `push_transform` maps its emitted primitives + hit regions on pop. The transform
        // is resolved against these absolute bounds so an anchored (#266) scale can pivot about the child.
        let local = cx.layout.layout(child_id);
        let child_bounds = Rect {
            origin: Point {
                x: bounds.origin.x + local.origin.x,
                y: bounds.origin.y + local.origin.y,
            },
            size: local.size,
        };
        let t = self.effective_transform(child_bounds);
        cx.push_transform(t);
        if let Some(child) = self.child.as_mut() {
            child.paint(child_bounds, cx);
        }
        cx.pop_transform();
    }

    fn handle_event(&mut self, ev: &Event, cx: &mut EventCx) {
        // Descend toward the target so the child's handlers run (dispatch drives the path).
        let Some(id) = self.id else {
            return;
        };
        if cx.next_child_on_path(id) == self.child_id {
            if let Some(child) = self.child.as_mut() {
                child.handle_event(ev, cx);
            }
        }
    }
}

impl IntoElement for Transformed {
    fn into_element(self) -> AnyElement {
        Box::new(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::{DamageSink, PaintCx, div};
    use crate::event::HitTest;
    use kagari_base::{Color, Size};
    use kagari_layout::LayoutTree;
    use kagari_render::{Background, Scene};
    use kagari_style::Theme;
    use kagari_text::{FontDb, TextSystem};

    struct NoopDamage;
    impl DamageSink for NoopDamage {
        fn mark_paint_dirty(&self, _id: NodeId) {}
    }

    fn green() -> Background {
        Background::Solid(Color::new(0.0, 1.0, 0.0, 1.0))
    }

    /// Builds, lays out, and paints `root` with a hit-test attached, returning the scene + hit-test.
    fn build_paint(root: &mut AnyElement, viewport: Size) -> (Scene, HitTest) {
        let mut arena = crate::arena::Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let damage: Arc<dyn DamageSink> = Arc::new(NoopDamage);
        let theme = Theme::default();
        let id = root.request_layout(&mut LayoutCx {
            arena: &mut arena,
            layout: &mut layout,
            text: &mut text,
            damage,
            theme: &theme,
            focus: None,
            cursor: None,
        });
        layout.compute(id, viewport).unwrap();
        let mut scene = Scene::new();
        let mut hit = HitTest::new();
        {
            let bounds = layout.layout(id);
            let mut cx = PaintCx::new(&mut scene, &layout, &mut text, None, Some(&mut hit), &theme);
            root.paint(bounds, &mut cx);
        }
        (scene, hit)
    }

    #[test]
    fn transform_should_scale_child_primitives() {
        // A 20x20 green quad at the origin under scale=2 → a 40x40 quad (bounds + content mask scaled).
        let mut root = transform(div().size(Size { w: 20.0, h: 20.0 }).background(green()))
            .scale(2.0)
            .into_element();
        let (scene, _hit) = build_paint(&mut root, Size { w: 200.0, h: 200.0 });

        assert_eq!(scene.quads.len(), 1);
        let q = &scene.quads[0];
        assert_eq!(
            q.bounds,
            Rect::from_xywh(0.0, 0.0, 40.0, 40.0),
            "the quad is scaled ×2"
        );
        assert_eq!(
            q.content_mask.rect.size.w, 40.0,
            "the content mask is scaled with the quad"
        );
    }

    #[test]
    fn transformed_should_scale_about_center_origin() {
        // A 20x20 green quad at the origin (center (10,10)) under scale=0.5 with origin CENTER shrinks
        // *about its center* → a 10x10 quad centered at (10,10) = bounds (5,5,10,10). Contrast the default
        // WindowOrigin, which pins the top-left → (0,0,10,10). This proves the anchor pivot (#266).
        let mut centered = transform(div().size(Size { w: 20.0, h: 20.0 }).background(green()))
            .scale(0.5)
            .origin(TransformOrigin::CENTER)
            .into_element();
        let (scene, _hit) = build_paint(&mut centered, Size { w: 200.0, h: 200.0 });
        assert_eq!(
            scene.quads[0].bounds,
            Rect::from_xywh(5.0, 5.0, 10.0, 10.0),
            "center-origin scale shrinks about the child's center (10,10)"
        );

        // Default (WindowOrigin) scales about window (0,0): same size, but pinned at the top-left.
        let mut window_origin =
            transform(div().size(Size { w: 20.0, h: 20.0 }).background(green()))
                .scale(0.5)
                .into_element();
        let (scene, _hit) = build_paint(&mut window_origin, Size { w: 200.0, h: 200.0 });
        assert_eq!(
            scene.quads[0].bounds,
            Rect::from_xywh(0.0, 0.0, 10.0, 10.0),
            "the default WindowOrigin pins the top-left (legacy zoom behavior)"
        );
    }

    #[test]
    fn transform_should_offset_child_primitives() {
        let mut root = transform(div().size(Size { w: 10.0, h: 10.0 }).background(green()))
            .scale(1.0)
            .offset(Point::new(30.0, 15.0))
            .into_element();
        let (scene, _hit) = build_paint(&mut root, Size { w: 200.0, h: 200.0 });
        assert_eq!(
            scene.quads[0].bounds.origin,
            Point::new(30.0, 15.0),
            "the quad is translated by the offset"
        );
    }

    #[test]
    fn transform_hit_should_map_pointer_to_content() {
        // An interactive 20x20 child at the origin under scale=2: its hit region covers window (0,0)..(40,40).
        let mut root = transform(div().size(Size { w: 20.0, h: 20.0 }).on_click(|_, _| {}))
            .scale(2.0)
            .into_element();
        let (_scene, hit) = build_paint(&mut root, Size { w: 200.0, h: 200.0 });

        // A window point at (30,30) is inside the scaled region; (50,50) is outside the 40x40 area.
        assert!(
            hit.pick(Point::new(30.0, 30.0)).is_some(),
            "picks inside the scaled region"
        );
        assert!(
            hit.pick(Point::new(50.0, 50.0)).is_none(),
            "misses outside the scaled region"
        );

        // window→content inverse maps the window point back into the child's base 20x20 space.
        let t = Transform::new(2.0, Point::new(0.0, 0.0));
        assert_eq!(
            t.inverse_point(Point::new(30.0, 30.0)),
            Point::new(15.0, 15.0)
        );
    }

    #[test]
    fn transform_nested_should_compose() {
        // Inner scale=2 inside outer scale=3 → the child quad is scaled ×6.
        let mut root = transform(
            transform(div().size(Size { w: 5.0, h: 5.0 }).background(green())).scale(2.0),
        )
        .scale(3.0)
        .into_element();
        let (scene, _hit) = build_paint(&mut root, Size { w: 400.0, h: 400.0 });
        assert_eq!(
            scene.quads[0].bounds.size.w, 30.0,
            "nested transforms compose (5 × 2 × 3 = 30)"
        );
    }

    #[test]
    fn transform_nested_should_apply_outer_after_inner() {
        // Inner (scale 2, offset (1,0)) inside outer (scale 3, offset (0,4)); the child origin must be
        // outer(inner(base)), proving composition ORDER (not just magnitude). base child origin = (0,0).
        let mut root = transform(
            transform(div().size(Size { w: 4.0, h: 4.0 }).background(green()))
                .scale(2.0)
                .offset(Point::new(1.0, 0.0)),
        )
        .scale(3.0)
        .offset(Point::new(0.0, 4.0))
        .into_element();
        let (scene, _hit) = build_paint(&mut root, Size { w: 400.0, h: 400.0 });

        let inner = Transform::new(2.0, Point::new(1.0, 0.0));
        let outer = Transform::new(3.0, Point::new(0.0, 4.0));
        let expected = outer.map_point(inner.map_point(Point::new(0.0, 0.0)));
        assert_eq!(
            scene.quads[0].bounds.origin, expected,
            "child origin = outer(inner(base)) — composition order"
        );
    }

    #[test]
    fn transform_reactive_scale_should_repaint_without_relayout() {
        use crate::reactive::prelude::*;
        use crate::reactive::{Owner, RwSignal, rx};
        use std::sync::atomic::{AtomicUsize, Ordering};

        // A DamageSink counting paint- vs layout-dirty marks (the #221 headline: zoom = paint-only).
        struct Counting {
            paint: AtomicUsize,
            layout: AtomicUsize,
        }
        impl DamageSink for Counting {
            fn mark_paint_dirty(&self, _id: NodeId) {
                self.paint.fetch_add(1, Ordering::SeqCst);
            }
            fn mark_layout_dirty(&self, _id: NodeId) {
                self.layout.fetch_add(1, Ordering::SeqCst);
            }
        }

        let owner = Owner::new();
        owner.set();

        let damage = Arc::new(Counting {
            paint: AtomicUsize::new(0),
            layout: AtomicUsize::new(0),
        });
        let damage_dyn: Arc<dyn DamageSink> = damage.clone();

        let zoom = RwSignal::new(1.0_f32);
        let mut arena = crate::arena::Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let theme = Theme::default();
        let mut el = transform(div().size(Size { w: 10.0, h: 10.0 }))
            .scale(rx(move || zoom.get()))
            .into_element();
        el.request_layout(&mut LayoutCx {
            arena: &mut arena,
            layout: &mut layout,
            text: &mut text,
            damage: damage_dyn.clone(),
            theme: &theme,
            focus: None,
            cursor: None,
        });

        // Changing the zoom signal re-resolves the bound effect and flags PAINT damage only — never
        // layout (paint-space transform ⇒ no relayout).
        let paint_before = damage.paint.load(Ordering::SeqCst);
        zoom.set(2.0);
        assert!(
            damage.paint.load(Ordering::SeqCst) > paint_before,
            "a scale change flags paint-damage"
        );
        assert_eq!(
            damage.layout.load(Ordering::SeqCst),
            0,
            "a scale change never flags layout-damage (paint-space, no relayout)"
        );

        drop(owner);
    }

    #[test]
    fn active_transform_should_report_composed() {
        let mut scene = Scene::new();
        let layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let theme = Theme::default();
        let mut cx = PaintCx::new(&mut scene, &layout, &mut text, None, None, &theme);

        assert_eq!(
            cx.active_transform(),
            Transform::IDENTITY,
            "identity with no transform"
        );
        let outer = Transform::new(3.0, Point::new(0.0, 4.0));
        let inner = Transform::new(2.0, Point::new(1.0, 0.0));
        cx.push_transform(outer);
        cx.push_transform(inner);
        assert_eq!(
            cx.active_transform(),
            outer.compose(inner),
            "active transform = outer ∘ inner (content→window)"
        );
        cx.pop_transform();
        cx.pop_transform();
        assert_eq!(
            cx.active_transform(),
            Transform::IDENTITY,
            "back to identity after pops"
        );
    }
}
