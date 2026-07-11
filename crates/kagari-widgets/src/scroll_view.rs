//! The [`ScrollView`] widget (#83): a scroll region with **draggable** overlay scrollbars — the reusable
//! scroll surface for content that overflows a fixed viewport.
//!
//! It wraps the core [`scroll`] element (T-M6-07), which clips its children to the
//! viewport and translates them by a reactive offset (paint-only), paints position-reflecting overlay thumbs,
//! and — when bound with [`offset_handle`](kagari_core::element::Scroll::offset_handle) — makes those thumbs
//! **draggable** (a mouse-down on a thumb captures the pointer and a drag scrolls, the grab point tracking the
//! cursor). This widget owns/accepts the [`ScrollHandle`], adds **wheel** scrolling (instant, family-consistent),
//! and toggles the axes.
//!
//! MVP scope: vertical + horizontal scroll of overflowing content, position-reflecting draggable scrollbars,
//! per-axis enable, optional external handle. **Follow-ups**: smooth (spring) wheel, page-on-track-click, a
//! scrollbar-visibility policy (auto/always/never), robust capture-release on focus-loss.

use kagari_base::{Point, Size};
use kagari_core::{AnyElement, IntoElement, MouseKind, ScrollHandle, div, scroll};

/// Logical px scrolled per wheel notch (the core clamps the visible offset; a fixed step is fine here since
/// the content extent is arbitrary, unlike the virtualized widgets' row-height step).
const WHEEL_STEP: f32 = 40.0;

/// A scroll region with draggable overlay scrollbars (#83). Build with [`scroll_view`]; set the viewport with
/// [`.size(..)`](Self::size), toggle axes with [`.horizontal`](Self::horizontal)/[`.vertical`](Self::vertical),
/// and optionally drive it programmatically with [`.handle(..)`](Self::handle). Returns `impl IntoElement`;
/// `Styled` is **not** on its surface.
pub struct ScrollView {
    content: AnyElement,
    viewport: Size,
    horizontal: bool,
    vertical: bool,
    handle: Option<ScrollHandle>,
}

/// Creates a scroll view over `content`. Set the viewport with [`.size(..)`](ScrollView::size) — it defaults
/// to a small placeholder, so a real caller sizes it to its slot. Content taller/wider than the viewport
/// scrolls (wheel + draggable scrollbar).
pub fn scroll_view(content: impl IntoElement) -> ScrollView {
    ScrollView {
        content: content.into_element(),
        viewport: Size { w: 240.0, h: 320.0 },
        horizontal: true,
        vertical: true,
        handle: None,
    }
}

impl ScrollView {
    /// Sets the scroll viewport (the clipped window). Mirrors [`VirtualizedList::size`](crate::VirtualizedList::size).
    pub fn size(mut self, viewport: Size) -> Self {
        self.viewport = viewport;
        self
    }

    /// Enables/disables horizontal scrolling (default enabled). A disabled axis pins its offset to 0 and
    /// paints no thumb.
    pub fn horizontal(mut self, on: bool) -> Self {
        self.horizontal = on;
        self
    }

    /// Enables/disables vertical scrolling (default enabled).
    pub fn vertical(mut self, on: bool) -> Self {
        self.vertical = on;
        self
    }

    /// Binds an external [`ScrollHandle`] for programmatic scrolling (scroll-restore, scroll-to). When unset,
    /// the view owns an internal handle.
    pub fn handle(mut self, handle: ScrollHandle) -> Self {
        self.handle = Some(handle);
        self
    }
}

