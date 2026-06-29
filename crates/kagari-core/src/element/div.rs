//! The [`Div`] element: a flex container with children and raw style props (#31).

use std::sync::{Arc, Mutex};

use kagari_base::{Color, Corners, Edges, NodeId, Px, Rect, Size};
use kagari_layout::{FlexDirection, LayoutStyle};
use kagari_render::{Background, Border, Quad, RoundedRect};

use super::{AnyElement, DamageSink, Element, Event, EventCx, IntoElement, LayoutCx, PaintCx};
use crate::arena::Node;
use crate::reactive::{Prop, create_effect};

/// A flex container element: children plus raw layout/paint props.
///
/// Style props are raw `kagari-render`/`kagari-base` values via [`Prop`] (the Tailwind token
/// API is deferred to the styling milestone, /spec Q4). A static prop is resolved once; a
/// reactive prop is bound at build to a synchronous effect that re-resolves it and flags damage.
pub struct Div {
    children: Vec<AnyElement>,
    layout: LayoutStyle,
    bg: Option<Prop<Background>>,
    /// Assigned on the first `request_layout`; the build-once rebuild lifecycle is #32.
    id: Option<NodeId>,
    /// Resolved background: written by the bound reactive effect (or once, for a static prop),
    /// read by `paint`. Shared + `'static` so the effect can own a handle to it.
    resolved_bg: Arc<Mutex<Option<Background>>>,
}

/// Creates an empty [`Div`].
pub fn div() -> Div {
    Div {
        children: Vec::new(),
        layout: LayoutStyle::default(),
        bg: None,
        id: None,
        resolved_bg: Arc::new(Mutex::new(None)),
    }
}

impl Div {
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

    /// Sets the background (static value or reactive closure via [`Prop`]).
    pub fn bg(mut self, bg: impl Into<Prop<Background>>) -> Self {
        self.bg = Some(bg.into());
        self
    }

    /// Lays children out in a row (the flex default).
    pub fn flex(mut self) -> Self {
        self.layout.flex_direction = FlexDirection::Row;
        self
    }

    /// Lays children out in a column.
    pub fn flex_col(mut self) -> Self {
        self.layout.flex_direction = FlexDirection::Column;
        self
    }

    /// Sets the gap between children.
    pub fn gap(mut self, gap: Px) -> Self {
        self.layout.gap = gap;
        self
    }

    /// Sets a fixed size.
    pub fn size(mut self, size: Size) -> Self {
        self.layout.size = Some(size);
        self
    }

    /// Resolves the background prop into `resolved_bg`. A static prop writes its value once; a
    /// reactive prop registers a synchronous effect (ADR 0001) that re-resolves the closure,
    /// writes the cell, and flags paint-damage for `id` — the #35 hook.
    ///
    /// The effect is registered **once** at build (the prop closure is moved into it) and stays
    /// alive via its signal subscription; it is owner-scoped to the *current* ambient owner (the
    /// App root in #36). Per-node owners — so a removed node disposes its effect — and the real
    /// damage tracking arrive with #32/#35; see RK-005.
    fn bind_background(&mut self, damage: &Arc<dyn DamageSink>, id: NodeId) {
        match self.bg.take() {
            Some(Prop::Static(bg)) => self.store_bg(bg),
            Some(Prop::Reactive(read)) => {
                let cell = Arc::clone(&self.resolved_bg);
                let damage = Arc::clone(damage);
                create_effect(move || {
                    let bg = read();
                    if let Ok(mut slot) = cell.lock() {
                        // Only flag damage when the resolved value actually changed: a plain
                        // signal re-runs subscribers even on an equal-value write, and idle
                        // re-paints violate the damage discipline (design.md §9).
                        if *slot != Some(bg) {
                            *slot = Some(bg);
                            damage.mark_paint_dirty(id);
                        }
                    }
                });
            }
            None => {}
        }
    }

    fn store_bg(&self, bg: Background) {
        if let Ok(mut slot) = self.resolved_bg.lock() {
            *slot = Some(bg);
        }
    }

    fn current_bg(&self) -> Option<Background> {
        self.resolved_bg.lock().ok().and_then(|slot| *slot)
    }
}

impl Element for Div {
    fn request_layout(&mut self, cx: &mut LayoutCx) -> NodeId {
        // Build-once: allocate this element's node on the first build (the rebuild lifecycle is
        // #32). Subsequent calls reuse the id, so the bound effect is registered only once.
        let id = match self.id {
            Some(id) => id,
            None => {
                let id = cx.arena.insert(Node::default());
                self.id = Some(id);
                self.bind_background(&cx.damage, id);
                id
            }
        };

        // Recurse children and link parent/children in the arena.
        let mut child_ids = Vec::with_capacity(self.children.len());
        for child in &mut self.children {
            let child_id = child.request_layout(cx);
            if let Some(node) = cx.arena.get_mut(child_id) {
                node.parent = Some(id);
            }
            child_ids.push(child_id);
        }
        if let Some(node) = cx.arena.get_mut(id) {
            node.children = child_ids;
        }
        id
    }

