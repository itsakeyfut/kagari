//! The [`Overlay`] element (#62): a deferred-draw portal. Its child paints above all normal content,
//! escaping any parent clip — via the overlay order namespace + the [`OverlayRegistry`](crate::OverlayRegistry).
//! It is placed at an absolute point ([`Overlay::position`]) or anchored to a target element with
//! auto-flipping [`Placement`] ([`Overlay::anchor`] / [`AnchorHandle`], #175).

use std::sync::{Arc, Mutex};

use kagari_base::{Color, Corners, Edges, NodeId, Point, Rect, Size};
use kagari_layout::{LayoutStyle, Position};
use kagari_render::{Background, Border, Quad, RoundedRect};

use super::{AnyElement, DamageSink, Element, Event, EventCx, IntoElement, LayoutCx, PaintCx};
use crate::arena::Node;
use crate::event::{FocusId, FocusRegistry};
use crate::overlay::{OverlayLayer, OverlayLayers};
use crate::reactive::prelude::*;
use crate::reactive::{Prop, RwSignal, create_effect, use_context};

/// A modal backdrop's scrim color (#219): a semi-transparent black behind the overlay content. Hardcoded
/// for the MVP substrate; a theme-token backdrop is post-MVP.
fn backdrop_scrim() -> Color {
    Color::new(0.0, 0.0, 0.0, 0.5)
}

/// An anchored overlay's side preference relative to its anchor element (#175).
///
/// `#[non_exhaustive]`: placement is a growth area (left/right/corner sides are common), so more
/// variants can be added without a breaking change — matching [`CursorIcon`](crate::CursorIcon).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[non_exhaustive]
pub enum Placement {
    /// Below the anchor (the menu/dropdown default).
    #[default]
    Below,
    /// Above the anchor.
    Above,
    /// Below, flipping to above when below would overflow the bottom window edge.
    Auto,
}

/// Resolves an anchored overlay's absolute top-left from the `anchor` rect (absolute), the overlay's
/// `size`, the window `viewport`, and the side `placement`. `Auto` flips to above when below would
/// overflow the bottom edge and above fits; the cross axis is clamped to keep the overlay on-screen.
pub(crate) fn resolve_placement(
    anchor: Rect,
    size: Size,
    viewport: Size,
    placement: Placement,
) -> Point {
    let below_y = anchor.origin.y + anchor.size.h;
    let above_y = anchor.origin.y - size.h;
    let y = match placement {
        Placement::Below => below_y,
        Placement::Above => above_y,
        Placement::Auto => {
            let below_overflows = below_y + size.h > viewport.h;
            let above_fits = above_y >= 0.0;
            if below_overflows && above_fits {
                above_y
            } else {
                below_y
            }
        }
    };
    // Cross axis: left-align to the anchor, clamped so the overlay's full width stays on-screen.
    let max_x = (viewport.w - size.w).max(0.0);
    let x = anchor.origin.x.clamp(0.0, max_x);
    Point::new(x, y)
}

/// A clone-shared handle identifying an anchor element for [`Overlay::anchor`]. Tag the anchor with
/// `div().anchor_ref(&handle)` — it records its `NodeId` here at build; the overlay reads it at paint
/// (build finishes before paint, so the tag/overlay build order is irrelevant). Mirrors
/// [`FocusHandle`](crate::event::FocusHandle)'s "hold a handle before the element mounts" ergonomics.
#[derive(Clone, Default)]
pub struct AnchorHandle {
    node: Arc<Mutex<Option<NodeId>>>,
}

impl AnchorHandle {
    /// A new, unbound handle.
    pub fn new() -> Self {
        Self::default()
    }

    /// The anchor's `NodeId`, once its element has been built (else `None`).
    pub fn node_id(&self) -> Option<NodeId> {
        self.node.lock().ok().and_then(|n| *n)
    }

    /// Records the anchor element's `NodeId` — called by `anchor_ref` at build.
    pub(crate) fn set(&self, node: NodeId) {
        if let Ok(mut slot) = self.node.lock() {
            *slot = Some(node);
        }
    }
}

/// Where an overlay's content is placed: an absolute window point (from the reactive `position` prop),
/// or anchored to an element.
enum OverlayPlacement {
    /// An absolute window point, resolved from the [`Overlay::position`] prop into `resolved_position`.
    Absolute,
    Anchored {
        anchor: AnchorHandle,
        placement: Placement,
    },
}

/// A portal overlay: its `child` is deferred-drawn at an absolute window `position` (static or
/// reactive — a reactive point lets it follow the cursor, e.g. a drag-image, #172), above all normal
/// content and clipped only to itself (escaping a parent clip / scroll). Built with [`overlay`]. Its
/// own node takes no space in normal flow.
pub struct Overlay {
    child: Option<AnyElement>,
    placement: OverlayPlacement,
    /// The absolute-placement position prop (static or reactive); taken + bound at build.
    position: Option<Prop<Point>>,
    /// Resolved absolute position: written by the bound effect (or once, for a static prop), read by
    /// `paint`. Shared + `'static` so the effect can own a handle (mirrors `Scroll`'s `resolved_offset`).
    resolved_position: Arc<Mutex<Option<Point>>>,
    id: Option<NodeId>,
    child_id: Option<NodeId>,
    /// Controllable open state (#219): when `Some`, the overlay is interactive — it paints/registers a
    /// layer only while `true`, and its open-state effect saves/restores focus + repaints on toggle. When
    /// `None`, the overlay is a plain (always-drawn, non-interactive) portal, e.g. a drag-image (#172).
    open: Option<RwSignal<bool>>,
    /// Dismiss on an outside press / Esc (opt-in).
    dismiss_on_outside: bool,
    /// Confine Tab/Shift+Tab within this overlay while it is topmost (opt-in).
    trap_focus: bool,
    /// Paint a modal backdrop + block input to content beneath (opt-in).
    backdrop: bool,
    /// Optional dismissal callback, run when the overlay is dismissed.
    on_dismiss: Option<Arc<dyn Fn() + Send + Sync>>,
    /// The window's interactive layer stack, read from context at build (absent in GPU-free tests).
    layers: Option<OverlayLayers>,
    /// The window's focus registry (#218), read from context at build, for focus save/restore.
    focus: Option<FocusRegistry>,
    /// Tracks the open transition for the open-state effect (previous open value + saved focus).
    open_state: Arc<Mutex<OpenState>>,
}

