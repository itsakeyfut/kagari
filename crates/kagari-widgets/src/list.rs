//! The [`VirtualizedList`] widget (#86): a fixed-height virtualized list with a clean public API and
//! built-in wheel scrolling.
//!
//! It wraps the core [`virtualized_list`](kagari_core::virtualized_list) element (#61 / T-M6-08), which does
//! the windowing — realizing only the rows intersecting the viewport (± a buffer), reusing in-range rows by
//! index, positioning each at `index * item_height - offset`, and clipping to the viewport. This widget adds
//! an **internal** [`ScrollHandle`] and a **wheel** handler so `virtualized_list(count, item_height, item_fn)`
//! scrolls out of the box.
//!
//! The viewport is set with [`.size(..)`](VirtualizedList::size) (the core needs a fixed viewport). Scrolling
//! is **smooth** (#321): a wheel notch springs to a clamped target via [`ScrollHandle::scroll_to`], and the
//! per-window frame clock drives the spring (the handle captures the app's `Ticker`/`ActiveSources` from
//! context when built under the window owner, #253). The core [`virtualized_list`](kagari_core::virtualized_list)
//! element paints a **draggable** overlay scrollbar (right edge) reflecting the content/viewport ratio.
//! Follow-ups: variable item height; a fling (a wheel has no release velocity, so it is not wheel-wired).

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
    handle: Option<ScrollHandle>,
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
        handle: None,
    }
}

impl VirtualizedList {
    /// Sets the scroll viewport (the clipped window the list scrolls within). Mirrors
    /// [`Scroll::size`](kagari_core::element::Scroll); the core windowing needs a concrete viewport.
    pub fn size(mut self, size: Size) -> Self {
        self.viewport = size;
        self
    }

    /// Binds an external [`ScrollHandle`] for programmatic scrolling (scroll-restore, scroll-to) and to
    /// observe the offset. Mirrors [`ScrollView::handle`](crate::ScrollView::handle); when unset, the list
    /// owns an internal handle.
    ///
    /// Construct the handle under the window's reactive owner (e.g. in your component body) so it captures
    /// the per-window frame clock — a handle built before the window owner is established will not drive the
    /// smooth-scroll spring (RK-033). Keep programmatic offsets within `[0, count·item_height − viewport]`:
    /// the list positions rows from the raw offset (unlike [`Scroll`](kagari_core::element::Scroll), it does
    /// not defensively clamp at paint), so a far over-scroll would push every row out of the viewport. The
    /// built-in wheel and thumb-drag paths already clamp.
    pub fn handle(mut self, handle: ScrollHandle) -> Self {
        self.handle = Some(handle);
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
            handle,
        } = self;

        // The scroll offset shared with the core windowing element (external handle, or an internal one).
        // The furthest the list can scroll is the content height beyond the viewport (floored at 0 — a
        // list shorter than its viewport).
        let handle = handle.unwrap_or_else(ScrollHandle::new);
        let max_y = (count as f32 * item_height - viewport.h).max(0.0);
        let core =
            core_virtualized_list(item_height, count, viewport, &handle, move |i| item_fn(i));

