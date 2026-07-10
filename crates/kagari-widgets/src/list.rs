//! The [`VirtualizedList`] widget (#86): a fixed-height virtualized list with a clean public API and
//! built-in wheel scrolling.
//!
//! It wraps the core [`virtualized_list`](kagari_core::virtualized_list) element (#61 / T-M6-08), which does
//! the windowing — realizing only the rows intersecting the viewport (± a buffer), reusing in-range rows by
//! index, positioning each at `index * item_height - offset`, and clipping to the viewport. This widget adds
//! an **internal** [`ScrollHandle`] and a **wheel** handler so `virtualized_list(count, item_height, item_fn)`
//! scrolls out of the box.
//!
//! MVP scope: the viewport is set with [`.size(..)`](VirtualizedList::size) (the core needs a fixed viewport);
//! scrolling is **instant** (the `ScrollHandle` spring is only advanced by an external tick, which a
//! standalone widget has none of). Smooth spring scroll + an overlay scrollbar compose with `ScrollView`
//! (#83); interactive rows (the core element does not forward events to realized rows) and variable item
//! height are follow-ups.

use std::sync::Arc;

use kagari_base::{Point, Size};
use kagari_core::{
    AnyElement, IntoElement, MouseKind, ScrollHandle, div,
    virtualized_list as core_virtualized_list,
};

/// A fixed-height virtualized list (#86). Build with [`virtualized_list`]; set the viewport with
/// [`.size(..)`](Self::size). Only the rows in view (± a buffer) are ever built, and a wheel scroll recycles
/// them — the realized element count stays O(visible), not O(`count`). Returns `impl IntoElement`; `Styled`
/// is **not** on its surface.
pub struct VirtualizedList {
    count: usize,
    item_height: f32,
    viewport: Size,
    item_fn: Arc<dyn Fn(usize) -> AnyElement>,
}

/// Creates a virtualized list of `count` rows, each `item_height` tall, building row `index` with
/// `item_fn(index)`. Set the scroll viewport with [`.size(..)`](VirtualizedList::size) — it defaults to a
/// small placeholder, so a real caller sizes it to its slot.
pub fn virtualized_list(
    count: usize,
    item_height: f32,
    item_fn: impl Fn(usize) -> AnyElement + 'static,
) -> VirtualizedList {
    VirtualizedList {
        count,
        item_height,
        viewport: Size { w: 240.0, h: 320.0 },
        item_fn: Arc::new(item_fn),
    }
}

impl VirtualizedList {
    /// Sets the scroll viewport (the clipped window the list scrolls within). Mirrors
    /// [`Scroll::size`](kagari_core::element::Scroll); the core windowing needs a concrete viewport.
    pub fn size(mut self, size: Size) -> Self {
        self.viewport = size;
        self
    }
}

