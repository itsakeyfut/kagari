//! The paint pass (#34): build the element tree, compute layout, and emit the render `Scene`.
//!
//! Orchestrates the three steps that turn a retained element tree into renderable primitives:
//! 1. **build** — `request_layout` populates the arena (#30) and registers nodes/styles/measures
//!    in the layout tree (#33). Build-once: re-running is idempotent.
//! 2. **layout** — `LayoutTree::compute` resolves every node's `Rect`.
//! 3. **paint** — walk the tree, emitting each element's primitives at its computed bounds into
//!    the `Scene`. Text leaves rasterize glyphs into the renderer's `atlas`.

use std::sync::Arc;

use kagari_base::{NodeId, Point, Rect, Size};
use kagari_layout::{LayoutError, LayoutTree};
use kagari_render::{Atlas, Scene};
use kagari_text::TextSystem;

use crate::a11y::A11yTree;
use crate::arena::Arena;
use crate::damage::DamageState;
use crate::element::{AnyElement, DamageSink, LayoutCx, PaintCx};
use crate::event::{CursorRegistry, FocusRegistry, HitTest};
use crate::overlay::OverlayRegistry;

/// A synthetic, fixed overlay order-band for the shell's drag-image (#178) — **not** a real overlay
/// registration index. Chosen high enough that its band (`OVERLAY_ORDER_BASE + slot*STRIDE`, cx.rs)
/// composites above all main content and normal overlays (registered at low indices), while still
/// leaving a full per-slot budget below `u32` saturation. Assumes no real overlay reaches this index.
const DRAG_IMAGE_ORDER_SLOT: usize = 60_000;

/// Builds, lays out, and paints `root` into `scene` for the given `viewport`.
///
/// `scene` is cleared first (its capacity is retained for reuse). `atlas` receives rasterized
/// glyphs; pass `None` for GPU-free Scene-structure tests (text emits no glyphs then). `hit_test`
/// receives the frame's interactive regions (#48); pass `None` to skip hit recording (it is cleared
/// first when present, mirroring `scene`). `focus`/`cursor` receive `track_focus`/`cursor`
/// associations at build (#49/#53, live wiring #177); pass `None` to skip registration. Unlike the
/// per-frame `hit_test`, these are populated on the build-once pass, so the caller keeps them across
/// frames (they are not cleared here).
#[allow(clippy::too_many_arguments)]
pub fn render_tree(
    root: &mut AnyElement,
    arena: &mut Arena,
    layout: &mut LayoutTree,
    text: &mut TextSystem,
    atlas: Option<&mut Atlas>,
    hit_test: Option<&mut HitTest>,
    overlay: Option<&mut OverlayRegistry>,
    focus: Option<&mut FocusRegistry>,
    cursor: Option<&mut CursorRegistry>,
    a11y: Option<&mut A11yTree>,
    scene: &mut Scene,
    viewport: Size,
    damage: &Arc<DamageState>,
    theme: &kagari_style::Theme,
) -> Result<NodeId, LayoutError> {
    let root_id = {
        // `Arc<DamageState>` coerces to the `Arc<dyn DamageSink>` the build context holds.
        let sink: Arc<dyn DamageSink> = Arc::clone(damage) as Arc<dyn DamageSink>;
        let mut layout_cx = LayoutCx {
            arena,
            layout,
            text,
            damage: sink,
            theme,
            // `track_focus`/`cursor` register into these on the build-once pass (#49/#53); the app
            // supplies its per-window registries here (live wiring #177), tests pass `None`.
            focus,
            cursor,
        };
        let id = root.request_layout(&mut layout_cx);
        // Reconcile pass (#278): if any `dyn_if`/`dyn_list` staged a structural change, apply it now — a
        // second build pass over the retained tree (`Element::reconcile`), before the layout-dirty drain
        // + `compute`, so mounts/unmounts take effect this frame (a newly-mounted child registers its
        // styles/measures/focus/cursor; `apply_staged` marks the parent layout-dirty so `compute` relays
        // it out). **Gated** on the structure wake, so an animating window (no structure change) skips
        // the whole-tree walk. Runs under the same `LayoutCx` as the build.
        if !damage.take_structure_dirty().is_empty() {
            root.reconcile(&mut layout_cx);
        }
        id
    };

    // Drain layout-dirty nodes flagged since the last frame so `compute` re-lays-out only those
    // subtrees (clean siblings keep their cached layout). Paint-dirty nodes are left untouched —
    // an appearance change needs no relayout.
    for id in damage.take_layout_dirty() {
        layout.mark_dirty(id);
    }

    layout.compute(root_id, viewport)?;

    scene.clear();
    let root_bounds = layout.layout(root_id);
    // Clear the hit-test for this frame (capacity retained), then record into it during paint.
    let mut hit_test = hit_test;
    if let Some(ht) = hit_test.as_deref_mut() {
        ht.clear();
    }
    // Clear the overlay registry for this frame; overlays re-register during paint (#62).
    let mut overlay = overlay;
    if let Some(reg) = overlay.as_deref_mut() {
        reg.clear();
    }
    // Clear the a11y sink for this frame; annotated elements re-record during paint (#67) — a
    // per-frame record (absolute bounds), not a build-once registry (RK-019).
    let mut a11y = a11y;
    if let Some(tree) = a11y.as_deref_mut() {
        tree.clear();
    }
    let mut paint_cx = PaintCx::new(scene, layout, text, atlas, hit_test, theme);
    paint_cx.set_viewport(viewport);
    if let Some(reg) = overlay {
        paint_cx.attach_overlay(reg);
    }
    if let Some(tree) = a11y {
        paint_cx.attach_a11y(tree);
    }
    root.paint(root_bounds, &mut paint_cx);

    // The frame consumed this damage. The GPU repaints the whole window for now (§1.4), so the
    // paint-dirty set and `damage_rect` are not read here — the frame scheduler (#36) will poll
    // `is_dirty`/`damage_rect` to gate redraws and drive partial redraw; such a consumer must run
    // before this `clear` (and before the layout-dirty drain above) to see the full damage set.
    damage.clear();
    // Return the root node id so the shell can walk the tree (e.g. the debug overlay's element
    // bounds, #69). `render_tree` remains an idempotent build-once + paint pass otherwise.
    Ok(root_id)
}