/// The open-state effect's cross-frame memory: the previous `open` value (to detect transitions) and the
/// [`FocusId`] saved on open, restored on close (focus trap return-focus, #218/#219).
#[derive(Default)]
struct OpenState {
    prev: bool,
    saved: Option<FocusId>,
}

/// Creates an overlay portal wrapping `child`. It paints at an absolute `(0, 0)` until placed with
/// [`Overlay::position`] (absolute) or [`Overlay::anchor`] (relative to an element).
pub fn overlay(child: impl IntoElement) -> Overlay {
    Overlay {
        child: Some(child.into_element()),
        placement: OverlayPlacement::Absolute,
        position: None,
        resolved_position: Arc::new(Mutex::new(None)),
        id: None,
        child_id: None,
        open: None,
        dismiss_on_outside: false,
        trap_focus: false,
        backdrop: false,
        on_dismiss: None,
        layers: None,
        focus: None,
        open_state: Arc::new(Mutex::new(OpenState::default())),
    }
}

impl Overlay {
    /// Places the overlay content at an absolute window position (top-left). Accepts a static
    /// [`Point`] or a reactive point ([`rx`](crate::reactive::rx)/handle) — a reactive position lets
    /// the overlay follow a moving point, e.g. a drag-image tracking the cursor (#172).
    pub fn position(mut self, position: impl Into<Prop<Point>>) -> Self {
        self.position = Some(position.into());
        self.placement = OverlayPlacement::Absolute;
        self
    }

    /// Anchors the overlay relative to the element tagged by `anchor` (via `div().anchor_ref(&h)`),
    /// with a side `placement` (`Auto` flips below→above at the bottom edge; the cross axis clamps
    /// on-screen). Resolved each frame at paint from the anchor's laid-out bounds (#175).
    pub fn anchor(mut self, anchor: &AnchorHandle, placement: Placement) -> Self {
        self.placement = OverlayPlacement::Anchored {
            anchor: anchor.clone(),
            placement,
        };
        self.position = None;
        self
    }

    /// Makes the overlay interactive with a controlled open signal (#219, D2): the overlay paints and
    /// registers a layer only while the signal is `true`. Opening saves the focused element and closing
    /// restores it; toggling repaints. Without `.open(..)` the overlay is a plain always-drawn portal.
    ///
    /// Drive the overlay's **mount/visibility from this same `open` signal** (e.g. wrap content in
    /// `dyn_if(open, ..)`): return-focus runs on the close transition of the effect, so unmounting the
    /// overlay while it is still `open` (via a *different* condition) disposes the effect without restoring
    /// focus.
    pub fn open(mut self, open: RwSignal<bool>) -> Self {
        self.open = Some(open);
        self
    }

    /// Dismiss this overlay on an outside pointer press or Esc (topmost/innermost-first, #219). The
    /// dismissing press is consumed; a modal `backdrop` also blocks it from content beneath.
    pub fn dismiss_on_outside(mut self, dismiss: bool) -> Self {
        self.dismiss_on_outside = dismiss;
        self
    }

    /// Confine Tab / Shift+Tab within this overlay's focusables while it is the topmost open overlay
    /// (#219, on #218). Focus is saved on open and restored on close.
    pub fn trap_focus(mut self, trap: bool) -> Self {
        self.trap_focus = trap;
        self
    }

    /// Paint a modal backdrop (a full-viewport scrim beneath the content) that blocks input to content
    /// beneath (#219). Combine with `.dismiss_on_outside(true)` for a backdrop-click-to-dismiss modal.
    pub fn backdrop(mut self, backdrop: bool) -> Self {
        self.backdrop = backdrop;
        self
    }

    /// Runs `on_dismiss` when the overlay is dismissed (outside press / Esc), after clearing its open
    /// signal — e.g. to notify a parent or run teardown (#219).
    pub fn on_dismiss(mut self, on_dismiss: impl Fn() + Send + Sync + 'static) -> Self {
        self.on_dismiss = Some(Arc::new(on_dismiss));
        self
    }

    /// Registers the open-state effect (#219): a synchronous effect (ADR 0001 / RK-005) that subscribes
    /// to the `open` signal and, on each transition, saves the focused [`FocusId`] on open / restores it
    /// on close (focus-trap return, #218) and flags paint-damage so the toggle repaints. Registered once
    /// at build under the window owner (so it lives for the window — the redraw builds under
    /// `self._owner`), a no-op when the overlay is not interactive (`open` is `None`).
    fn bind_open(&self, id: NodeId, damage: &Arc<dyn DamageSink>) {
        let Some(open) = self.open else {
            return;
        };
        let focus = self.focus.clone();
        let state = Arc::clone(&self.open_state);
        let damage = Arc::clone(damage);
        create_effect(move || {
            let now = open.get();
            // Compute the focus target to restore (close transition) while holding the lock, then release
            // it *before* writing `focus.current()` — `set` fires subscribers synchronously, and the
            // `open_state` mutex is not reentrant (same discipline as `dismiss_topmost`/`step_within`).
            let restore = {
                let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
                if now == st.prev {
                    return;
                }
                st.prev = now;
                match (&focus, now) {
                    // Opened: remember what had focus so it can be restored on close (untracked read).
                    (Some(focus), true) => {
                        st.saved = focus.current().get_untracked();
                        None
                    }
                    // Closed: hand the saved id back out to restore after the lock is released.
                    (Some(_), false) => st.saved.take(),
                    (None, _) => None,
                }
            };
            if let (Some(focus), Some(saved)) = (&focus, restore) {
                focus.current().set(Some(saved));
            }
            // Paint reads `open` untracked, so a toggle must flag damage to repaint (damage-driven redraw).
            damage.mark_paint_dirty(id);
        });
    }

