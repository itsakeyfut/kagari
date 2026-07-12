//! The [`Splitter`] / ResizablePane widget (#84): two panes divided by a draggable divider, their sizes
//! controlled by an `RwSignal<f32>` ratio (0..1, D2). Dragging the divider (pointer capture) or arrow-keying
//! it while focused updates `ratio`, clamped so neither pane shrinks below `.min(px)`.
//!
//! Sizing is total-independent: each pane is `flex_grow(ratio)` / `flex_grow(1 − ratio)` with
//! `flex_basis(0) + min_w(0)/min_h(0)`, so taffy divides the whole main axis by the weights in one pass (no
//! container-total needed, no relayout feedback). The pixel-drag reads the container's laid-out length from
//! a [`BoundsHandle`] the root publishes at paint.

use std::cell::Cell;
use std::rc::Rc;

use kagari_base::Axis;
use kagari_core::reactive::prelude::*;
use kagari_core::reactive::{RwSignal, rx};
use kagari_core::{
    AnyElement, BoundsHandle, CursorIcon, IntoElement, KeyCode, MouseButton, MouseKind, div,
    use_focus_handle,
};
use kagari_style::{ColorRole, Styled};

/// Divider thickness (logical px) — also its grab target.
const DIVIDER_THICKNESS: f32 = 6.0;
/// Ratio change per arrow-key press when the divider is focused.
const KEY_STEP: f32 = 0.02;

/// A caller ratio coerced to a usable `[0, 1]` (a non-finite value → `0.5`), so a bad external signal
/// value can't collapse a pane via a negative/NaN `flex_grow` (`f32::clamp` leaves NaN as NaN, so the
/// finite check is load-bearing). The drag/key handlers additionally clamp to the min-pane range.
fn sane(ratio: f32) -> f32 {
    if ratio.is_finite() {
        ratio.clamp(0.0, 1.0)
    } else {
        0.5
    }
}

/// A two-pane resizable split bound to an `RwSignal<f32>` ratio (#84). Build with [`splitter`], add the two
/// panes with [`.pane`](Self::pane) twice, set the orientation with [`.horizontal`](Self::horizontal)/
/// [`.vertical`](Self::vertical) and a minimum pane size with [`.min`](Self::min). Returns
/// `impl IntoElement`; `Styled` is not on its surface. The splitter fills the space its parent gives it.
pub struct Splitter {
    ratio: RwSignal<f32>,
    pane_a: Option<AnyElement>,
    pane_b: Option<AnyElement>,
    /// The split orientation: `Horizontal` = panes side by side (a vertical divider, the default).
    axis: Axis,
    /// Minimum pane size (logical px) — the ratio is clamped so neither pane goes below it.
    min: f32,
}

/// Creates a horizontal splitter bound to `ratio` (controlled: dragging the divider sets `ratio`). Add the
/// two panes with [`.pane`](Splitter::pane).
pub fn splitter(ratio: RwSignal<f32>) -> Splitter {
    Splitter {
        ratio,
        pane_a: None,
        pane_b: None,
        axis: Axis::Horizontal,
        min: 0.0,
    }
}

impl Splitter {
    /// Adds a pane. The first call fills the leading pane (left/top), the second the trailing pane
    /// (right/bottom); a third call replaces the trailing pane (last wins). A missing pane renders empty.
    pub fn pane(mut self, pane: impl IntoElement) -> Self {
        if self.pane_a.is_none() {
            self.pane_a = Some(pane.into_element());
        } else {
            self.pane_b = Some(pane.into_element());
        }
        self
    }

    /// Panes side by side, divided by a vertical divider dragged left/right (the default).
    pub fn horizontal(mut self) -> Self {
        self.axis = Axis::Horizontal;
        self
    }

    /// Panes stacked, divided by a horizontal divider dragged up/down.
    pub fn vertical(mut self) -> Self {
        self.axis = Axis::Vertical;
        self
    }

    /// Sets the minimum pane size (logical px): the ratio is clamped so neither pane shrinks below it.
    pub fn min(mut self, px: f32) -> Self {
        self.min = px.max(0.0);
        self
    }
}

