//! The [`Scroll`] container element (#60): clips its children to a fixed viewport and translates them
//! by a **reactive offset** — paint-only, so scrolling never relays out (specs §4.3). Overlay
//! scrollbars reflect the content/viewport ratio; [`ScrollHandle`] drives the offset imperatively.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use kagari_base::{Color, Corners, Edges, NodeId, Point, Rect, Size};
use kagari_layout::scroll::{ScrollState, fling_target, thumb};
use kagari_layout::{Display, LayoutStyle, Overflow};
use kagari_render::{Background, Border, Quad, RoundedRect};

use super::{AnyElement, DamageSink, Element, Event, EventCx, IntoElement, LayoutCx, PaintCx};
use crate::anim::{Animated, AnimationSpec};
use crate::arena::Node;
use crate::reactive::{Prop, create_effect, rx};

/// Overlay-scrollbar thumb thickness (logical px).
const SCROLLBAR_THICKNESS: f32 = 6.0;

/// The scroll spring (#176): near-critically-damped (2·√120 ≈ 21.9) so `scroll_to` and flings settle
/// smoothly without a visible bounce. Tunable — the inertial-scroll physics tuning is post-MVP.
const SCROLL_SPRING: AnimationSpec = AnimationSpec::Spring {
    stiffness: 120.0,
    damping: 22.0,
};

/// A clone-shared handle to a [`Scroll`]'s offset (#60): an [`Animated<Point>`](Animated) spring behind
/// [`Scroll::offset`], for imperative control and reactive reads. Smooth by default (#176) —
/// `scroll_to` animates, `fling` throws with a release velocity, `jump_to` snaps instantly.
///
/// Mirrors [`FocusHandle`](crate::event::FocusHandle). Constructing one needs a reactive owner (the
/// app root, #36); in tests set an `Owner` first. The frame clock drives [`tick`](Self::tick) while
/// animating (the live drive lands with the deferred frame loop). Bind it with `scroll().offset(&handle)`.
#[derive(Clone)]
pub struct ScrollHandle {
    offset: Animated<Point>,
}

impl ScrollHandle {
    /// A new handle at offset `(0, 0)`.
    pub fn new() -> Self {
        Self {
            offset: Animated::new(Point::new(0.0, 0.0), SCROLL_SPRING),
        }
    }

    /// The current scroll offset — a tracked read inside a reactive closure.
    pub fn offset(&self) -> Point {
        self.offset.get()
    }

    /// Animates to an absolute offset (the container clamps it to the content at paint). Smooth by
    /// default (#176); for an instant move use [`jump_to`](Self::jump_to).
    pub fn scroll_to(&self, offset: Point) {
        self.offset.set_target(offset);
    }

    /// Animates by a delta (e.g. one wheel notch), retargeting from the current **target** so repeated
    /// notches accumulate (rather than dropping deltas mid-flight); the container clamps at paint.
    pub fn scroll_by(&self, delta: Point) {
        self.offset.set_target(self.offset.target() + delta);
    }

    /// Snaps to an absolute offset immediately, without animating — for restoring a saved scroll
    /// position or any move where a smooth transition would be wrong.
    pub fn jump_to(&self, offset: Point) {
        self.offset.snap_to(offset);
    }

    /// Flings the scroll with a release `velocity` (logical px/sec): seeds the spring so the offset
    /// carries forward and settles at the projected, content-clamped rest point (#176). `content` and
    /// `viewport` come from the scroll's layout (the caller passes the last-measured extents).
    pub fn fling(&self, velocity: Point, content: Size, viewport: Size) {
        let target = fling_target(self.offset.get_untracked(), velocity, content, viewport);
        self.offset.fling(velocity, target);
    }

    /// Advances the scroll animation by `dt`, returning `false` once it has settled. Driven by the
    /// frame clock while animating (the live drive lands with the deferred frame loop); returns
    /// `false` immediately when at rest.
    pub fn tick(&self, dt: Duration) -> bool {
        self.offset.tick(dt)
    }
}

impl Default for ScrollHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl From<&ScrollHandle> for Prop<Point> {
    fn from(handle: &ScrollHandle) -> Self {
        // `Animated<Point>` is a shared handle (Clone); capture a clone so the reactive prop tracks the
        // offset — tick's between-frame writes re-run this and flag paint-damage.
        let offset = handle.offset.clone();
        rx(move || offset.get())
    }
}