    /// Resolves the reactive `position` prop into `resolved_position` (the absolute-placement analogue
    /// of `Scroll`'s `bind_offset`). A static prop writes the cell once; a reactive prop registers a
    /// synchronous effect (ADR 0001) that re-resolves and — only when the value changed — writes the
    /// cell and flags **paint**-damage for `id`, so a moving position (a drag-image tracking the
    /// cursor) repaints. Registered once at build; re-fires on external signal writes (RK-009).
    fn bind_position(&mut self, damage: &Arc<dyn DamageSink>, id: NodeId) {
        match self.position.take() {
            Some(Prop::Static(p)) => self.store_position(p),
            Some(Prop::Reactive(read)) => {
                let cell = Arc::clone(&self.resolved_position);
                let damage = Arc::clone(damage);
                create_effect(move || {
                    let p = read();
                    if let Ok(mut slot) = cell.lock() {
                        if *slot != Some(p) {
                            *slot = Some(p);
                            damage.mark_paint_dirty(id);
                        }
                    }
                });
            }
            None => {}
        }
    }

    fn store_position(&self, p: Point) {
        if let Ok(mut slot) = self.resolved_position.lock() {
            *slot = Some(p);
        }
    }

    fn current_position(&self) -> Point {
        self.resolved_position
            .lock()
            .ok()
            .and_then(|slot| *slot)
            .unwrap_or(Point::new(0.0, 0.0))
    }
}