    fn paint(&mut self, bounds: Rect, cx: &mut PaintCx) {
        let Some(bg) = self.current_bg() else {
            return;
        };
        cx.scene.quads.push(Quad {
            bounds,
            corner_radii: Corners::default(),
            bg,
            border: Border {
                widths: Edges::default(),
                color: Color::TRANSPARENT,
            },
            content_mask: RoundedRect {
                rect: bounds,
                radii: Corners::default(),
            },
            // Painter's-order assignment is the paint pass's job (#34); single layer for now.
            order: 0,
        });
    }

    fn handle_event(&mut self, _ev: &Event, _cx: &mut EventCx) {}
}

impl IntoElement for Div {
    fn into_element(self) -> AnyElement {
        Box::new(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arena::Arena;
    use crate::reactive::rx;
    use kagari_render::Scene;
    use reactive_graph::owner::Owner;
    use reactive_graph::prelude::*;
    use reactive_graph::signal::signal;

    struct NoopDamage;
    impl DamageSink for NoopDamage {
        fn mark_paint_dirty(&self, _id: NodeId) {}
    }

    #[derive(Default)]
    struct RecordingDamage {
        dirtied: Mutex<Vec<NodeId>>,
    }
    impl DamageSink for RecordingDamage {
        fn mark_paint_dirty(&self, id: NodeId) {
            if let Ok(mut v) = self.dirtied.lock() {
                v.push(id);
            }
        }
    }

    #[test]
    fn div_child_should_build_tree() {
        let mut arena = Arena::new();
        let damage: Arc<dyn DamageSink> = Arc::new(NoopDamage);
        let mut cx = LayoutCx {
            arena: &mut arena,
            damage,
        };

        let mut root = div().child(div()).child(div());
        let root_id = root.request_layout(&mut cx);

        let root_node = cx.arena.get(root_id).expect("root node present");
        assert_eq!(root_node.children.len(), 2);
        for child_id in root_node.children.clone() {
            assert_eq!(
                cx.arena.get(child_id).and_then(|n| n.parent),
                Some(root_id),
                "each child's parent points back at the root"
            );
        }
    }

    #[test]
    fn into_element_should_round_trip() {
        let any: AnyElement = div().into_element();
        // Re-wrapping an AnyElement is the identity (no double box).
        let _again: AnyElement = any.into_element();
    }

    #[test]
    fn div_static_bg_should_paint_quad() {
        let mut arena = Arena::new();
        let damage: Arc<dyn DamageSink> = Arc::new(NoopDamage);
        let mut cx = LayoutCx {
            arena: &mut arena,
            damage,
        };

        let green = Background::Solid(Color::new(0.0, 1.0, 0.0, 1.0));
        let mut d = div().bg(green);
        d.request_layout(&mut cx);

        let mut scene = Scene::new();
        let mut pcx = PaintCx { scene: &mut scene };
        d.paint(Rect::default(), &mut pcx);

        assert_eq!(scene.quads.len(), 1);
        assert_eq!(scene.quads[0].bg, green);
    }

    #[test]
    fn div_reactive_bg_should_repaint_and_flag_damage() {
        // create_effect needs a current owner; keep `owner` alive for the whole test (RK-003).
        // The effect is synchronous (ImmediateEffect), so this is hang-free (RK-005).
        let owner = Owner::new();
        owner.set();

        let red = Background::Solid(Color::new(1.0, 0.0, 0.0, 1.0));
        let blue = Background::Solid(Color::new(0.0, 0.0, 1.0, 1.0));
        let (read, write) = signal(red);

        let mut arena = Arena::new();
        let damage = Arc::new(RecordingDamage::default());
        let mut cx = LayoutCx {
            arena: &mut arena,
            damage: damage.clone(),
        };

        let mut d = div().bg(rx(move || read.get()));
        let id = d.request_layout(&mut cx);

        // The effect runs once on creation: resolved bg = red, damage flagged for `id`.
        let mut scene = Scene::new();
        d.paint(Rect::default(), &mut PaintCx { scene: &mut scene });
        assert_eq!(scene.quads[0].bg, red);
        assert!(damage.dirtied.lock().unwrap().contains(&id));
        let flags_after_build = damage.dirtied.lock().unwrap().len();

        // A signal write re-runs the effect synchronously: resolved bg = blue, re-flagged.
        write.set(blue);
        let mut scene = Scene::new();
        d.paint(Rect::default(), &mut PaintCx { scene: &mut scene });
        assert_eq!(scene.quads[0].bg, blue);
        let dirtied = damage.dirtied.lock().unwrap();
        assert!(
            dirtied.len() > flags_after_build,
            "the write re-flags damage"
        );
        assert!(
            dirtied[flags_after_build..].contains(&id),
            "re-flag targets this node's id"
        );
        drop(dirtied);

        drop(owner);
    }
}