/// A scroll container: a fixed-size viewport that clips its children and translates them by a
/// reactive offset (paint-only). Children stack in **block** flow so their natural size overflows the
/// viewport (a flex container would shrink them to fit). Bind the offset with a static [`Point`] or a
/// reactive [`ScrollHandle`]; overlay scrollbars appear when content overflows.
pub struct Scroll {
    children: Vec<AnyElement>,
    layout: LayoutStyle,
    offset: Option<Prop<Point>>,
    /// Resolved offset: written by the bound reactive effect (or once, for a static prop), read by
    /// `paint`. Shared + `'static` so the effect can own a handle to it (mirrors `Div`'s `resolved_bg`).
    resolved_offset: Arc<Mutex<Option<Point>>>,
    id: Option<NodeId>,
    child_ids: Vec<NodeId>,
    scrollbar: bool,
}

/// Creates an empty scroll container (block flow, overflow-scroll). Set a viewport with
/// [`Scroll::size`] so content can overflow it.
///
/// Children stack in **block** flow (natural size, top-to-bottom) — unlike [`div`](super::div)'s flex
/// default — so their content overflows the viewport rather than shrinking to fit. For a flex layout
/// inside a scroll, wrap the content in a single `div()`.
pub fn scroll() -> Scroll {
    Scroll {
        children: Vec::new(),
        layout: LayoutStyle {
            display: Display::Block,
            overflow: Overflow::Scroll,
            ..LayoutStyle::default()
        },
        offset: None,
        resolved_offset: Arc::new(Mutex::new(None)),
        id: None,
        child_ids: Vec::new(),
        scrollbar: true,
    }
}

impl Scroll {
    /// Appends a child element.
    pub fn child(mut self, child: impl IntoElement) -> Self {
        self.children.push(child.into_element());
        self
    }

    /// Appends several child elements.
    pub fn children<I>(mut self, children: I) -> Self
    where
        I: IntoIterator,
        I::Item: IntoElement,
    {
        self.children
            .extend(children.into_iter().map(IntoElement::into_element));
        self
    }

    /// Binds the scroll offset — a static [`Point`] or a reactive [`ScrollHandle`]/closure. A
    /// reactive write re-resolves at paint and flags **paint**-dirty (no relayout).
    pub fn offset(mut self, offset: impl Into<Prop<Point>>) -> Self {
        self.offset = Some(offset.into());
        self
    }

    /// Sets the viewport size — the clipped, scrollable area. A scroll container needs a definite
    /// size so its content can overflow it.
    pub fn size(mut self, size: Size) -> Self {
        self.layout.size = Some(size);
        self
    }

    /// Hides the overlay scrollbars (shown by default when content overflows).
    pub fn hide_scrollbar(mut self) -> Self {
        self.scrollbar = false;
        self
    }

    /// Resolves the offset prop into `resolved_offset`, the **paint-dirty** analogue of `Div`'s
    /// `bind_background`. A static prop writes the cell once; a reactive prop registers a synchronous
    /// effect (ADR 0001) that re-resolves the closure and — only when the value changed (design.md
    /// §9) — writes the cell and flags **paint**-damage for `id`. Registered once at build, so the
    /// effect re-fires only on external signal writes (between frames, serial with the loop; RK-009).
    fn bind_offset(&mut self, damage: &Arc<dyn DamageSink>, id: NodeId) {
        match self.offset.take() {
            Some(Prop::Static(offset)) => self.store_offset(offset),
            Some(Prop::Reactive(read)) => {
                let cell = Arc::clone(&self.resolved_offset);
                let damage = Arc::clone(damage);
                create_effect(move || {
                    let offset = read();
                    if let Ok(mut slot) = cell.lock() {
                        if *slot != Some(offset) {
                            *slot = Some(offset);
                            damage.mark_paint_dirty(id);
                        }
                    }
                });
            }
            None => {}
        }
    }

    fn store_offset(&self, offset: Point) {
        if let Ok(mut slot) = self.resolved_offset.lock() {
            *slot = Some(offset);
        }
    }

    fn current_offset(&self) -> Point {
        self.resolved_offset
            .lock()
            .ok()
            .and_then(|slot| *slot)
            .unwrap_or(Point::new(0.0, 0.0))
    }