        // The wrapping div carries the wheel handler (the core element does not handle wheel — it handles
        // row + thumb events) and its own hit region over the whole viewport, so a wheel anywhere in the
        // list scrolls it. Smooth spring scroll: accumulate the notch on the animation *target* (not the
        // momentary offset, so quick successive notches don't drop deltas mid-flight) and clamp it — the
        // core VL element does not clamp its paint offset, so the clamp here keeps the spring from settling
        // over-scrolled (blank). The app frame clock ticks the handle's spring (#253).
        div()
            .size(viewport)
            .on_wheel(move |ev, _cx| {
                if let MouseKind::Wheel { dy, .. } = ev.kind {
                    // `dy` is already a logical-pixel scroll distance (the app scales a notch by `WHEEL_LINE_PX`),
                    // so apply it directly — multiplying by `item_height` again over-scrolls by a row per pixel.
                    let next = (handle.target().y - dy).clamp(0.0, max_y);
                    handle.scroll_to(Point::new(0.0, next));
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
    use std::time::Duration;

    use kagari_base::{Color, NodeId, Point, Size as BSize};
    use kagari_core::arena::Arena;
    use kagari_core::damage::DamageState;
    use kagari_core::paint::render_tree;
    use kagari_core::reactive::{Owner, provide_context};
    use kagari_core::{
        ActiveSources, DispatchState, HitTest, Modifiers, MouseEvent, MouseKind, Ticker,
        dispatch_mouse, div,
    };
    use kagari_layout::LayoutTree;
    use kagari_render::{Background, Scene};
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

    /// Renders one frame and returns the painted scene (for inspecting the overlay-scrollbar thumb).
    fn frame_scene(h: &mut Harness) -> Scene {
        let mut scene = Scene::new();
        h.root_id = render_tree(
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
        .unwrap();
        scene
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

    /// Builds a `count`-row harness whose scroll offset is an external [`ScrollHandle`] (so a test can
    /// observe/settle the spring), with BOTH a [`Ticker`] and an [`ActiveSources`] provided in context —
    /// the app wiring (#253/RK-033): the widget's handle captures them, so the smooth wheel registers an
    /// active source and the ticker drives the spring. Requires an owner already set. Returns the harness,
    /// the handle, the ticker, and the active-source counter (which reaches 0 once the spring settles).
    fn scrolling_harness(count: usize) -> (Harness, ScrollHandle, Ticker, ActiveSources) {
        let ticker = Ticker::new();
        let sources = ActiveSources::new();
        provide_context(ticker.clone());
        provide_context(sources.clone());
        let handle = ScrollHandle::new();
        let root = virtualized_list(count, 20.0, row)
            .size(VIEWPORT)
            .handle(handle.clone())
            .into_element();
        (harness(root), handle, ticker, sources)
    }

    /// Drives the ticker until the spring converges — detected by the active-source count returning to 0
    /// (the real app-idle signal, RK-033). On convergence the [`Animated`] snaps to the target *exactly*,
    /// so the caller can rely on the settled offset landing on an exact row boundary.
    fn settle(ticker: &Ticker, sources: &ActiveSources) {
        let mut ticks = 0;
        while sources.count() > 0 {
            ticker.tick_all(Duration::from_secs_f32(1.0 / 60.0));
            ticks += 1;
            assert!(ticks < 10_000, "the spring settles in bounded time");
        }
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
        let (mut h, _handle, ticker, sources) = scrolling_harness(1000);
        let rows0 = realized_rows(&h);
        assert_eq!(rows0.len(), 7);

        // Wheel down 3 rows (dy = -60 px → +60px target): the spring settles (snaps) at 60, then the frame
        // realizes the shifted window (0..7 → 1..10, so index 0 is released).
        wheel(&mut h, -60.0);
        settle(&ticker, &sources);
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
        // A 1000-row list: the max offset is 1000·20 − 100 = 19900, a genuinely large px extent — this also
        // exercises the scale-relative spring rest thresholds (anim.rs), i.e. that a spring targeting ~19900
        // actually converges and releases its active source (a fixed absolute epsilon never rested at this
        // magnitude — see `animated_should_converge_at_large_magnitude`).
        let (mut h, _handle, ticker, sources) = scrolling_harness(1000);
        let top = realized_rows(&h);

        // Wheel up at the top: `next` would go negative but clamps to 0, so the window stays pinned at row 0.
        wheel(&mut h, 5.0);
        settle(&ticker, &sources);
        frame(&mut h);
        assert_eq!(
            realized_rows(&h),
            top,
            "wheel-up at the top clamps the offset to 0 (window unchanged)"
        );

        // Wheel far past the end: `next` clamps to max_y (19900), so the spring settles at the clamped max
        // and realizes a bounded tail window — never blank / OOB (the arch-review's over-scroll-blank hazard
        // is prevented by clamping the target here; the spring settles at scale thanks to the anim.rs fix).
        wheel(&mut h, -100_000.0);
        settle(&ticker, &sources);
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

    #[test]
    fn virtualized_list_smooth_wheel_should_settle_at_target() {
        let owner = Owner::new();
        owner.set();
        let (mut h, handle, ticker, sources) = scrolling_harness(1000);

        // A wheel notch springs (does not jump): dy -60 px → clamped target 60, but the offset does not reach
        // it until the ticker (the app frame clock) drives the spring.
        wheel(&mut h, -60.0);
        assert!(
            handle.offset().y < 60.0,
            "smooth scroll animates rather than jumping (got {})",
            handle.offset().y
        );

        settle(&ticker, &sources);
        assert!(
            (handle.offset().y - 60.0).abs() <= 0.1,
            "the spring settles at the clamped wheel target (got {})",
            handle.offset().y
        );
        drop(owner);
    }

    #[test]
    fn virtualized_list_quick_notches_should_accumulate_on_target() {
        let owner = Owner::new();
        owner.set();
        let (mut h, handle, ticker, sources) = scrolling_harness(1000);

        // Two wheel notches while the spring is still in flight must accumulate on the *target*, not the
        // momentary offset — otherwise a mid-flight notch drops the earlier delta. dy -60 px (→ target 60),
        // advance one frame (offset ≪ 60), then dy -60 again → target 120 (2·60), not ~66 (60 + a bit).
        wheel(&mut h, -60.0);
        ticker.tick_all(Duration::from_secs_f32(1.0 / 60.0));
        assert!(
            handle.offset().y < 60.0,
            "still mid-flight after one tick (got {})",
            handle.offset().y
        );
        wheel(&mut h, -60.0);
        settle(&ticker, &sources);
        assert!(
            (handle.offset().y - 120.0).abs() <= 0.1,
            "two quick notches accumulate on the target to 120, not a dropped-delta ~66 (got {})",
            handle.offset().y
        );
        drop(owner);
    }

    #[test]
    fn virtualized_list_should_paint_scrollbar_thumb() {
        let owner = Owner::new();
        owner.set();
        // 1000 rows of 20px overflow the 100px viewport → the core element paints an overlay thumb.
        let mut h = harness(
            virtualized_list(1000, 20.0, row)
                .size(VIEWPORT)
                .into_element(),
        );
        let scene = frame_scene(&mut h);
        let thumb = Background::Solid(Color::from_srgb([0.5, 0.5, 0.5, 0.85]));
        assert!(
            scene.quads.iter().any(|q| q.bg == thumb),
            "the virtualized list paints an overlay scrollbar thumb when content overflows"
        );
        drop(owner);
    }
}
