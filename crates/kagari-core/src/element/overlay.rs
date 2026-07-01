//! The [`Overlay`] element (#62): a deferred-draw portal. Its child paints above all normal content,
//! escaping any parent clip, at an absolute window position — via the overlay order namespace + the
//! [`OverlayRegistry`](crate::OverlayRegistry). Anchored placement (below/above/auto-flip) is #175.

use kagari_base::{NodeId, Point, Rect, Size};
use kagari_layout::{Display, LayoutStyle};

use super::{AnyElement, Element, Event, EventCx, IntoElement, LayoutCx, PaintCx};
use crate::arena::Node;

/// A portal overlay: its `child` is deferred-drawn at an absolute window `position`, above all
/// normal content and clipped only to itself (escaping a parent clip / scroll). Built with
/// [`overlay`]. Its own node takes no space in normal flow.
pub struct Overlay {
    child: Option<AnyElement>,
    position: Point,
    id: Option<NodeId>,
    child_id: Option<NodeId>,
}

/// Creates an overlay portal wrapping `child`. Set the window position with [`Overlay::position`]
/// (default `(0, 0)`).
pub fn overlay(child: impl IntoElement) -> Overlay {
    Overlay {
        child: Some(child.into_element()),
        position: Point::new(0.0, 0.0),
        id: None,
        child_id: None,
    }
}

impl Overlay {
    /// Sets the absolute window position (top-left) the overlay content paints at. Anchored placement
    /// relative to another element is a follow-up (#175).
    pub fn position(mut self, position: Point) -> Self {
        self.position = position;
        self
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
                // Block + zero size: the overlay takes no space in normal flow (it does not push
                // siblings), while its child lays out at its natural/explicit size (block does not
                // flex-shrink it) and is painted at the absolute position.
                cx.layout.set_style(
                    id,
                    &LayoutStyle {
                        display: Display::Block,
                        size: Some(Size { w: 0.0, h: 0.0 }),
                        ..LayoutStyle::default()
                    },
                );
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
        // The overlay ignores its zero-size in-flow `bounds` and paints its child at the absolute
        // window position, deferred to the top painter-order and escaping any parent clip.
        let (Some(child), Some(child_id)) = (self.child.as_mut(), self.child_id) else {
            return;
        };
        let child_bounds = Rect {
            origin: self.position,
            size: cx.layout.layout(child_id).size,
        };
        // Register the overlay (records the entry + hands out its z-slot in registration order);
        // slot 0 when no registry is attached (a lone overlay still draws frontmost).
        let anchor = self.id.unwrap_or(child_id);
        let slot = cx.overlay_register(anchor, child_bounds).unwrap_or(0);
        // Escape any parent clip and draw the subtree in the overlay's high painter-order band.
        let saved_clip = cx.clip();
        cx.set_clip(None);
        cx.enter_overlay(slot);
        child.paint(child_bounds, cx);
        cx.exit_overlay();
        cx.set_clip(saved_clip);
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
}