    /// Emits the overlay scrollbar thumbs (drawn above content, clipped to the viewport). One thumb
    /// per overflowing axis, sized by the content/viewport ratio via [`kagari_layout::scroll::thumb`].
    fn paint_scrollbars(&self, bounds: Rect, content: Size, offset: Point, cx: &mut PaintCx) {
        // If an active clip fully culls the viewport (e.g. a nested scroll scrolled out of its
        // parent), emit no thumbs — mirroring `Div`'s empty-mask skip, so no degenerate zero-mask
        // quad is pushed.
        let mask_rect = cx.clip_rect(bounds);
        if cx.clip().is_some() && mask_rect.is_empty() {
            return;
        }
        let color = Background::Solid(Color::new(0.6, 0.6, 0.6, 0.6));
        let mask = RoundedRect {
            rect: mask_rect,
            radii: Corners::default(),
        };
        let push_thumb = |rect: Rect, cx: &mut PaintCx| {
            let order = cx.next_order();
            cx.scene.quads.push(Quad {
                bounds: rect,
                corner_radii: Corners::default(),
                bg: color,
                border: Border {
                    widths: Edges::default(),
                    color: Color::TRANSPARENT,
                },
                content_mask: mask,
                order,
            });
        };
        // Vertical thumb on the right edge.
        if let Some((pos, len)) = thumb(bounds.size.h, content.h, offset.y, bounds.size.h) {
            let rect = Rect::from_xywh(
                bounds.origin.x + bounds.size.w - SCROLLBAR_THICKNESS,
                bounds.origin.y + pos,
                SCROLLBAR_THICKNESS,
                len,
            );
            push_thumb(rect, cx);
        }
        // Horizontal thumb on the bottom edge.
        if let Some((pos, len)) = thumb(bounds.size.w, content.w, offset.x, bounds.size.w) {
            let rect = Rect::from_xywh(
                bounds.origin.x + pos,
                bounds.origin.y + bounds.size.h - SCROLLBAR_THICKNESS,
                len,
                SCROLLBAR_THICKNESS,
            );
            push_thumb(rect, cx);
        }
    }
}

impl Element for Scroll {
    fn request_layout(&mut self, cx: &mut LayoutCx) -> NodeId {
        let id = match self.id {
            Some(id) => id,
            None => {
                let id = cx.arena.insert(Node::default());
                self.id = Some(id);
                cx.layout.insert(id, None);
                // Static layout: set the style once so re-builds don't re-dirty taffy every frame
                // (the scroll's layout never changes; only its paint offset does).
                cx.layout.set_style(id, &self.layout);
                self.bind_offset(&cx.damage, id);
                id
            }
        };

        let mut child_ids = Vec::with_capacity(self.children.len());
        for child in &mut self.children {
            let child_id = child.request_layout(cx);
            if let Some(node) = cx.arena.get_mut(child_id) {
                node.parent = Some(id);
            }
            child_ids.push(child_id);
        }
        if let Some(node) = cx.arena.get_mut(id) {
            node.children = child_ids.clone();
        }
        cx.layout.set_children(id, &child_ids);
        self.child_ids = child_ids;
        id
    }

    fn paint(&mut self, bounds: Rect, cx: &mut PaintCx) {
        let viewport = bounds.size;
        // Content extent from children's laid-out (taffy, parent-relative) bounds — the max corner.
        let mut content = Size::new(0.0, 0.0);
        for &child_id in &self.child_ids {
            let local = cx.layout.layout(child_id);
            content.w = content.w.max(local.origin.x + local.size.w);
            content.h = content.h.max(local.origin.y + local.size.h);
        }
        let offset = ScrollState {
            offset: self.current_offset(),
            content,
        }
        .clamp(viewport);

        // Paint children translated by -offset, clipped to the viewport (nested scrolls intersect).
        // Offsetting the absolute origin keeps the RK-007 cascade: parent origin + child local - offset.
        let saved = cx.push_clip(bounds);
        for (child, &child_id) in self.children.iter_mut().zip(&self.child_ids) {
            let local = cx.layout.layout(child_id);
            let child_bounds = Rect {
                origin: Point {
                    x: bounds.origin.x + local.origin.x - offset.x,
                    y: bounds.origin.y + local.origin.y - offset.y,
                },
                size: local.size,
            };
            child.paint(child_bounds, cx);
        }
        cx.set_clip(saved);

        // Overlay scrollbars last, so they take the highest painter order and draw over the content.
        if self.scrollbar {
            self.paint_scrollbars(bounds, content, offset, cx);
        }
    }

    fn handle_event(&mut self, ev: &Event, cx: &mut EventCx) {
        // The scroll container has no handlers of its own (the offset is driven by a ScrollHandle);
        // just descend toward the target so children's handlers run, like `Div`.
        let Some(id) = self.id else {
            return;
        };
        if let Some(next) = cx.next_child_on_path(id) {
            if let Some(i) = self.child_ids.iter().position(|&c| c == next) {
                self.children[i].handle_event(ev, cx);
            }
        }
    }
}