impl IntoElement for ScrollView {
    fn into_element(self) -> AnyElement {
        let ScrollView {
            content,
            viewport,
            horizontal,
            vertical,
            handle,
        } = self;
        let handle = handle.unwrap_or_else(ScrollHandle::new);

        // The core scroll: clip + reactive offset + draggable thumbs (via the bound handle) + per-axis lock.
        let inner = scroll()
            .size(viewport)
            .axes(horizontal, vertical)
            .offset_handle(&handle)
            .child(content);

        // The wrapping div carries a viewport-covering wheel handler (the core scroll's own hit regions are
        // only the thumbs). Instant `jump_to`, clamped to the core's last-measured content extent — matching
        // the virtualized-widget family (a standalone handle's spring is not self-ticked).
        let wheel = handle.clone();
        div()
            .size(viewport)
            .on_wheel(move |ev, _| {
                if let MouseKind::Wheel { dx, dy } = ev.kind {
                    let content = wheel.content();
                    // Untracked: an event-time snapshot must not create a reactive subscription (matches
                    // the core drag handler's `offset_untracked`).
                    let off = wheel.offset_untracked();
                    let max_x = (content.w - viewport.w).max(0.0);
                    let max_y = (content.h - viewport.h).max(0.0);
                    let x = if horizontal {
                        (off.x - dx * WHEEL_STEP).clamp(0.0, max_x)
                    } else {
                        0.0
                    };
                    let y = if vertical {
                        (off.y - dy * WHEEL_STEP).clamp(0.0, max_y)
                    } else {
                        0.0
                    };
                    wheel.jump_to(Point::new(x, y));
                }
            })
            .child(inner)
            .into_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc as StdArc;

    use kagari_base::{Color, NodeId, Size as BSize};
    use kagari_core::arena::Arena;
    use kagari_core::damage::DamageState;
    use kagari_core::paint::render_tree;
    use kagari_core::reactive::Owner;
    use kagari_core::{
        DispatchState, HitTest, Modifiers, MouseButton, MouseEvent, MouseKind, dispatch_mouse, div,
    };
    use kagari_layout::LayoutTree;
    use kagari_render::{Background, Scene};
    use kagari_style::Theme;
    use kagari_text::{FontDb, TextSystem};

    const VIEWPORT: BSize = BSize { w: 200.0, h: 200.0 };
    const VP: BSize = BSize { w: 100.0, h: 100.0 }; // the scroll_view's own viewport

    fn green() -> Background {
        Background::Solid(Color::new(0.0, 1.0, 0.0, 1.0))
    }

    /// A scroll view of a 100×300 green child (overflows the 100-tall viewport), optionally external-handled.
    fn tall_view(handle: Option<ScrollHandle>) -> AnyElement {
        let content = div().size(BSize { w: 100.0, h: 300.0 }).background(green());
        let mut sv = scroll_view(content).size(VP);
        if let Some(h) = handle {
            sv = sv.handle(h);
        }
        sv.into_element()
    }

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
        h.root_id = frame(&mut h).0;
        h
    }