impl IntoElement for VirtualizedList {
    fn into_element(self) -> AnyElement {
        let VirtualizedList {
            count,
            item_height,
            viewport,
            item_fn,
        } = self;

        // An internal scroll offset shared with the core windowing element. The furthest the list can scroll
        // is the content height beyond the viewport (floored at 0 — a list shorter than its viewport).
        let handle = ScrollHandle::new();
        let max_y = (count as f32 * item_height - viewport.h).max(0.0);
        let core =
            core_virtualized_list(item_height, count, viewport, &handle, move |i| item_fn(i));

        // The wrapping div carries the wheel handler (the core element has no `handle_event`) and its own
        // hit region over the whole viewport, so a wheel anywhere in the list scrolls it. Instant `jump_to`
        // (not the spring) because nothing ticks a standalone handle's animation.
        div()
            .size(viewport)
            .on_wheel(move |ev, _cx| {
                if let MouseKind::Wheel { dy, .. } = ev.kind {
                    let next = (handle.offset().y - dy * item_height).clamp(0.0, max_y);
                    handle.jump_to(Point::new(0.0, next));
                }
            })
            .child(core)
            .into_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc as StdArc;

    use kagari_base::{NodeId, Point, Size as BSize};
    use kagari_core::arena::Arena;
    use kagari_core::damage::DamageState;
    use kagari_core::paint::render_tree;
    use kagari_core::reactive::Owner;
    use kagari_core::{
        DispatchState, HitTest, Modifiers, MouseEvent, MouseKind, dispatch_mouse, div,
    };
    use kagari_layout::LayoutTree;
    use kagari_render::Scene;
    use kagari_style::Theme;
    use kagari_text::{FontDb, TextSystem};

    const VIEWPORT: BSize = BSize { w: 100.0, h: 100.0 };

    struct Harness {
        root: AnyElement,
        arena: Arena,
        layout: LayoutTree,
        text: TextSystem,
        hit: HitTest,
        damage: StdArc<DamageState>,
        theme: Theme,
        root_id: NodeId,
    }

    fn harness(root: AnyElement) -> Harness {
        let mut h = Harness {
            root,
            arena: Arena::new(),
            layout: LayoutTree::new(),
            text: TextSystem::new(FontDb::new()),
            hit: HitTest::new(),
            damage: StdArc::new(DamageState::default()),
            theme: Theme::light(),
            root_id: NodeId::from_raw(0),
        };
        h.root_id = frame(&mut h);
        h
    }

    /// Renders one frame (runs reconcile so an offset change realizes the new window), returns the root id.
    fn frame(h: &mut Harness) -> NodeId {
        let mut scene = Scene::new();
        render_tree(
            &mut h.root,
            &mut h.arena,
            &mut h.layout,
            &mut h.text,
            None,
            Some(&mut h.hit),
            None,
            None,
            None,
            None,
            &mut scene,
            VIEWPORT,
            &h.damage,
            &h.theme,
        )
        .unwrap()
    }

    /// The realized row-slot node ids (widget root div → child[0] core node → its children = the row slots).
    fn realized_rows(h: &Harness) -> Vec<NodeId> {
        let core = h.arena.get(h.root_id).unwrap().children[0];
        h.arena.get(core).unwrap().children.clone()
    }

    /// A simple fixed-size row element for `index`.
    fn row(_index: usize) -> AnyElement {
        div()
            .size(BSize { w: 100.0, h: 20.0 })
            .background(kagari_render::Background::Solid(kagari_base::Color::new(
                0.0, 1.0, 0.0, 1.0,
            )))
            .into_element()
    }

    /// Scrolls the list by dispatching a wheel event at the viewport centre.
    fn wheel(h: &mut Harness, dy: f32) {
        let ev = MouseEvent::new(
            MouseKind::Wheel { dx: 0.0, dy },
            Point::new(50.0, 50.0),
            Modifiers::default(),
        );
        let mut dispatch = DispatchState::default();
        dispatch_mouse(&mut h.root, &h.arena, &h.hit, &ev, &mut dispatch);
    }

    #[test]
    fn virtualized_list_should_render_visible_window_only() {
        let owner = Owner::new();
        owner.set();
        // 1000 rows of 20px in a 100px viewport → visible 0..5 + ±2 buffer = 7 realized, never 1000.
        let h = harness(
            virtualized_list(1000, 20.0, row)
                .size(BSize { w: 100.0, h: 100.0 })
                .into_element(),
        );
        let rows = realized_rows(&h);
        assert_eq!(rows.len(), 7, "only the visible window (± buffer) is built");
        assert!(rows.len() < 1000, "the huge list is not fully realized");
        drop(owner);
    }

    #[test]
    fn virtualized_list_should_recycle_on_scroll() {
        let owner = Owner::new();
        owner.set();
        let mut h = harness(
            virtualized_list(1000, 20.0, row)
                .size(BSize { w: 100.0, h: 100.0 })
                .into_element(),
        );
        let rows0 = realized_rows(&h);
        assert_eq!(rows0.len(), 7);

        // Wheel down 3 rows (dy = -3 → +60px offset): the window shifts 0..7 → 3..10.
        wheel(&mut h, -3.0);
        frame(&mut h); // reconcile realizes the shifted window
        let rows1 = realized_rows(&h);

        assert!(
            rows1.len() < 1000,
            "the realized count stays O(visible), not O(count)"
        );
        assert!(
            !rows1.contains(&rows0[0]),
            "the top row scrolled out of the window (recycled, node released)"
        );
        assert!(
            !h.arena.contains(rows0[0]),
            "the released row's node is freed"
        );
        assert_ne!(rows1, rows0, "the visible window shifted after scrolling");
        drop(owner);
    }

    #[test]
    fn virtualized_list_empty_should_render_nothing() {
        let owner = Owner::new();
        owner.set();
        let mut h = harness(
            virtualized_list(0, 20.0, row)
                .size(BSize { w: 100.0, h: 100.0 })
                .into_element(),
        );
        assert_eq!(
            realized_rows(&h).len(),
            0,
            "a zero-count list realizes no rows"
        );
        // A second frame must not panic on the empty range.
        frame(&mut h);
        drop(owner);
    }

    #[test]
    fn virtualized_list_should_clamp_scroll() {
        let owner = Owner::new();
        owner.set();
        let mut h = harness(
            virtualized_list(1000, 20.0, row)
                .size(BSize { w: 100.0, h: 100.0 })
                .into_element(),
        );
        let top = realized_rows(&h);

        // Wheel up at the top: `next` would go negative but clamps to 0, so the window stays pinned at row 0.
        wheel(&mut h, 5.0);
        frame(&mut h);
        assert_eq!(
            realized_rows(&h),
            top,
            "wheel-up at the top clamps the offset to 0 (window unchanged)"
        );

        // Wheel far past the end: `next` clamps to max_y, realizing a bounded tail window — not empty / OOB.
        wheel(&mut h, -100_000.0);
        frame(&mut h);
        let tail = realized_rows(&h);
        assert!(
            !tail.is_empty() && tail.len() < 1000,
            "over-scroll clamps to a bounded tail window"
        );
        assert!(
            !tail.contains(&top[0]),
            "over-scroll clamps to the tail, not the head (row 0 released)"
        );
        drop(owner);
    }
}