impl IntoElement for Splitter {
    fn into_element(self) -> AnyElement {
        let Splitter {
            ratio,
            pane_a,
            pane_b,
            axis,
            min,
        } = self;
        let horizontal = matches!(axis, Axis::Horizontal);
        // The container publishes its laid-out bounds here; the divider drag reads the main-axis length.
        let bounds = BoundsHandle::new();
        let focus = use_focus_handle();

        // Each pane: proportional to its weight (flex: weight 1 0) — flex_basis 0 + min 0 so grow divides the
        // whole main axis, not just the content-free remainder. The min-PANE clamp is enforced on the ratio
        // in the handlers, not via CSS min. The wrapper is a flex column so a `flex_grow` content element
        // fills the pane (and any content stretches to the pane width) — a dock panel fills its region.
        let pane_wrapper = || div().flex_col().flex_basis(0.0).min_w(0.0).min_h(0.0);
        let pane_a_el = {
            let d = pane_wrapper().flex_grow(rx(move || sane(ratio.get())));
            match pane_a {
                Some(c) => d.child(c),
                None => d,
            }
        };
        let pane_b_el = {
            let d = pane_wrapper().flex_grow(rx(move || 1.0 - sane(ratio.get())));
            match pane_b {
                Some(c) => d.child(c),
                None => d,
            }
        };

        // Transient drag state (event-time only, not signals — mirrors how widgets hold handler state).
        let dragging = Rc::new(Cell::new(false));
        let start_ratio = Rc::new(Cell::new(0.0_f32));
        let start_pos = Rc::new(Cell::new(0.0_f32));

        // The usable main-axis length (container minus the divider), or None before the first paint. A
        // cloneable `Fn` so the move + key handlers each hold their own `BoundsHandle` clone.
        let usable = {
            let bounds = bounds.clone();
            move || {
                let len = if horizontal {
                    bounds.get().size.w
                } else {
                    bounds.get().size.h
                };
                let u = len - DIVIDER_THICKNESS;
                (u > 1.0).then_some(u)
            }
        };

        let divider = {
            let drag_d = dragging.clone();
            let drag_m = dragging.clone();
            let drag_u = dragging.clone();
            let sr_d = start_ratio.clone();
            let sr_m = start_ratio.clone();
            let sp_d = start_pos.clone();
            let sp_m = start_pos;
            let usable_m = usable.clone();
            let usable_k = usable;
            let ring = focus.clone();

            let mut d = div()
                .track_focus(&focus)
                .cursor(if horizontal {
                    CursorIcon::ColResize
                } else {
                    CursorIcon::RowResize
                })
                .bg(ColorRole::Border)
                .border_w_1()
                .border_color(rx(move || {
                    ring.is_focus_visible().then_some(ColorRole::FocusRing)
                }))
                .on_mouse_down(move |ev, cx| {
                    if let MouseKind::Down(MouseButton::Left) = ev.kind {
                        cx.capture_pointer();
                        drag_d.set(true);
                        sr_d.set(sane(ratio.get_untracked()));
                        sp_d.set(if horizontal { ev.pos.x } else { ev.pos.y });
                    }
                })
                .on_mouse_move(move |ev, _cx| {
                    if !drag_m.get() {
                        return;
                    }
                    let Some(usable) = usable_m() else {
                        return;
                    };
                    let pos = if horizontal { ev.pos.x } else { ev.pos.y };
                    let min_r = (min / usable).min(0.5);
                    let next = (sr_m.get() + (pos - sp_m.get()) / usable).clamp(min_r, 1.0 - min_r);
                    ratio.set(next);
                })
                .on_mouse_up(move |ev, cx| {
                    if let MouseKind::Up(MouseButton::Left) = ev.kind {
                        if drag_u.replace(false) {
                            cx.release_pointer();
                        }
                    }
                })
                .on_key_down(move |kev, _cx| {
                    if !kev.pressed {
                        return;
                    }
                    let dir = match kev.code {
                        KeyCode::ArrowLeft | KeyCode::ArrowUp => -1.0,
                        KeyCode::ArrowRight | KeyCode::ArrowDown => 1.0,
                        _ => return,
                    };
                    let min_r = usable_k().map(|u| (min / u).min(0.5)).unwrap_or(0.0);
                    let next =
                        (sane(ratio.get_untracked()) + dir * KEY_STEP).clamp(min_r, 1.0 - min_r);
                    ratio.set(next);
                });
            // Fixed main-axis thickness (min pins it; grow 0 default keeps it from expanding); the cross axis
            // stretches to fill by the flex container's default alignment.
            d = if horizontal {
                d.min_w(DIVIDER_THICKNESS)
            } else {
                d.min_h(DIVIDER_THICKNESS)
            };
            d
        };

        // The root fills the flex slot its parent gives it (grow 1 + shrinkable), so the panes have a
        // definite main axis to divide. Place a splitter in a flex container (a dock region) or a sized
        // parent; it is responsive — it follows the parent's resize (unlike a fixed size).
        let root = if horizontal {
            div().flex()
        } else {
            div().flex_col()
        };
        root.flex_grow(1.0)
            .min_w(0.0)
            .min_h(0.0)
            .track_bounds(&bounds)
            .child(pane_a_el)
            .child(divider)
            .child(pane_b_el)
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
    use kagari_core::event::FocusRegistry;
    use kagari_core::paint::render_tree;
    use kagari_core::reactive::{Owner, provide_context};
    use kagari_core::{
        DispatchState, HitTest, KeyEvent, Modifiers, MouseEvent, dispatch_key, dispatch_mouse,
    };
    use kagari_layout::LayoutTree;
    use kagari_render::Scene;
    use kagari_style::Theme;
    use kagari_text::{FontDb, TextSystem};

    // A 200×100 slot the splitter fills; a horizontal split divides the 200 width (minus the 6px divider =
    // 194), a vertical split divides the 100 height (minus 6 = 94).
    const VIEWPORT: BSize = BSize { w: 200.0, h: 100.0 };
    const USABLE_W: f32 = 200.0 - DIVIDER_THICKNESS;

    struct Harness {
        root: AnyElement,
        arena: Arena,
        layout: LayoutTree,
        text: TextSystem,
        hit: HitTest,
        focus: FocusRegistry,
        damage: StdArc<DamageState>,
        theme: Theme,
        root_id: NodeId,
    }

    fn harness(build: impl FnOnce() -> AnyElement) -> Harness {
        let focus = FocusRegistry::new();
        provide_context(focus.clone());
        // Wrap the splitter in a sized flex slot it fills (grow 1), so the panes have a definite axis.
        let root = div().flex().size(VIEWPORT).child(build());
        let mut h = Harness {
            root: root.into_element(),
            arena: Arena::new(),
            layout: LayoutTree::new(),
            text: TextSystem::new(FontDb::new()),
            hit: HitTest::new(),
            focus,
            damage: StdArc::new(DamageState::default()),
            theme: Theme::light(),
            root_id: NodeId::from_raw(0),
        };
        h.root_id = frame(&mut h);
        h
    }

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
            Some(&mut h.focus),
            None,
            None,
            &mut scene,
            VIEWPORT,
            &h.damage,
            &h.theme,
        )
        .unwrap()
    }

    /// The splitter's `[pane_a, divider, pane_b]` node ids (wrapper root → splitter root → its children).
    fn parts(h: &Harness) -> (NodeId, NodeId, NodeId) {
        let splitter = h.arena.get(h.root_id).unwrap().children[0];
        let kids = &h.arena.get(splitter).unwrap().children;
        (kids[0], kids[1], kids[2])
    }

    fn send(h: &mut Harness, st: &mut DispatchState, k: MouseKind, x: f32, y: f32) {
        let ev = MouseEvent::new(k, Point::new(x, y), Modifiers::default());
        dispatch_mouse(&mut h.root, &h.arena, &h.hit, &ev, st);
    }

    fn press(h: &mut Harness, focused: NodeId, code: KeyCode) {
        let kev = KeyEvent::new(code, Modifiers::default(), true, false);
        dispatch_key(&mut h.root, &h.arena, Some(focused), &kev);
    }

    #[test]
    fn splitter_should_size_panes_by_ratio() {
        let owner = Owner::new();
        owner.set();
        let ratio = RwSignal::new(0.7);
        let h = harness(|| splitter(ratio).pane(div()).pane(div()).into_element());
        let (a, _d, b) = parts(&h);
        // An ASYMMETRIC ratio pins the wiring: the leading pane gets `ratio`, the trailing `1 − ratio` (an
        // equal 0.5 split couldn't tell those apart, nor catch a pane_b that wrongly read `ratio`).
        let aw = h.layout.absolute_layout(a).size.w;
        let bw = h.layout.absolute_layout(b).size.w;
        assert!(
            (aw - 0.7 * USABLE_W).abs() < 0.5,
            "leading pane is ratio (0.7) of the usable width (got {aw})"
        );
        assert!(
            (bw - 0.3 * USABLE_W).abs() < 0.5,
            "trailing pane is 1 − ratio (0.3) of the usable width (got {bw})"
        );
        drop(owner);
    }

    #[test]
    fn splitter_drag_should_resize_panes() {
        let owner = Owner::new();
        owner.set();
        let ratio = RwSignal::new(0.5);
        let mut h = harness(|| splitter(ratio).pane(div()).pane(div()).into_element());
        let (_a, divider, _b) = parts(&h);
        let d = h.layout.absolute_layout(divider);
        let (cx, cy) = (d.origin.x + d.size.w / 2.0, d.origin.y + d.size.h / 2.0);
        // Grab the divider and drag it 40px right: ratio += 40 / usable (194) ≈ 0.706.
        let mut st = DispatchState::default();
        send(&mut h, &mut st, MouseKind::Down(MouseButton::Left), cx, cy);
        send(&mut h, &mut st, MouseKind::Move, cx + 40.0, cy);
        assert!(
            (ratio.get_untracked() - (0.5 + 40.0 / USABLE_W)).abs() < 0.01,
            "dragging the divider updates the ratio by delta/usable (got {})",
            ratio.get_untracked()
        );
        drop(owner);
    }

    #[test]
    fn splitter_should_clamp_min_size() {
        let owner = Owner::new();
        owner.set();
        let ratio = RwSignal::new(0.5);
        // min 50px → min ratio = 50/194 ≈ 0.258; a hard drag left clamps there, not to 0.
        let mut h = harness(|| {
            splitter(ratio)
                .min(50.0)
                .pane(div())
                .pane(div())
                .into_element()
        });
        let (_a, divider, _b) = parts(&h);
        let d = h.layout.absolute_layout(divider);
        let (cx, cy) = (d.origin.x + d.size.w / 2.0, d.origin.y + d.size.h / 2.0);
        let mut st = DispatchState::default();
        send(&mut h, &mut st, MouseKind::Down(MouseButton::Left), cx, cy);
        send(&mut h, &mut st, MouseKind::Move, cx - 1000.0, cy);
        assert!(
            (ratio.get_untracked() - 50.0 / USABLE_W).abs() < 0.01,
            "over-drag clamps to the min-pane ratio, not 0 (got {})",
            ratio.get_untracked()
        );
        drop(owner);
    }

    #[test]
    fn splitter_arrow_keys_should_resize_when_focused() {
        let owner = Owner::new();
        owner.set();
        let ratio = RwSignal::new(0.5);
        let mut h = harness(|| splitter(ratio).pane(div()).pane(div()).into_element());
        let (_a, divider, _b) = parts(&h);
        press(&mut h, divider, KeyCode::ArrowRight);
        assert!(
            (ratio.get_untracked() - (0.5 + KEY_STEP)).abs() < 1e-4,
            "ArrowRight nudges the ratio by one step (got {})",
            ratio.get_untracked()
        );
        press(&mut h, divider, KeyCode::ArrowLeft);
        assert!(
            (ratio.get_untracked() - 0.5).abs() < 1e-4,
            "ArrowLeft steps back (got {})",
            ratio.get_untracked()
        );
        drop(owner);
    }

    #[test]
    fn splitter_vertical_should_split_on_y() {
        let owner = Owner::new();
        owner.set();
        let ratio = RwSignal::new(0.5);
        let mut h = harness(|| {
            splitter(ratio)
                .vertical()
                .pane(div())
                .pane(div())
                .into_element()
        });
        let (a, divider, b) = parts(&h);
        // A vertical split divides the HEIGHT: equal panes, each half of the 94px usable height.
        let ah = h.layout.absolute_layout(a).size.h;
        let bh = h.layout.absolute_layout(b).size.h;
        assert!(
            (ah - bh).abs() < 0.5,
            "equal panes at ratio 0.5 (a={ah}, b={bh})"
        );
        // The divider drag maps to the Y axis: dragging down increases the ratio.
        let d = h.layout.absolute_layout(divider);
        let (cx, cy) = (d.origin.x + d.size.w / 2.0, d.origin.y + d.size.h / 2.0);
        let mut st = DispatchState::default();
        send(&mut h, &mut st, MouseKind::Down(MouseButton::Left), cx, cy);
        send(&mut h, &mut st, MouseKind::Move, cx, cy + 20.0);
        assert!(
            ratio.get_untracked() > 0.5,
            "a vertical divider drag down increases the ratio (got {})",
            ratio.get_untracked()
        );
        drop(owner);
    }

    #[test]
    fn splitter_up_should_release_and_stop_scrubbing() {
        let owner = Owner::new();
        owner.set();
        let ratio = RwSignal::new(0.5);
        let mut h = harness(|| splitter(ratio).pane(div()).pane(div()).into_element());
        let (_a, divider, _b) = parts(&h);
        let d = h.layout.absolute_layout(divider);
        let (cx, cy) = (d.origin.x + d.size.w / 2.0, d.origin.y + d.size.h / 2.0);
        let mut st = DispatchState::default();
        // Down + move → resize; Up releases; a further move must NOT keep scrubbing (drag flag reset).
        send(&mut h, &mut st, MouseKind::Down(MouseButton::Left), cx, cy);
        send(&mut h, &mut st, MouseKind::Move, cx + 20.0, cy);
        let after_drag = ratio.get_untracked();
        send(
            &mut h,
            &mut st,
            MouseKind::Up(MouseButton::Left),
            cx + 20.0,
            cy,
        );
        send(&mut h, &mut st, MouseKind::Move, cx + 60.0, cy);
        assert!(
            (ratio.get_untracked() - after_drag).abs() < 1e-6,
            "after release, a move does not continue scrubbing (was {after_drag}, now {})",
            ratio.get_untracked()
        );
        drop(owner);
    }

    #[test]
    fn splitter_hover_without_grab_should_not_scrub() {
        let owner = Owner::new();
        owner.set();
        let ratio = RwSignal::new(0.5);
        let mut h = harness(|| splitter(ratio).pane(div()).pane(div()).into_element());
        let (_a, divider, _b) = parts(&h);
        let d = h.layout.absolute_layout(divider);
        let (cx, cy) = (d.origin.x + d.size.w / 2.0, d.origin.y + d.size.h / 2.0);
        // A move over the divider with NO prior Down must not move the ratio (the drag flag guards it).
        let mut st = DispatchState::default();
        send(&mut h, &mut st, MouseKind::Move, cx + 30.0, cy);
        assert!(
            (ratio.get_untracked() - 0.5).abs() < 1e-6,
            "hovering the divider without a grab does not scrub (got {})",
            ratio.get_untracked()
        );
        drop(owner);
    }

    #[test]
    fn splitter_drag_should_accumulate_from_start() {
        let owner = Owner::new();
        owner.set();
        let ratio = RwSignal::new(0.5);
        let mut h = harness(|| splitter(ratio).pane(div()).pane(div()).into_element());
        let (_a, divider, _b) = parts(&h);
        let d = h.layout.absolute_layout(divider);
        let (cx, cy) = (d.origin.x + d.size.w / 2.0, d.origin.y + d.size.h / 2.0);
        // Two successive moves accumulate from the DOWN position (start-relative), not incrementally: a
        // final position 80px past the grab → ratio = 0.5 + 80/usable, not double-counted.
        let mut st = DispatchState::default();
        send(&mut h, &mut st, MouseKind::Down(MouseButton::Left), cx, cy);
        send(&mut h, &mut st, MouseKind::Move, cx + 40.0, cy);
        send(&mut h, &mut st, MouseKind::Move, cx + 80.0, cy);
        assert!(
            (ratio.get_untracked() - (0.5 + 80.0 / USABLE_W)).abs() < 0.01,
            "the ratio tracks the cursor's offset from the grab, not a doubled increment (got {})",
            ratio.get_untracked()
        );
        drop(owner);
    }

    #[test]
    fn splitter_should_clamp_to_center_when_min_exceeds_half() {
        let owner = Owner::new();
        owner.set();
        let ratio = RwSignal::new(0.5);
        // min 150 > half the 194px usable → min ratio caps at 0.5, so any drag pins the ratio to the
        // centre (a valid clamp range, no inverted-bounds panic).
        let mut h = harness(|| {
            splitter(ratio)
                .min(150.0)
                .pane(div())
                .pane(div())
                .into_element()
        });
        let (_a, divider, _b) = parts(&h);
        let d = h.layout.absolute_layout(divider);
        let (cx, cy) = (d.origin.x + d.size.w / 2.0, d.origin.y + d.size.h / 2.0);
        let mut st = DispatchState::default();
        send(&mut h, &mut st, MouseKind::Down(MouseButton::Left), cx, cy);
        send(&mut h, &mut st, MouseKind::Move, cx + 1000.0, cy);
        assert!(
            (ratio.get_untracked() - 0.5).abs() < 1e-6,
            "an over-large min pins the ratio to the centre (got {})",
            ratio.get_untracked()
        );
        drop(owner);
    }
}