impl Element for Overlay {
    fn request_layout(&mut self, cx: &mut LayoutCx) -> NodeId {
        let id = match self.id {
            Some(id) => id,
            None => {
                let id = cx.arena.insert(Node::default());
                self.id = Some(id);
                cx.layout.insert(id, None);
                // Absolute: the overlay is taken out of normal flow (it does not push siblings) and
                // sizes to its content, so a content-sized child (a menu/tooltip bubble with no explicit
                // size) lays out at its natural size instead of collapsing. Painted at the resolved
                // absolute position (the node's own flow position is ignored).
                cx.layout.set_style(
                    id,
                    &LayoutStyle {
                        position: Position::Absolute,
                        ..LayoutStyle::default()
                    },
                );
                // Read the interactive substrate handles from context (#219): the window's overlay layer
                // stack + focus registry (#218). Absent in GPU-free tests → the overlay stays a plain,
                // non-interactive portal. The redraw builds under the window owner, so context resolves
                // and the open-state effect survives (RK-008).
                self.layers = use_context::<OverlayLayers>();
                self.focus = use_context::<FocusRegistry>();
                self.bind_position(&cx.damage, id);
                self.bind_open(id, &cx.damage);
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

    fn paint(&mut self, _bounds: Rect, cx: &mut PaintCx) {
        // Controllable open (#219): a closed interactive overlay paints nothing and registers no layer.
        if let Some(open) = self.open {
            if !open.get_untracked() {
                return;
            }
        }
        // The overlay ignores its zero-size in-flow `bounds` and paints its child at the resolved
        // window position, deferred to the top painter-order and escaping any parent clip.
        let Some(child_id) = self.child_id else {
            return;
        };
        let overlay_size = cx.layout.layout(child_id).size;
        // Resolve the origin (immutable `&self`) BEFORE taking the child's `&mut` borrow: an absolute
        // point (static or reactive, e.g. a cursor-tracked drag-image), or anchored to a target
        // element's absolute bounds (auto-flip / on-screen clamp). An unbound anchor falls back to (0, 0).
        let origin = match &self.placement {
            OverlayPlacement::Absolute => self.current_position(),
            OverlayPlacement::Anchored { anchor, placement } => match anchor.node_id() {
                Some(anchor_id) => resolve_placement(
                    cx.layout.absolute_layout(anchor_id),
                    overlay_size,
                    cx.viewport(),
                    *placement,
                ),
                None => Point::new(0.0, 0.0),
            },
        };
        let child_bounds = Rect {
            origin,
            size: overlay_size,
        };
        // Register the overlay node (records the entry + hands out its z-slot in registration order);
        // slot 0 when no registry is attached (a lone overlay still draws frontmost).
        let anchor_node = self.id.unwrap_or(child_id);
        let slot = cx.overlay_register(anchor_node, child_bounds).unwrap_or(0);
        // Snapshot the backdrop opt-in before the child's `&mut` borrow.
        let backdrop = self.backdrop;
        let Some(child) = self.child.as_mut() else {
            return;
        };
        // Escape any parent clip and draw the subtree in the overlay's high painter-order band.
        let saved_clip = cx.clip();
        cx.set_clip(None);
        cx.enter_overlay(slot);
        // Modal backdrop (#219): a full-viewport scrim painted first, so it takes the lowest order in the
        // band (beneath the content); it blocks input to content below (the shell consumes outside
        // presses over it).
        if backdrop {
            let vp = cx.viewport();
            let rect = Rect::from_xywh(0.0, 0.0, vp.w, vp.h);
            let order = cx.next_order();
            let mask = cx.clip_rect(rect);
            cx.scene.quads.push(Quad {
                bounds: rect,
                corner_radii: Corners::default(),
                bg: Background::Solid(backdrop_scrim()),
                border: Border {
                    widths: Edges::default(),
                    color: Color::TRANSPARENT,
                },
                content_mask: RoundedRect {
                    rect: mask,
                    radii: Corners::default(),
                },
                order,
            });
        }
        child.paint(child_bounds, cx);
        cx.exit_overlay();
        cx.set_clip(saved_clip);
        // Register the interactive layer (#219) — open overlays only (those built with `.open(..)`). The
        // shell reads this stack for outside/Esc dismiss routing and focus-trap. Registration order =
        // paint order = z order, so the topmost open overlay is the last registered.
        if let (Some(open), Some(layers)) = (self.open, &self.layers) {
            layers.push(OverlayLayer::new(
                anchor_node,
                child_bounds,
                self.dismiss_on_outside,
                self.trap_focus,
                self.backdrop,
                open,
                self.on_dismiss.clone(),
            ));
        }
    }

    fn reconcile(&mut self, cx: &mut LayoutCx) {
        // Container: recurse so nested `dyn_if`/`dyn_list` inside the overlay content reconcile live (#278).
        if let Some(child) = self.child.as_mut() {
            child.reconcile(cx);
        }
    }

    fn handle_event(&mut self, ev: &Event, cx: &mut EventCx) {
        // Descend toward the target so the overlay content's handlers run (dispatch drives the path).
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

impl IntoElement for Overlay {
    fn into_element(self) -> AnyElement {
        Box::new(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arena::Arena;
    use crate::element::{DamageSink, div, scroll};
    use crate::event::HitTest;
    use crate::overlay::OverlayRegistry;
    use crate::reactive::{Owner, provide_context};
    use kagari_base::Color;
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

    /// Builds, lays out, and paints `root` with an overlay registry + hit-test attached, returning
    /// the scene, hit-test, registry, arena, and root id for assertions.
    fn build_paint(
        root: &mut AnyElement,
        viewport: Size,
    ) -> (Scene, HitTest, OverlayRegistry, Arena, NodeId) {
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let damage: std::sync::Arc<dyn DamageSink> = std::sync::Arc::new(NoopDamage);
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
        let mut overlay_reg = OverlayRegistry::new();
        {
            let bounds = layout.layout(id);
            let mut cx = PaintCx::new(&mut scene, &layout, &mut text, None, Some(&mut hit), &theme);
            cx.set_viewport(viewport);
            cx.attach_overlay(&mut overlay_reg);
            root.paint(bounds, &mut cx);
        }
        (scene, hit, overlay_reg, arena, id)
    }

    /// Builds/lays out/paints `root` with NO overlay registry attached, returning just the scene —
    /// exercises the `overlay_register` → `None` → slot-0 fallback.
    fn paint_no_registry(root: &mut AnyElement, viewport: Size) -> Scene {
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let damage: std::sync::Arc<dyn DamageSink> = std::sync::Arc::new(NoopDamage);
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
        scene
    }

    #[test]
    fn overlay_should_paint_above_and_escape_clip() {
        // An overlay inside a 100x100 scroll (which clips to its viewport): the overlay's 80x200
        // child, painted at (10,10), draws above the scrolled content AND is masked only to itself
        // (its 200px height is not clipped to the scroll's viewport).
        let mut root = scroll()
            .size(Size { w: 100.0, h: 100.0 })
            .hide_scrollbar()
            .child(div().size(Size { w: 100.0, h: 50.0 }).background(green()))
            .child(
                overlay(div().size(Size { w: 80.0, h: 200.0 }).background(green()))
                    .position(Point::new(10.0, 10.0)),
            )
            .into_element();

        let (scene, _hit, reg, _arena, _id) = build_paint(&mut root, Size { w: 100.0, h: 100.0 });

        assert_eq!(
            scene.quads.len(),
            2,
            "the scrolled content + the overlay child"
        );
        let content = &scene.quads[0];
        let overlay_q = &scene.quads[1];
        assert!(
            overlay_q.order > content.order,
            "the overlay draws above the content"
        );
        assert!(
            overlay_q.order >= 1 << 28,
            "the overlay uses the high painter-order namespace"
        );
        assert_eq!(
            overlay_q.bounds.origin,
            Point::new(10.0, 10.0),
            "the overlay paints at its absolute position"
        );
        assert_eq!(
            overlay_q.content_mask.rect.size.h, 200.0,
            "the overlay escapes the scroll clip (masked to itself, not the 100px viewport)"
        );
        assert_eq!(reg.entries().len(), 1, "the overlay registered one entry");
    }

    #[test]
    fn overlay_should_content_size_its_child() {
        // #283: an overlay's content-sized (auto) child lays out at its natural size, not collapsed to
        // width 0 by the portal node. Here an auto-size outer (green) wraps an explicit 80x16 inner
        // (blue); the outer must content-size to 80x16. Before the `Position::Absolute` node the outer
        // laid out 0x16 (invisible) — a real tooltip/menu bubble would vanish.
        let inner = Background::Solid(Color::new(0.0, 0.0, 1.0, 1.0));
        let mut root = div()
            .child(
                overlay(
                    div()
                        .background(green())
                        .child(div().size(Size { w: 80.0, h: 16.0 }).background(inner)),
                )
                .position(Point::new(10.0, 10.0)),
            )
            .into_element();

        let (scene, _hit, _reg, _arena, _id) = build_paint(&mut root, Size { w: 200.0, h: 200.0 });

        let bubble = scene
            .quads
            .iter()
            .find(|q| q.bg == green())
            .expect("the auto-size outer bubble paints a quad");
        assert!(
            bubble.order >= 1 << 28,
            "the bubble draws in the overlay band"
        );
        assert_eq!(
            bubble.bounds.size,
            Size { w: 80.0, h: 16.0 },
            "the auto-size overlay child content-sizes to its inner (was 0 wide before #283)"
        );
        assert_eq!(
            bubble.bounds.origin,
            Point::new(10.0, 10.0),
            "the bubble still paints at its placed position"
        );
    }

    #[test]
    fn overlay_should_take_no_flow_space() {
        // #283: the overlay node is out of flow, so content-sizing it (now a non-zero node) must NOT push
        // siblings. A column of [10px spacer, overlay(50px child), 10px spacer]: the trailing spacer sits
        // at y=10 (right after the first), as if the overlay took no space — not y=60 (pushed by 50px).
        let top = Background::Solid(Color::new(1.0, 0.0, 0.0, 1.0));
        let bottom = Background::Solid(Color::new(0.0, 0.0, 1.0, 1.0));
        let mut root = div()
            .flex_col()
            .child(div().size(Size { w: 10.0, h: 10.0 }).background(top))
            .child(
                overlay(div().size(Size { w: 50.0, h: 50.0 }).background(green()))
                    .position(Point::new(0.0, 0.0)),
            )
            .child(div().size(Size { w: 10.0, h: 10.0 }).background(bottom))
            .into_element();

        let (scene, _hit, _reg, _arena, _id) = build_paint(&mut root, Size { w: 200.0, h: 200.0 });

        let trailing = scene
            .quads
            .iter()
            .find(|q| q.bg == bottom)
            .expect("the trailing spacer paints");
        assert_eq!(
            trailing.bounds.origin.y, 10.0,
            "the overlay takes no flow space; the trailing sibling is not pushed down"
        );
    }

    #[test]
    fn overlay_should_hit_test_above_content() {
        // Interactive content with an interactive overlay on top of it: a pick in the overlap
        // resolves the overlay (its hit region has a higher painter order).
        let mut root = div()
            .child(div().size(Size { w: 100.0, h: 100.0 }).on_click(|_, _| {}))
            .child(
                overlay(div().size(Size { w: 50.0, h: 50.0 }).on_click(|_, _| {}))
                    .position(Point::new(0.0, 0.0)),
            )
            .into_element();

        let (_scene, hit, _reg, arena, root_id) =
            build_paint(&mut root, Size { w: 200.0, h: 200.0 });

        // The overlay node is root's 2nd child; its content is the interactive child under it.
        let overlay_node = arena.get(root_id).unwrap().children[1];
        let overlay_child = arena.get(overlay_node).unwrap().children[0];
        assert_eq!(
            hit.pick(Point::new(10.0, 10.0)),
            Some(overlay_child),
            "the overlay is picked over the content beneath it"
        );
    }

    #[test]
    fn overlay_should_stack_in_registration_order() {
        // Two overlapping overlays: the later-registered one (slot 1) draws above the earlier (slot
        // 0) and is picked first.
        let mut root = div()
            .child(
                overlay(
                    div()
                        .size(Size { w: 50.0, h: 50.0 })
                        .background(green())
                        .on_click(|_, _| {}),
                )
                .position(Point::new(0.0, 0.0)),
            )
            .child(
                overlay(
                    div()
                        .size(Size { w: 50.0, h: 50.0 })
                        .background(green())
                        .on_click(|_, _| {}),
                )
                .position(Point::new(0.0, 0.0)),
            )
            .into_element();

        let (scene, hit, reg, arena, root_id) = build_paint(&mut root, Size { w: 200.0, h: 200.0 });

        assert_eq!(reg.entries().len(), 2, "both overlays registered");
        assert_eq!(scene.quads.len(), 2);
        assert!(
            scene.quads[1].order > scene.quads[0].order,
            "the later-registered overlay draws on top"
        );
        let top_overlay = arena.get(root_id).unwrap().children[1];
        let top_child = arena.get(top_overlay).unwrap().children[0];
        assert_eq!(
            hit.pick(Point::new(10.0, 10.0)),
            Some(top_child),
            "the top overlay is picked"
        );
    }

    #[test]
    fn overlay_nested_should_stack_inner_above_outer() {
        // An overlay whose child contains another overlay: the inner (slot 1) draws above the outer
        // (slot 0) — exercising the `overlay_orders` stack (push inner band, pop back to outer).
        let mut root = overlay(
            div()
                .size(Size { w: 100.0, h: 100.0 })
                .background(green())
                .child(
                    overlay(div().size(Size { w: 50.0, h: 50.0 }).background(green()))
                        .position(Point::new(20.0, 20.0)),
                ),
        )
        .position(Point::new(0.0, 0.0))
        .into_element();

        let (scene, _hit, reg, _arena, _id) = build_paint(&mut root, Size { w: 200.0, h: 200.0 });

        assert_eq!(
            reg.entries().len(),
            2,
            "outer + inner overlays register (outer→inner order)"
        );
        assert_eq!(scene.quads.len(), 2, "the outer bg + the inner bg");
        // quads[0] = outer's bg (slot-0 band), quads[1] = inner's bg (slot-1 band).
        assert!(
            scene.quads[1].order > scene.quads[0].order,
            "the inner overlay draws above the outer (higher slot band)"
        );
    }

    #[test]
    fn overlay_should_draw_frontmost_without_a_registry() {
        // With no registry attached, a lone overlay falls back to slot 0 and still draws in the high
        // order band (frontmost, above main content).
        let mut root = div()
            .child(div().size(Size { w: 100.0, h: 100.0 }).background(green()))
            .child(
                overlay(div().size(Size { w: 50.0, h: 50.0 }).background(green()))
                    .position(Point::new(0.0, 0.0)),
            )
            .into_element();

        let scene = paint_no_registry(&mut root, Size { w: 200.0, h: 200.0 });

        assert_eq!(scene.quads.len(), 2);
        let content = &scene.quads[0];
        let overlay_q = &scene.quads[1];
        assert!(
            overlay_q.order >= 1 << 28,
            "the overlay draws in the high band even without a registry"
        );
        assert!(
            overlay_q.order > content.order,
            "still above the main content"
        );
    }

    #[test]
    fn overlay_should_restore_clip_for_later_siblings() {
        // A sibling painted AFTER an overlay inside a scroll must be clipped normally again — the
        // overlay's `set_clip(None)` is restored. The trailing 200px div is clipped to the viewport.
        let mut root = scroll()
            .size(Size { w: 100.0, h: 100.0 })
            .hide_scrollbar()
            .child(
                overlay(div().size(Size { w: 80.0, h: 200.0 }).background(green()))
                    .position(Point::new(10.0, 10.0)),
            )
            .child(div().size(Size { w: 100.0, h: 200.0 }).background(green()))
            .into_element();

        let (scene, _hit, _reg, _arena, _id) = build_paint(&mut root, Size { w: 100.0, h: 100.0 });

        // The trailing sibling is a normal (main-order) scroll child: its content mask is clipped to
        // the ~100px viewport. If the overlay's clip restore were dropped, it would keep its 200px.
        let trailing = scene
            .quads
            .iter()
            .find(|q| q.order < 1 << 28)
            .expect("a main-order sibling quad exists");
        assert_eq!(
            trailing.content_mask.rect.size.h, 100.0,
            "the sibling after the overlay is clipped to the viewport (clip was restored)"
        );
    }

    #[test]
    fn render_tree_should_clear_and_populate_the_overlay_registry() {
        use crate::damage::DamageState;
        use crate::paint::render_tree;

        let mut root = div()
            .child(
                overlay(div().size(Size { w: 50.0, h: 50.0 }).background(green()))
                    .position(Point::new(0.0, 0.0)),
            )
            .into_element();

        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let mut scene = Scene::new();
        let mut reg = OverlayRegistry::new();
        let damage = std::sync::Arc::new(DamageState::default());
        let theme = Theme::default();
        let viewport = Size { w: 200.0, h: 200.0 };

        // Frame 1: the overlay registers one entry and draws in the high band, all via render_tree.
        render_tree(
            &mut root,
            &mut arena,
            &mut layout,
            &mut text,
            None,
            None,
            Some(&mut reg),
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
            reg.entries().len(),
            1,
            "the overlay registered via render_tree"
        );
        assert!(
            scene.quads.iter().any(|q| q.order >= 1 << 28),
            "the overlay drew in the high band"
        );

        // Frame 2: the same registry is cleared each frame — entries do not accumulate.
        render_tree(
            &mut root,
            &mut arena,
            &mut layout,
            &mut text,
            None,
            None,
            Some(&mut reg),
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
            reg.entries().len(),
            1,
            "render_tree clears the registry each frame (no accumulation)"
        );
    }

    #[test]
    fn overlay_position_should_follow_reactive_point() {
        use crate::damage::DamageState;
        use crate::paint::render_tree;
        use crate::reactive::prelude::*;
        use crate::reactive::{RwSignal, rx};
        use reactive_graph::owner::Owner;

        // A reactive-position overlay (the drag-image substrate, #172): as the cursor signal moves,
        // the overlay repaints at the new point. Synchronous effect → hang-free (RK-005); the owner is
        // held to the end (RK-003/008); the write is serial with the frame loop (RK-009).
        let owner = Owner::new();
        owner.set();

        let cursor = RwSignal::new(Point::new(10.0, 20.0));
        let mut root = div()
            .child(
                overlay(div().size(Size { w: 30.0, h: 30.0 }).background(green()))
                    .position(rx(move || cursor.get())),
            )
            .into_element();

        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let mut reg = OverlayRegistry::new();
        let damage = std::sync::Arc::new(DamageState::default());
        let theme = Theme::default();
        let viewport = Size { w: 200.0, h: 200.0 };

        // Frame 1: the overlay paints at the initial cursor (10, 20), in the high overlay band.
        let mut scene = Scene::new();
        render_tree(
            &mut root,
            &mut arena,
            &mut layout,
            &mut text,
            None,
            None,
            Some(&mut reg),
            None,
            None,
            None,
            &mut scene,
            viewport,
            &damage,
            &theme,
        )
        .unwrap();
        let front = scene
            .quads
            .iter()
            .find(|q| q.order >= 1 << 28)
            .expect("the overlay draws in the high band");
        assert_eq!(
            front.bounds.origin,
            Point::new(10.0, 20.0),
            "the overlay paints at the initial cursor"
        );

        // Move the cursor between frames: the bound effect re-resolves the position + flags damage.
        cursor.set(Point::new(120.0, 90.0));
        assert!(damage.is_dirty(), "moving the cursor flags paint-damage");

        // Frame 2: the overlay follows the cursor to (120, 90) — paint-only, no relayout.
        let mut scene = Scene::new();
        render_tree(
            &mut root,
            &mut arena,
            &mut layout,
            &mut text,
            None,
            None,
            Some(&mut reg),
            None,
            None,
            None,
            &mut scene,
            viewport,
            &damage,
            &theme,
        )
        .unwrap();
        let front = scene
            .quads
            .iter()
            .find(|q| q.order >= 1 << 28)
            .expect("the overlay draws in the high band");
        assert_eq!(
            front.bounds.origin,
            Point::new(120.0, 90.0),
            "the overlay follows the cursor to its new point"
        );

        drop(owner);
    }

    #[test]
    fn overlay_reactive_position_effect_should_dispose_on_unmount() {
        use crate::reactive::prelude::*;
        use crate::reactive::{Owner, RwSignal, rx};

        // A drag-image is mounted via dyn_if under a per-child owner; unmounting it (drop/cancel) must
        // dispose its cursor-tracking position effect, so a removed drag-image leaves no effect writing
        // to a dead node (RK-005/006). Synchronous effect → hang-free (RK-005).
        let owner = Owner::new();
        owner.set();

        let cursor = RwSignal::new(Point::new(10.0, 20.0));
        let theme = Theme::default();
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let damage: std::sync::Arc<dyn DamageSink> = std::sync::Arc::new(NoopDamage);

        // Mount under a child owner (as dyn_if does for a conditionally-mounted drag-image).
        let child_owner = owner.child();
        let ov = child_owner.with(|| {
            let mut ov =
                overlay(div().size(Size { w: 20.0, h: 20.0 })).position(rx(move || cursor.get()));
            ov.request_layout(&mut LayoutCx {
                arena: &mut arena,
                layout: &mut layout,
                text: &mut text,
                damage: std::sync::Arc::clone(&damage),
                theme: &theme,
                focus: None,
                cursor: None,
            });
            ov
        });

        // Present: the effect resolved the initial cursor, and follows a live write while mounted.
        assert_eq!(
            ov.current_position(),
            Point::new(10.0, 20.0),
            "resolved to the initial cursor"
        );
        cursor.set(Point::new(50.0, 60.0));
        assert_eq!(
            ov.current_position(),
            Point::new(50.0, 60.0),
            "the live effect follows the cursor while mounted"
        );

        // Unmount: disposing the child owner drops the position effect, so later writes are inert.
        child_owner.cleanup();
        cursor.set(Point::new(90.0, 90.0));
        assert_eq!(
            ov.current_position(),
            Point::new(50.0, 60.0),
            "after unmount the disposed effect ignores cursor writes (no dangling update)"
        );

        drop(owner);
    }

    #[test]
    fn resolve_placement_should_place_below() {
        // Below: the overlay hangs off the anchor's bottom edge, left-aligned to the anchor.
        let anchor = Rect::from_xywh(20.0, 40.0, 60.0, 30.0); // bottom = 70
        let p = resolve_placement(
            anchor,
            Size::new(50.0, 50.0),
            Size::new(200.0, 200.0),
            Placement::Below,
        );
        assert_eq!(p, Point::new(20.0, 70.0));
    }

    #[test]
    fn resolve_placement_should_flip_to_above_on_bottom_overflow() {
        // Auto: an anchor near the bottom (bottom = 90). Below (y 90, +50 = 140 > 100 viewport)
        // overflows, so it flips above: y = anchor.top (80) - overlay.h (50) = 30.
        let anchor = Rect::from_xywh(20.0, 80.0, 60.0, 10.0);
        let p = resolve_placement(
            anchor,
            Size::new(50.0, 50.0),
            Size::new(200.0, 100.0),
            Placement::Auto,
        );
        assert_eq!(
            p.y, 30.0,
            "flips above when below would overflow the bottom edge"
        );
    }

    #[test]
    fn resolve_placement_should_clamp_cross_axis() {
        // An anchor near the right edge: the 80-wide overlay would run off, so x clamps to
        // viewport.w - overlay.w = 200 - 80 = 120.
        let anchor = Rect::from_xywh(180.0, 10.0, 20.0, 20.0);
        let p = resolve_placement(
            anchor,
            Size::new(80.0, 30.0),
            Size::new(200.0, 200.0),
            Placement::Below,
        );
        assert_eq!(
            p.x, 120.0,
            "clamps cross-axis so the overlay stays on-screen"
        );
    }

    #[test]
    fn overlay_anchor_should_flip_on_edge() {
        // An anchor near the bottom of a 200x100 window, anchored `Auto`: the overlay flips above.
        // Column: an 80px spacer, then the 20px anchor (absolute y 80..100), then the anchored overlay.
        let anchor = AnchorHandle::new();
        let mut root = div()
            .flex_col()
            .size(Size { w: 200.0, h: 100.0 })
            .child(div().size(Size { w: 50.0, h: 80.0 }))
            .child(div().size(Size { w: 50.0, h: 20.0 }).anchor_ref(&anchor))
            .child(
                overlay(div().size(Size { w: 50.0, h: 50.0 }).background(green()))
                    .anchor(&anchor, Placement::Auto),
            )
            .into_element();

        let (scene, _hit, _reg, _arena, _id) = build_paint(&mut root, Size { w: 200.0, h: 100.0 });

        // The anchor sits at absolute y 80..100 (near the bottom). Below (100, +50 = 150 > 100)
        // overflows, so Auto flips above: the overlay child paints at y = 80 - 50 = 30.
        assert_eq!(scene.quads.len(), 1, "only the overlay child has a bg quad");
        assert_eq!(
            scene.quads[0].bounds.origin.y, 30.0,
            "Auto flips the overlay above the bottom-edge anchor"
        );
    }

    #[test]
    fn resolve_placement_should_stay_below_when_above_also_overflows() {
        // A tall overlay (95) in a 100 window anchored `Auto`: below (bottom 90 + 95 = 185 > 100)
        // overflows, but above (top 80 - 95 = -15) also overflows the top — so it stays below
        // (the main axis is flip-only, no clamp).
        let anchor = Rect::from_xywh(20.0, 80.0, 60.0, 10.0);
        let p = resolve_placement(
            anchor,
            Size::new(50.0, 95.0),
            Size::new(200.0, 100.0),
            Placement::Auto,
        );
        assert_eq!(
            p.y, 90.0,
            "stays below (= anchor bottom) when above also overflows"
        );
    }

    #[test]
    fn overlay_unbound_anchor_should_paint_at_origin() {
        // An overlay anchored to a handle never bound to an element (no `anchor_ref`) degrades to
        // the origin (0, 0) rather than panicking.
        let handle = AnchorHandle::new();
        let mut root = div()
            .child(
                overlay(div().size(Size { w: 50.0, h: 50.0 }).background(green()))
                    .anchor(&handle, Placement::Auto),
            )
            .into_element();

        let (scene, _hit, _reg, _arena, _id) = build_paint(&mut root, Size { w: 200.0, h: 200.0 });

        assert_eq!(scene.quads.len(), 1);
        assert_eq!(
            scene.quads[0].bounds.origin,
            Point::new(0.0, 0.0),
            "an unbound anchor falls back to the origin"
        );
    }

    #[test]
    fn overlay_anchor_should_clamp_to_screen_edge() {
        // A right-edge anchor (x 180) with an 80-wide overlay: Below places at the anchor's bottom,
        // and the cross axis clamps to viewport.w - overlay.w = 200 - 80 = 120 to stay on-screen.
        let anchor = AnchorHandle::new();
        let mut root = div()
            .flex()
            .size(Size { w: 200.0, h: 100.0 })
            .child(div().size(Size { w: 180.0, h: 20.0 }))
            .child(div().size(Size { w: 20.0, h: 20.0 }).anchor_ref(&anchor))
            .child(
                overlay(div().size(Size { w: 80.0, h: 30.0 }).background(green()))
                    .anchor(&anchor, Placement::Below),
            )
            .into_element();

        let (scene, _hit, _reg, _arena, _id) = build_paint(&mut root, Size { w: 200.0, h: 100.0 });

        // Anchor at (180, 0) size (20, 20): Below → y = 20; x = clamp(180, 0, 120) = 120.
        assert_eq!(scene.quads.len(), 1);
        assert_eq!(
            scene.quads[0].bounds.origin,
            Point::new(120.0, 20.0),
            "places below the anchor and clamps the cross axis on-screen"
        );
    }

    /// Builds + paints `root` (with a viewport + overlay registry), assuming an ambient reactive owner is
    /// set (so `use_context` resolves at build). Returns the scene. Used by the interactive-overlay tests.
    fn paint_interactive(root: &mut AnyElement, viewport: Size) -> Scene {
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let damage: std::sync::Arc<dyn DamageSink> = std::sync::Arc::new(NoopDamage);
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
        let mut overlay_reg = OverlayRegistry::new();
        {
            let bounds = layout.layout(id);
            let mut cx = PaintCx::new(&mut scene, &layout, &mut text, None, None, &theme);
            cx.set_viewport(viewport);
            cx.attach_overlay(&mut overlay_reg);
            root.paint(bounds, &mut cx);
        }
        scene
    }

    #[test]
    fn overlay_open_false_should_paint_nothing_and_not_register() {
        let owner = Owner::new();
        owner.set();
        let layers = OverlayLayers::new();
        provide_context(layers.clone());

        // A closed interactive overlay: its child is still built, but paint gates on `open` → nothing is
        // drawn and no layer is registered.
        let mut root = overlay(div().size(Size { w: 50.0, h: 50.0 }).background(green()))
            .open(RwSignal::new(false))
            .into_element();
        let scene = paint_interactive(&mut root, Size { w: 200.0, h: 200.0 });

        assert!(scene.quads.is_empty(), "a closed overlay paints no quads");
        assert!(
            layers.is_empty(),
            "a closed overlay registers no interactive layer"
        );

        drop(owner);
    }

    #[test]
    fn overlay_backdrop_should_paint_scrim_beneath_child() {
        let owner = Owner::new();
        owner.set();
        let layers = OverlayLayers::new();
        provide_context(layers.clone());

        // An open modal overlay: a full-viewport scrim is painted beneath the child, both in the high
        // overlay band, and the overlay registers an interactive layer.
        let mut root = overlay(div().size(Size { w: 50.0, h: 50.0 }).background(green()))
            .open(RwSignal::new(true))
            .backdrop(true)
            .into_element();
        let scene = paint_interactive(&mut root, Size { w: 200.0, h: 200.0 });

        assert_eq!(scene.quads.len(), 2, "the backdrop scrim + the child bg");
        let backdrop = &scene.quads[0];
        let child = &scene.quads[1];
        assert!(
            backdrop.order < child.order,
            "the backdrop is painted beneath the child content"
        );
        assert!(
            backdrop.order >= 1 << 28,
            "the backdrop is in the high overlay band"
        );
        assert_eq!(
            backdrop.bounds,
            Rect::from_xywh(0.0, 0.0, 200.0, 200.0),
            "the backdrop covers the full viewport"
        );
        assert_eq!(
            backdrop.bg,
            Background::Solid(backdrop_scrim()),
            "the backdrop is the translucent scrim"
        );
        assert!(!layers.is_empty(), "the open overlay registered a layer");

        drop(owner);
    }

    #[test]
    fn overlay_open_toggle_should_save_and_restore_focus() {
        let owner = Owner::new();
        owner.set();
        let layers = OverlayLayers::new();
        provide_context(layers.clone());
        let reg = FocusRegistry::new();
        provide_context(reg.clone());

        // Two focusables; `first` holds focus before the overlay opens.
        let first = reg.handle();
        let second = reg.handle();
        first.focus();
        assert!(first.is_focused(), "the pre-open focus target");

        // Build a trap-focus overlay under the owner: its open-state effect registers here (#219).
        let open = RwSignal::new(false);
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let damage: std::sync::Arc<dyn DamageSink> = std::sync::Arc::new(NoopDamage);
        let theme = Theme::default();
        let mut ov = overlay(div().size(Size { w: 20.0, h: 20.0 }))
            .open(open)
            .trap_focus(true)
            .into_element();
        ov.request_layout(&mut LayoutCx {
            arena: &mut arena,
            layout: &mut layout,
            text: &mut text,
            damage,
            theme: &theme,
            focus: None,
            cursor: None,
        });

        // Open → the effect saves the focused id (`first`). Move focus inside; close → focus returns.
        open.set(true);
        second.focus();
        assert!(second.is_focused(), "focus moved while the overlay is open");
        open.set(false);
        assert_eq!(
            reg.current().get_untracked(),
            Some(first.id()),
            "closing the overlay returns focus to the pre-open target"
        );

        drop(owner);
    }
}