/// Paints one detached `element` into an **existing** `scene` (without clearing it) with its top-left
/// at `origin`, above all main content and overlays (a high overlay order band) and **not** hit-tested
/// — the shell's cursor-tracked drag-image (#178). Builds + lays the element out in its own
/// `arena`/`layout` (fresh per drag), so it is independent of the main tree. Call after `render_tree`.
#[allow(clippy::too_many_arguments)]
pub fn paint_element_into(
    element: &mut AnyElement,
    arena: &mut Arena,
    layout: &mut LayoutTree,
    text: &mut TextSystem,
    atlas: Option<&mut Atlas>,
    scene: &mut Scene,
    origin: Point,
    viewport: Size,
    damage: &Arc<DamageState>,
    theme: &kagari_style::Theme,
) -> Result<(), LayoutError> {
    let root_id = {
        let sink: Arc<dyn DamageSink> = Arc::clone(damage) as Arc<dyn DamageSink>;
        let mut layout_cx = LayoutCx {
            arena,
            layout,
            text,
            damage: sink,
            theme,
            focus: None,
            cursor: None,
        };
        element.request_layout(&mut layout_cx)
    };
    layout.compute(root_id, viewport)?;
    let size = layout.layout(root_id).size;
    let bounds = Rect { origin, size };
    // `None` hit-test: the drag-image is visual only and must not intercept pointer events (drop
    // targets under it stay reachable). The overlay band puts it above all normal primitives.
    let mut paint_cx = PaintCx::new(scene, layout, text, atlas, None, theme);
    paint_cx.set_viewport(viewport);
    paint_cx.enter_overlay(DRAG_IMAGE_ORDER_SLOT);
    element.paint(bounds, &mut paint_cx);
    paint_cx.exit_overlay();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::{Element, IntoElement, LayoutCx, div, text};
    use kagari_base::{Color, NodeId};
    use kagari_render::{Background, Scene};
    use kagari_style::Theme;
    use kagari_text::FontDb;

    struct NoopDamage;
    impl DamageSink for NoopDamage {
        fn mark_paint_dirty(&self, _id: NodeId) {}
    }

    #[test]
    fn render_tree_should_emit_div_quad_with_computed_bounds() {
        let green = Background::Solid(Color::new(0.0, 1.0, 0.0, 1.0));
        let mut root = div().background(green).child(text("Hi")).into_element();

        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text_system = TextSystem::new(FontDb::new());
        let mut scene = Scene::new();
        let damage = Arc::new(DamageState::default());
        let theme = Theme::default();

        render_tree(
            &mut root,
            &mut arena,
            &mut layout,
            &mut text_system,
            None, // GPU-free: text emits no glyphs; we assert the div quad + its layout.
            None, // no hit-test recording here
            None, // no overlay
            None, // no focus registry
            None, // no cursor registry
            None, // no a11y sink
            &mut scene,
            Size { w: 200.0, h: 100.0 },
            &damage,
            &theme,
        )
        .unwrap();

        // The div emits exactly one quad, with its declared background.
        assert_eq!(scene.quads.len(), 1, "the div emits one background quad");
        assert_eq!(scene.quads[0].bg, green);
        // The div is sized around its text content on the cross axis, not stretched to the
        // viewport — a single text line is far shorter than the 100px viewport height.
        let h = scene.quads[0].bounds.size.h;
        assert!(
            h > 0.0 && h < 100.0,
            "div height fits its text line ({h}), not the viewport"
        );
        // No atlas → no glyph sprites (real glyph rasterization is shown by the `hello` example).
        assert!(scene.glyphs.is_empty());
    }

    #[test]
    fn paint_should_position_nested_child_at_absolute_bounds() {
        // A grandchild's painted bounds must accumulate ancestor origins (taffy layout coords are
        // parent-relative). Column root: a 30px-tall spacer, then a container whose child has a bg.
        let red = Background::Solid(Color::new(1.0, 0.0, 0.0, 1.0));
        let mut root = div()
            .flex_col()
            .child(div().size(Size { w: 50.0, h: 30.0 }))
            .child(div().child(div().background(red)))
            .into_element();

        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text_system = TextSystem::new(FontDb::new());
        let mut scene = Scene::new();
        let damage = Arc::new(DamageState::default());
        let theme = Theme::default();

        render_tree(
            &mut root,
            &mut arena,
            &mut layout,
            &mut text_system,
            None,
            None, // no hit-test recording here
            None, // no overlay
            None, // no focus registry
            None, // no cursor registry
            None, // no a11y sink
            &mut scene,
            Size { w: 200.0, h: 200.0 },
            &damage,
            &theme,
        )
        .unwrap();

        // Only the red grandchild has a background. Its absolute y must be below the spacer
        // (~30), proving the parent (mid container, offset by the spacer) origin was accumulated
        // — not the bug where the grandchild paints at its parent-relative (0) y.
        assert_eq!(scene.quads.len(), 1, "only the red grandchild has a bg");
        assert!(
            scene.quads[0].bounds.origin.y >= 29.0,
            "grandchild is placed at its absolute y ({}), accumulating the spacer offset",
            scene.quads[0].bounds.origin.y
        );
    }

    #[test]
    fn text_leaf_should_size_to_shaped_content() {
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text_system = TextSystem::new(FontDb::new());
        let damage: Arc<dyn DamageSink> = Arc::new(NoopDamage);
        let theme = Theme::default();

        let mut leaf = text("Hi");
        let id = {
            let mut cx = LayoutCx {
                arena: &mut arena,
                layout: &mut layout,
                text: &mut text_system,
                damage,
                theme: &theme,
                focus: None,
                cursor: None,
            };
            leaf.request_layout(&mut cx)
        };
        layout
            .compute(
                id,
                Size {
                    w: 1000.0,
                    h: 1000.0,
                },
            )
            .unwrap();

        let bounds = layout.layout(id);
        assert!(
            bounds.size.w > 0.0 && bounds.size.h > 0.0,
            "the text leaf's MeasureFn reports its shaped intrinsic size"
        );
    }
}