    fn frame(h: &mut Harness) -> (NodeId, Scene) {
        let mut scene = Scene::new();
        let id = render_tree(
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
        (id, scene)
    }

    fn send(h: &mut Harness, st: &mut DispatchState, k: MouseKind, x: f32, y: f32) {
        let ev = MouseEvent::new(k, Point::new(x, y), Modifiers::default());
        dispatch_mouse(&mut h.root, &h.arena, &h.hit, &ev, st);
    }

    #[test]
    fn scroll_view_should_scroll_content() {
        let owner = Owner::new();
        owner.set();
        let handle = ScrollHandle::new();
        let mut h = harness(tall_view(Some(handle.clone())));
        // A wheel over the content centre scrolls: dy -3 → offset.y = 0 - (-3)*40 = 120, clamped to max 200.
        let mut st = DispatchState::default();
        send(
            &mut h,
            &mut st,
            MouseKind::Wheel { dx: 0.0, dy: -3.0 },
            50.0,
            50.0,
        );
        assert_eq!(
            handle.offset().y,
            120.0,
            "a wheel notch scrolls the content by a fixed step"
        );
        // A large wheel over-scrolls past the content: it clamps to max (content 300 − viewport 100 = 200).
        send(
            &mut h,
            &mut st,
            MouseKind::Wheel { dx: 0.0, dy: -10.0 },
            50.0,
            50.0,
        );
        assert_eq!(
            handle.offset().y,
            200.0,
            "an over-scroll clamps to the content extent (content − viewport)"
        );
        drop(owner);
    }

    #[test]
    fn scrollbar_should_reflect_position() {
        let owner = Owner::new();
        owner.set();
        // Overflowing content → the core paints a position-reflecting overlay thumb (gray).
        let mut h = harness(tall_view(None));
        let (_, scene) = frame(&mut h);
        let thumb = Background::Solid(Color::from_srgb([0.5, 0.5, 0.5, 0.85]));
        assert!(
            scene.quads.iter().any(|q| q.bg == thumb),
            "overflowing content paints an overlay scrollbar thumb"
        );
        drop(owner);
    }

    #[test]
    fn scroll_view_drag_should_move_offset() {
        let owner = Owner::new();
        owner.set();
        let handle = ScrollHandle::new();
        let mut h = harness(tall_view(Some(handle.clone())));
        // Grab the vertical thumb (x∈[94,100], y∈[0,33.3]) and drag down; the grab tracks the cursor.
        let mut st = DispatchState::default();
        send(
            &mut h,
            &mut st,
            MouseKind::Down(MouseButton::Left),
            97.0,
            15.0,
        );
        send(&mut h, &mut st, MouseKind::Move, 97.0, 60.0);
        // Content 300 / viewport 100 → thumb len 33.3, free 66.67, max_off 200. Drag delta 45 → offset
        // 45 · 200/66.67 = 135 (the grab point tracks the cursor exactly).
        assert!(
            (handle.offset().y - 135.0).abs() < 0.01,
            "dragging the thumb scrolls by the exact inverse-mapped amount (got {})",
            handle.offset().y
        );
        drop(owner);
    }

    #[test]
    fn scroll_view_horizontal_false_should_not_scroll_x() {
        let owner = Owner::new();
        owner.set();
        let handle = ScrollHandle::new();
        // Content overflows both axes, but horizontal is disabled → a horizontal wheel does not move x.
        let content = div().size(BSize { w: 300.0, h: 300.0 }).background(green());
        let mut h = harness(
            scroll_view(content)
                .size(VP)
                .horizontal(false)
                .handle(handle.clone())
                .into_element(),
        );
        let mut st = DispatchState::default();
        send(
            &mut h,
            &mut st,
            MouseKind::Wheel { dx: -3.0, dy: -3.0 },
            50.0,
            50.0,
        );
        assert_eq!(
            handle.offset().x,
            0.0,
            "the disabled horizontal axis does not scroll"
        );
        assert!(
            handle.offset().y > 0.0,
            "the enabled vertical axis still scrolls"
        );
        drop(owner);
    }

    #[test]
    fn scroll_view_external_handle_should_drive() {
        let owner = Owner::new();
        owner.set();
        let handle = ScrollHandle::new();
        handle.jump_to(Point::new(0.0, 50.0));
        let mut h = harness(tall_view(Some(handle.clone())));
        // The external handle's offset drives the painted content (scrolled up by 50 → green at y = -50).
        let (_, scene) = frame(&mut h);
        assert!(
            scene
                .quads
                .iter()
                .any(|q| q.bg == green() && q.bounds.origin.y == -50.0),
            "an external handle's offset scrolls the content"
        );
        drop(owner);
    }

    #[test]
    fn scroll_view_empty_should_render_nothing() {
        let owner = Owner::new();
        owner.set();
        // A content element that fits the viewport → no thumb, no panic.
        let content = div().size(BSize { w: 40.0, h: 40.0 }).background(green());
        let mut h = harness(scroll_view(content).size(VP).into_element());
        let (_, scene) = frame(&mut h);
        let thumb = Background::Solid(Color::from_srgb([0.5, 0.5, 0.5, 0.85]));
        assert!(
            !scene.quads.iter().any(|q| q.bg == thumb),
            "content that fits paints no scrollbar thumb"
        );
        drop(owner);
    }
}