impl IntoElement for Scroll {
    fn into_element(self) -> AnyElement {
        Box::new(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arena::Arena;
    use crate::damage::DamageState;
    use crate::element::div;
    use crate::event::HitTest;
    use crate::paint::render_tree;
    use crate::reactive::provide_context;
    use crate::scheduler::ActiveSources;
    use kagari_layout::LayoutTree;
    use kagari_render::Scene;
    use kagari_style::Theme;
    use kagari_text::{FontDb, TextSystem};
    use reactive_graph::owner::Owner;

    struct NoopDamage;
    impl DamageSink for NoopDamage {
        fn mark_paint_dirty(&self, _id: NodeId) {}
    }

    fn green() -> Background {
        Background::Solid(Color::new(0.0, 1.0, 0.0, 1.0))
    }

    fn frame() -> Duration {
        Duration::from_secs_f32(1.0 / 60.0)
    }

    /// Builds `root`, computes layout at `viewport`, and paints — returning the scene, the layout
    /// tree, and the arena so tests can compare painted vs. taffy (relayout-free) bounds.
    fn build_and_paint(
        root: &mut AnyElement,
        viewport: Size,
    ) -> (Scene, LayoutTree, Arena, NodeId) {
        let mut arena = Arena::new();
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
        let bounds = layout.layout(id);
        root.paint(
            bounds,
            &mut PaintCx::new(&mut scene, &layout, &mut text, None, None, &theme),
        );
        (scene, layout, arena, id)
    }

    #[test]
    fn scroll_should_translate_children_paint_only() {
        // Three 100x50 children (block-stacked at natural y 0/50/100, content 150) in a 100-tall
        // viewport, scrolled by 25. The middle child paints at natural y (50) - offset (25) = 25,
        // while its taffy layout stays at 50 — proving the translate is paint-only, no relayout.
        let mut root = scroll()
            .size(Size { w: 100.0, h: 100.0 })
            .hide_scrollbar()
            .offset(Point::new(0.0, 25.0))
            .child(div().size(Size { w: 100.0, h: 50.0 }).background(green()))
            .child(div().size(Size { w: 100.0, h: 50.0 }).background(green()))
            .child(div().size(Size { w: 100.0, h: 50.0 }).background(green()))
            .into_element();

        let (scene, layout, arena, root_id) =
            build_and_paint(&mut root, Size { w: 100.0, h: 100.0 });

        // All three children overlap the viewport (painted y -25/25/75), so three quads emit.
        assert_eq!(
            scene.quads.len(),
            3,
            "three visible child quads (scrollbar hidden)"
        );
        assert_eq!(
            scene.quads[1].bounds.origin.y, 25.0,
            "the middle child paints at natural y (50) minus the offset (25)"
        );

        // Its taffy layout is unchanged at natural y 50 — the offset never fed the layout tree.
        let child1 = arena.get(root_id).unwrap().children[1];
        assert_eq!(
            layout.layout(child1).origin.y,
            50.0,
            "the child's laid-out y is untouched by scrolling (paint-only)"
        );
    }

    #[test]
    fn scroll_should_clip_to_container_bounds() {
        // Partial clip: offset 25 pushes the first child to painted y -25..25; only y 0..25 is
        // inside the viewport, so its content_mask is clipped to height 25 (not the full 50), while
        // its geometry bounds keep the un-clipped translated origin (-25).
        let mut partial = scroll()
            .size(Size { w: 100.0, h: 100.0 })
            .hide_scrollbar()
            .offset(Point::new(0.0, 25.0))
            .child(div().size(Size { w: 100.0, h: 50.0 }).background(green()))
            .child(div().size(Size { w: 100.0, h: 50.0 }).background(green()))
            .child(div().size(Size { w: 100.0, h: 50.0 }).background(green()))
            .into_element();
        let (scene, _l, _a, _r) = build_and_paint(&mut partial, Size { w: 100.0, h: 100.0 });
        assert_eq!(
            scene.quads[0].bounds.origin.y, -25.0,
            "geometry keeps the translated origin"
        );
        assert_eq!(
            scene.quads[0].content_mask.rect.origin.y, 0.0,
            "the content mask starts at the viewport top"
        );
        assert_eq!(
            scene.quads[0].content_mask.rect.size.h, 25.0,
            "the content mask is clipped to the visible 25px (not the full 50)"
        );

        // Full clip: offset 50 (the max for content 150 / viewport 100) pushes the first child fully
        // above the viewport (painted y -50..0) — its quad is skipped entirely, leaving two.
        let mut full = scroll()
            .size(Size { w: 100.0, h: 100.0 })
            .hide_scrollbar()
            .offset(Point::new(0.0, 50.0))
            .child(div().size(Size { w: 100.0, h: 50.0 }).background(green()))
            .child(div().size(Size { w: 100.0, h: 50.0 }).background(green()))
            .child(div().size(Size { w: 100.0, h: 50.0 }).background(green()))
            .into_element();
        let (scene, _l, _a, _r) = build_and_paint(&mut full, Size { w: 100.0, h: 100.0 });
        assert_eq!(
            scene.quads.len(),
            2,
            "the fully-scrolled-out child emits no quad"
        );
    }

    #[test]
    fn scroll_should_clip_hit_region_out_of_view() {
        // An interactive first child scrolled fully above the viewport records a hit region whose
        // clip is empty, so it is never picked; unscrolled, the same point picks it.
        let interactive = || {
            scroll()
                .size(Size { w: 100.0, h: 100.0 })
                .hide_scrollbar()
                .child(div().size(Size { w: 100.0, h: 50.0 }).on_click(|_e, _c| {}))
                .child(div().size(Size { w: 100.0, h: 50.0 }).background(green()))
                .child(div().size(Size { w: 100.0, h: 50.0 }).background(green()))
        };

        // Control: no scroll → the first child (y 0..50) is picked at (50, 25).
        let mut unscrolled = interactive().into_element();
        {
            let mut arena = Arena::new();
            let mut layout = LayoutTree::new();
            let mut text = TextSystem::new(FontDb::new());
            let damage: Arc<dyn DamageSink> = Arc::new(NoopDamage);
            let theme = Theme::default();
            let id = unscrolled.request_layout(&mut LayoutCx {
                arena: &mut arena,
                layout: &mut layout,
                text: &mut text,
                damage,
                theme: &theme,
                focus: None,
                cursor: None,
            });
            layout.compute(id, Size { w: 100.0, h: 100.0 }).unwrap();
            let mut scene = Scene::new();
            let mut hit = HitTest::new();
            unscrolled.paint(
                layout.layout(id),
                &mut PaintCx::new(&mut scene, &layout, &mut text, None, Some(&mut hit), &theme),
            );
            assert!(
                hit.pick(Point::new(50.0, 25.0)).is_some(),
                "unscrolled, the interactive first child is pickable"
            );
        }

        // Scrolled by 50: the first child moves to painted y -50..0 → its clip is empty → no pick.
        let mut scrolled = interactive().offset(Point::new(0.0, 50.0)).into_element();
        {
            let mut arena = Arena::new();
            let mut layout = LayoutTree::new();
            let mut text = TextSystem::new(FontDb::new());
            let damage: Arc<dyn DamageSink> = Arc::new(NoopDamage);
            let theme = Theme::default();
            let id = scrolled.request_layout(&mut LayoutCx {
                arena: &mut arena,
                layout: &mut layout,
                text: &mut text,
                damage,
                theme: &theme,
                focus: None,
                cursor: None,
            });
            layout.compute(id, Size { w: 100.0, h: 100.0 }).unwrap();
            let mut scene = Scene::new();
            let mut hit = HitTest::new();
            scrolled.paint(
                layout.layout(id),
                &mut PaintCx::new(&mut scene, &layout, &mut text, None, Some(&mut hit), &theme),
            );
            // The second child (non-interactive) now occupies (50, 25); the scrolled-out first child
            // records only an empty-clip region, so the pick misses entirely.
            assert!(
                hit.pick(Point::new(50.0, 25.0)).is_none(),
                "the scrolled-out interactive child is clipped away and not pickable"
            );
        }
    }

    #[test]
    fn scroll_should_emit_overlay_scrollbar_thumbs() {
        // One 200x200 child in a 100x100 viewport overflows both axes, so both a vertical and a
        // horizontal thumb are emitted (above the child), each half the track (100 * 100 / 200 = 50).
        let mut root = scroll()
            .size(Size { w: 100.0, h: 100.0 })
            .child(div().size(Size { w: 200.0, h: 200.0 }).background(green()))
            .into_element();
        let (scene, _l, _a, _r) = build_and_paint(&mut root, Size { w: 100.0, h: 100.0 });

        // quads: [child, vertical thumb, horizontal thumb].
        assert_eq!(scene.quads.len(), 3, "child + two overflow thumbs");
        let vertical = &scene.quads[1];
        let horizontal = &scene.quads[2];
        assert_eq!(
            vertical.bounds.size.w, SCROLLBAR_THICKNESS,
            "vertical thumb is thin"
        );
        assert_eq!(
            vertical.bounds.size.h, 50.0,
            "vertical thumb length is the viewport/content ratio (100/200 of 100)"
        );
        assert_eq!(
            horizontal.bounds.size.h, SCROLLBAR_THICKNESS,
            "horizontal thumb is thin"
        );
        assert_eq!(
            horizontal.bounds.size.w, 50.0,
            "horizontal thumb length reflects the ratio"
        );
        assert!(
            vertical.order > scene.quads[0].order && horizontal.order > scene.quads[0].order,
            "thumbs draw above the content"
        );
    }

    #[test]
    fn scroll_nested_should_intersect_clips() {
        // An inner scroll (100x100) inside a shorter outer scroll (100x60): the inner's clip is the
        // INTERSECTION of both viewports (push_clip intersects, not replaces). The inner's second
        // child (y 50..100) is visible only where both allow — 50..60 — so its content mask is
        // clipped to height 10, not the inner viewport's 50. That exercises push_clip's Some-arm.
        let inner = scroll()
            .size(Size { w: 100.0, h: 100.0 })
            .hide_scrollbar()
            .child(div().size(Size { w: 100.0, h: 50.0 }).background(green()))
            .child(div().size(Size { w: 100.0, h: 50.0 }).background(green()));
        let mut root = scroll()
            .size(Size { w: 100.0, h: 60.0 })
            .hide_scrollbar()
            .child(inner)
            .into_element();

        let (scene, _l, _a, _r) = build_and_paint(&mut root, Size { w: 100.0, h: 60.0 });
        assert_eq!(
            scene.quads.len(),
            2,
            "both inner children overlap the intersected viewport"
        );
        assert_eq!(scene.quads[1].content_mask.rect.origin.y, 50.0);
        assert_eq!(
            scene.quads[1].content_mask.rect.size.h, 10.0,
            "the inner second child is clipped to the outer∩inner viewport (60-50=10), not the inner 50"
        );
    }

    #[test]
    fn scroll_should_omit_scrollbar_when_content_fits() {
        // Content smaller than the viewport → no overflow → no thumbs (only the child quad), even
        // with scrollbars enabled (the default). Exercises the `thumb() == None` element path.
        let mut root = scroll()
            .size(Size { w: 100.0, h: 100.0 })
            .child(div().size(Size { w: 80.0, h: 80.0 }).background(green()))
            .into_element();
        let (scene, _l, _a, _r) = build_and_paint(&mut root, Size { w: 100.0, h: 100.0 });
        assert_eq!(
            scene.quads.len(),
            1,
            "content fits → child quad only, no scrollbar thumbs"
        );
    }

    #[test]
    fn scroll_should_clamp_over_scroll() {
        // An offset far beyond the max (content 150 / viewport 100 → max 50) is clamped to 50, so
        // the render matches an exact-max offset — proving Scroll::paint applies ScrollState::clamp.
        let build = |off: f32| {
            scroll()
                .size(Size { w: 100.0, h: 100.0 })
                .hide_scrollbar()
                .offset(Point::new(0.0, off))
                .child(div().size(Size { w: 100.0, h: 50.0 }).background(green()))
                .child(div().size(Size { w: 100.0, h: 50.0 }).background(green()))
                .child(div().size(Size { w: 100.0, h: 50.0 }).background(green()))
                .into_element()
        };
        let mut over = build(500.0);
        let (over_scene, _l, _a, _r) = build_and_paint(&mut over, Size { w: 100.0, h: 100.0 });
        let mut maxed = build(50.0);
        let (max_scene, _l, _a, _r) = build_and_paint(&mut maxed, Size { w: 100.0, h: 100.0 });
        assert_eq!(
            over_scene.quads.len(),
            max_scene.quads.len(),
            "over-scroll clamps to the same visible set as the exact-max offset"
        );
        assert_eq!(
            over_scene.quads.last().unwrap().bounds.origin.y,
            max_scene.quads.last().unwrap().bounds.origin.y,
            "an over-scroll offset is clamped to the max, matching the exact-max render"
        );
    }

    #[test]
    fn scroll_should_paint_empty_without_panic() {
        // A scroll with no children paints nothing (no content extent, no thumb) and does not panic.
        let mut root = scroll().size(Size { w: 100.0, h: 100.0 }).into_element();
        let (scene, _l, _a, _r) = build_and_paint(&mut root, Size { w: 100.0, h: 100.0 });
        assert!(scene.quads.is_empty(), "an empty scroll emits no quads");
    }

    #[test]
    fn scroll_handle_should_drive_offset() {
        // `Animated::new` needs an owner; keep it alive for the whole test (RK-003).
        let owner = Owner::new();
        owner.set();

        let handle = ScrollHandle::new();
        assert_eq!(
            handle.offset(),
            Point::new(0.0, 0.0),
            "starts at the origin"
        );

        // jump_to is instant (no animation) — the offset moves immediately.
        handle.jump_to(Point::new(0.0, 30.0));
        assert_eq!(
            handle.offset(),
            Point::new(0.0, 30.0),
            "jump_to snaps to the offset immediately"
        );

        // scroll_to animates: the offset does not reach the target until the spring converges.
        handle.scroll_to(Point::new(0.0, 80.0));
        assert_ne!(
            handle.offset(),
            Point::new(0.0, 80.0),
            "scroll_to does not snap"
        );
        let mut ticks = 0;
        while handle.tick(frame()) {
            ticks += 1;
            assert!(ticks < 10_000);
        }
        assert_eq!(
            handle.offset(),
            Point::new(0.0, 80.0),
            "scroll_to settles at the target"
        );

        drop(owner);
    }

    #[test]
    fn scroll_reactive_offset_should_repaint_paint_only() {
        // A between-frames `scroll_to` re-resolves the offset (paint-dirty) and shifts the painted
        // children, while their taffy layout is unchanged. Synchronous effect → hang-free (RK-005);
        // keep the owner alive (RK-003); the write is serial with the frame loop (RK-009).
        let owner = Owner::new();
        owner.set();

        let handle = ScrollHandle::new();
        let mut root = scroll()
            .size(Size { w: 100.0, h: 100.0 })
            .hide_scrollbar()
            .offset(&handle)
            .child(div().size(Size { w: 100.0, h: 50.0 }).background(green()))
            .child(div().size(Size { w: 100.0, h: 50.0 }).background(green()))
            .child(div().size(Size { w: 100.0, h: 50.0 }).background(green()))
            .into_element();

        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let theme = Theme::default();
        let damage = Arc::new(DamageState::default());
        let viewport = Size { w: 100.0, h: 100.0 };

        // Frame 1 at offset 0: the middle child paints at its natural y = 50.
        let mut scene = Scene::new();
        render_tree(
            &mut root,
            &mut arena,
            &mut layout,
            &mut text,
            None,
            None,
            None,
            None,
            None,
            None,
            &mut scene,
            viewport,
            &damage,
            &theme,
        )
        .unwrap();
        assert_eq!(
            scene.quads[1].bounds.origin.y, 50.0,
            "frame 1: middle child at natural y"
        );

        // Scroll between frames: jump_to writes the offset instantly (paint-only). The effect
        // re-resolves the offset and flags paint-damage only — nothing becomes layout-dirty, so the
        // relayout-free (paint-only) contract holds.
        handle.jump_to(Point::new(0.0, 25.0));
        assert!(damage.is_dirty(), "the offset write flags damage");
        assert!(
            damage.take_layout_dirty().is_empty(),
            "scrolling is paint-only — nothing is layout-dirty"
        );

        // Frame 2: the child paints 25px higher (the offset moved the paint, not the layout).
        let mut scene = Scene::new();
        render_tree(
            &mut root,
            &mut arena,
            &mut layout,
            &mut text,
            None,
            None,
            None,
            None,
            None,
            None,
            &mut scene,
            viewport,
            &damage,
            &theme,
        )
        .unwrap();
        assert_eq!(
            scene.quads[1].bounds.origin.y, 25.0,
            "frame 2: middle child scrolled up 25"
        );

        drop(owner);
    }

    #[test]
    fn scroll_to_should_animate_not_snap() {
        // Requirement: scroll_to animates rather than snapping.
        let owner = Owner::new();
        owner.set();

        let handle = ScrollHandle::new();
        handle.scroll_to(Point::new(0.0, 60.0));

        // First tick: moving but not yet at the target (animated, not snapped).
        assert!(handle.tick(frame()), "still animating after one tick");
        let mid = handle.offset();
        assert!(
            mid.y > 0.0 && mid.y < 60.0,
            "mid-flight value is between the start and the target ({mid:?})"
        );

        let mut ticks = 0;
        while handle.tick(frame()) {
            ticks += 1;
            assert!(ticks < 10_000);
        }
        assert_eq!(
            handle.offset(),
            Point::new(0.0, 60.0),
            "scroll_to settles at the target"
        );

        drop(owner);
    }

    #[test]
    fn scroll_by_should_accumulate_on_the_target() {
        // Two notches in the same frame (no tick between) must accumulate on the target — retargeting
        // from the momentary value would drop the first delta.
        let owner = Owner::new();
        owner.set();

        let handle = ScrollHandle::new();
        handle.scroll_by(Point::new(0.0, 10.0));
        handle.scroll_by(Point::new(0.0, 10.0));

        let mut ticks = 0;
        while handle.tick(frame()) {
            ticks += 1;
            assert!(ticks < 10_000);
        }
        assert_eq!(
            handle.offset(),
            Point::new(0.0, 20.0),
            "two same-frame notches accumulate to 20 (not 10)"
        );

        drop(owner);
    }

    #[test]
    fn scroll_fling_should_decay_to_rest_paint_only() {
        // Requirement: a pan fling decays smoothly to rest (spring), updating the offset paint-only.
        let owner = Owner::new();
        owner.set();

        let handle = ScrollHandle::new();
        let mut root = scroll()
            .size(Size { w: 100.0, h: 100.0 })
            .hide_scrollbar()
            .offset(&handle)
            .child(div().size(Size { w: 100.0, h: 50.0 }).background(green()))
            .child(div().size(Size { w: 100.0, h: 50.0 }).background(green()))
            .child(div().size(Size { w: 100.0, h: 50.0 }).background(green()))
            .into_element();

        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let theme = Theme::default();
        let damage = Arc::new(DamageState::default());
        let viewport = Size { w: 100.0, h: 100.0 };

        // Frame 1 at rest: the middle child paints at its natural y = 50 (binds the offset effect).
        let mut scene = Scene::new();
        render_tree(
            &mut root,
            &mut arena,
            &mut layout,
            &mut text,
            None,
            None,
            None,
            None,
            None,
            None,
            &mut scene,
            viewport,
            &damage,
            &theme,
        )
        .unwrap();
        assert_eq!(
            scene.quads[1].bounds.origin.y, 50.0,
            "frame 1: middle child at natural y"
        );

        // Fling downward: content 150 tall in a 100 viewport → max offset 50; a hard fling projects
        // past it and clamps to 50.
        let content = Size { w: 100.0, h: 150.0 };
        handle.fling(Point::new(0.0, 1000.0), content, viewport);

        // Drive the spring to rest; each tick is paint-only (nothing becomes layout-dirty).
        let mut ticks = 0;
        while handle.tick(frame()) {
            assert!(
                damage.take_layout_dirty().is_empty(),
                "each fling frame is paint-only — nothing is layout-dirty"
            );
            ticks += 1;
            assert!(ticks < 10_000);
        }
        assert!(
            ticks > 1,
            "the fling animates over several frames (decays, not a snap)"
        );
        assert_eq!(
            handle.offset(),
            Point::new(0.0, 50.0),
            "the fling settles at the clamped rest offset"
        );

        // Final frame at the settled offset 50: the first child (natural y 0..50) scrolls fully out
        // of view (painted −50..0, culled), so the second child now rests at the viewport top (y 0).
        let mut scene = Scene::new();
        render_tree(
            &mut root,
            &mut arena,
            &mut layout,
            &mut text,
            None,
            None,
            None,
            None,
            None,
            None,
            &mut scene,
            viewport,
            &damage,
            &theme,
        )
        .unwrap();
        assert_eq!(scene.quads.len(), 2, "the first child scrolled out of view");
        assert_eq!(
            scene.quads[0].bounds.origin.y, 0.0,
            "the second child rests at the viewport top (content scrolled up by the fling offset)"
        );

        drop(owner);
    }

    #[test]
    fn scroll_fling_should_stop_at_convergence() {
        // Requirement: convergence stops the animation (no idle frames) — the scheduler active source
        // registers on the fling and deregisters on rest.
        let owner = Owner::new();
        owner.set();

        let sources = ActiveSources::new();
        provide_context(sources.clone()); // the handle's `Animated` captures this from context
        let handle = ScrollHandle::new();
        assert_eq!(sources.count(), 0, "idle before a fling");

        let content = Size { w: 100.0, h: 300.0 };
        let viewport = Size { w: 100.0, h: 100.0 };
        handle.fling(Point::new(0.0, 800.0), content, viewport);
        assert_eq!(
            sources.count(),
            1,
            "the fling registers an active source (continuous drive)"
        );

        let mut ticks = 0;
        while handle.tick(frame()) {
            ticks += 1;
            assert!(ticks < 10_000);
        }
        assert_eq!(
            sources.count(),
            0,
            "convergence deregisters — the app returns to on-demand (no idle frames)"
        );

        drop(owner);
    }
}
