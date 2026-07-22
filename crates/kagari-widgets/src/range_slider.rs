//! The [`RangeSlider`] widget (#234): a two-thumb slider selecting a `[lo, hi]` range bound to an
//! `RwSignal<(f32, f32)>` (in/out points, value clamps). Extends [`Slider`](crate::Slider) (#75).
//!
//! Composition reuses the #75 flex "thumb-as-divider" idiom, generalised to **3 track segments + 2 thumbs**
//! (`Div` has no absolute positioning): `[before, thumbLo, between, thumbHi, after]` horizontally
//! (`[after, thumbHi, between, thumbLo, before]` vertically, top = max). The two thumbs are fixed `T` wide
//! and the three segments `flex_grow` over the free space `U = length − 2T`, so
//! `before = frac(lo)·U`, `between = (frac(hi) − frac(lo))·U` (the filled range), `after = (1 − frac(hi))·U`.
//! A thumb's centre from the track start is `frac·U + T/2` for the layout-start thumb and `frac·U + 3T/2`
//! for the other, which is the inverse the drag map uses. The thumbs cannot cross (`lo ≤ hi`); each is
//! independently focusable (two tab stops) and moves under the arrow/Page/Home/End keys when focused.

use std::cell::Cell;
use std::rc::Rc;

use kagari_base::{Axis, Point, SharedString, Size};
use kagari_core::element::Div;
use kagari_core::reactive::prelude::*;
use kagari_core::reactive::{RwSignal, rx};
use kagari_core::{
    AnyElement, BoundsHandle, CursorIcon, FocusHandle, IntoElement, KeyCode, MouseButton,
    MouseKind, Role, div, use_focus_handle,
};
use kagari_style::{ColorRole, Styled};

use crate::control::{ControlSize, fraction, sane, slider_dims, snap};

/// Minimum track length (logical px) so a bare range slider is usable; it still `flex_grow`s to fill a wider slot.
const RANGE_SLIDER_MIN_LEN: f32 = 120.0;

/// Which of the two thumbs a drag/keyboard interaction targets.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Thumb {
    Lo,
    Hi,
}

/// Builds one thumb: a [`Switch`](crate::Switch)-style knob (rounded + drop shadow) that, when `focus` is
/// present (enabled), shows the focus ring and moves *its* component of `value` under the keyboard. Always
/// carries a11y ([`Role::Slider`] + a `minimum`/`maximum` label + the reactive value, #75) so a screen reader
/// distinguishes the two thumbs by name. `focus.is_some()` ⇒ enabled (RK-026).
#[allow(clippy::too_many_arguments)] // a cohesive thumb-build call: the slider model + style in one place
fn build_thumb(
    thumb: Thumb,
    focus: Option<&FocusHandle>,
    value: RwSignal<(f32, f32)>,
    min: f32,
    max: f32,
    step: f32,
    thumb_bg: ColorRole,
    thumb_px: f32,
) -> Div {
    let mut t = div().rounded_full().shadow_sm().bg(thumb_bg).size(Size {
        w: thumb_px,
        h: thumb_px,
    });
    if let Some(f) = focus {
        let ring = f.clone();
        t = t
            .border_w_2()
            .border_color(rx(move || {
                ring.is_focus_visible().then_some(ColorRole::FocusRing)
            }))
            .track_focus(f);
    }
    // A11y (#75 machinery): each thumb is a slider with a `minimum`/`maximum` name (so a screen reader tells
    // the two apart) announcing its own value, resolved reactively at paint.
    let label = if matches!(thumb, Thumb::Lo) {
        "minimum"
    } else {
        "maximum"
    };
    t = t
        .role(Role::Slider)
        .a11y_label(label)
        .a11y_value(rx(move || {
            let (lo, hi) = value.get();
            let v = if matches!(thumb, Thumb::Lo) { lo } else { hi };
            SharedString::from(format!("{v}"))
        }));
    // Keyboard moves this thumb when enabled. Arrows/Page snap to the step; Home/End set the thumb's exact
    // bound (min / max / the other thumb) without snapping. The no-cross clamp keeps `lo <= hi`.
    if focus.is_some() {
        t = t.on_key_down(move |kev, _cx| {
            if !kev.pressed {
                return;
            }
            let (lo, hi) = value.get_untracked();
            let (lo, hi) = (sane(lo, min, max), sane(hi, min, max));
            let cur = if matches!(thumb, Thumb::Lo) { lo } else { hi };
            let kstep = if step > 0.0 {
                step
            } else {
                (max - min) / 100.0
            };
            let page = if step > 0.0 {
                step * 10.0
            } else {
                (max - min) / 10.0
            };
            let next = match kev.code {
                KeyCode::ArrowUp | KeyCode::ArrowRight => snap(cur + kstep, min, max, step),
                KeyCode::ArrowDown | KeyCode::ArrowLeft => snap(cur - kstep, min, max, step),
                KeyCode::PageUp => snap(cur + page, min, max, step),
                KeyCode::PageDown => snap(cur - page, min, max, step),
                KeyCode::Home => {
                    if matches!(thumb, Thumb::Lo) {
                        min
                    } else {
                        lo
                    }
                }
                KeyCode::End => {
                    if matches!(thumb, Thumb::Lo) {
                        hi
                    } else {
                        max
                    }
                }
                _ => return,
            };
            match thumb {
                Thumb::Lo => value.set((next.min(hi), hi)),
                Thumb::Hi => value.set((lo, next.max(lo))),
            }
        });
    }
    t
}

/// A range slider (#234): a `[lo, hi]` selection over `[min, max]` bound to an `RwSignal<(f32, f32)>`, driven
/// by drag (pointer capture, nearest-thumb click-to-set) or the keyboard. Build with [`range_slider`]; chain
/// [`.range`](Self::range)/[`.step`](Self::step)/[`.size`](Self::size)/[`.disabled`](Self::disabled)/
/// [`.vertical`](Self::vertical). Returns `impl IntoElement`; `Styled` is not on its surface.
pub struct RangeSlider {
    value: RwSignal<(f32, f32)>,
    min: f32,
    max: f32,
    step: f32,
    size: ControlSize,
    disabled: bool,
    axis: Axis,
}

/// Creates a range slider bound to `value` (controlled, D2), range `0.0..=1.0`, continuous, horizontal.
pub fn range_slider(value: RwSignal<(f32, f32)>) -> RangeSlider {
    RangeSlider {
        value,
        min: 0.0,
        max: 1.0,
        step: 0.0,
        size: ControlSize::Md,
        disabled: false,
        axis: Axis::Horizontal,
    }
}

impl RangeSlider {
    /// Sets the value range (`min..=max`). Both thumbs are clamped to it and span it.
    pub fn range(mut self, min: f32, max: f32) -> Self {
        self.min = min;
        self.max = max;
        self
    }

    /// Sets the step both thumbs snap to (drag + keyboard). `0.0` (the default) = continuous.
    pub fn step(mut self, step: f32) -> Self {
        self.step = step;
        self
    }

    /// Sets the control size (track thickness + thumb size).
    pub fn size(mut self, size: ControlSize) -> Self {
        self.size = size;
        self
    }

    /// Disables the range slider: muted styling and inert (no drag / keyboard), and not focusable.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Lays the range slider out horizontally (left = min, right = max) — the default.
    pub fn horizontal(mut self) -> Self {
        self.axis = Axis::Horizontal;
        self
    }

    /// Lays the range slider out vertically (bottom = min, top = max — a fader): a drag up increases the value.
    pub fn vertical(mut self) -> Self {
        self.axis = Axis::Vertical;
        self
    }
}

impl IntoElement for RangeSlider {
    fn into_element(self) -> AnyElement {
        let RangeSlider {
            value,
            min,
            max,
            step,
            size,
            disabled,
            axis,
        } = self;
        let vertical = matches!(axis, Axis::Vertical);
        let (track_h, thumb_px) = slider_dims(size);
        let (track_h, thumb_px) = (track_h.0, thumb_px.0);

        // The container publishes its laid-out bounds; the drag reads the main-axis length (RK: paint-cell,
        // read untracked in handlers — never a signal).
        let bounds = BoundsHandle::new();

        // Track segments: before/after unfilled (Border), between filled (Accent) — the selected range. A thin
        // fixed cross-thickness (`track_h`) + `flex_grow` on the main axis (RK-030: Border, not white Surface).
        let (filled_bg, rest_bg) = if disabled {
            (ColorRole::BorderStrong, ColorRole::Border)
        } else {
            (ColorRole::Accent, ColorRole::Border)
        };
        let seg = |bg: ColorRole, d: Div| {
            let d = d.rounded_full().bg(bg);
            if vertical {
                d.min_w(track_h).min_h(0.0)
            } else {
                d.min_h(track_h).min_w(0.0)
            }
        };
        let before = seg(
            rest_bg,
            div().flex_grow(rx(move || fraction(value.get().0, min, max))),
        );
        let between = seg(
            filled_bg,
            div().flex_grow(rx(move || {
                let (lo, hi) = value.get();
                (fraction(hi, min, max) - fraction(lo, min, max)).max(0.0)
            })),
        );
        let after = seg(
            rest_bg,
            div().flex_grow(rx(move || 1.0 - fraction(value.get().1, min, max))),
        );

        // Two thumbs. Mint the lo handle before the hi handle so Tab visits lo then hi (tree order = mint
        // order). Focus (and thus keyboard) is present only when enabled (RK-026).
        let focus_lo = (!disabled).then(use_focus_handle);
        let focus_hi = (!disabled).then(use_focus_handle);
        let thumb_bg = if disabled {
            ColorRole::Surface
        } else {
            ColorRole::AccentFg
        };
        let thumb_lo = build_thumb(
            Thumb::Lo,
            focus_lo.as_ref(),
            value,
            min,
            max,
            step,
            thumb_bg,
            thumb_px,
        );
        let thumb_hi = build_thumb(
            Thumb::Hi,
            focus_hi.as_ref(),
            value,
            min,
            max,
            step,
            thumb_bg,
            thumb_px,
        );

        // The root fills the slot its parent gives it (a definite main-axis length so the thumbs can travel).
        let mut root = if vertical {
            div().flex_col()
        } else {
            div().flex()
        };
        root = root
            .items_center()
            .flex_grow(1.0)
            .cursor(CursorIcon::Pointer)
            .track_bounds(&bounds);
        root = if vertical {
            root.min_h(RANGE_SLIDER_MIN_LEN).min_w(0.0)
        } else {
            root.min_w(RANGE_SLIDER_MIN_LEN).min_h(0.0)
        };

        if !disabled {
            // Pointer → value for a thumb: the fraction of the pointer along the usable travel
            // (`length − 2·thumb`), using the layout offset for that thumb so it tracks the cursor. Vertical
            // inverts (top = max). The no-cross clamp keeps `lo ≤ hi`.
            let set: Rc<dyn Fn(Thumb, Point)> = {
                let bounds = bounds.clone();
                Rc::new(move |thumb: Thumb, p: Point| {
                    let b = bounds.get();
                    let (coord, origin, len) = if vertical {
                        (p.y, b.origin.y, b.size.h)
                    } else {
                        (p.x, b.origin.x, b.size.w)
                    };
                    let usable = len - 2.0 * thumb_px;
                    if usable <= 1.0 {
                        return;
                    }
                    let is_start = (!vertical && matches!(thumb, Thumb::Lo))
                        || (vertical && matches!(thumb, Thumb::Hi));
                    let offset = if is_start {
                        thumb_px / 2.0
                    } else {
                        3.0 * thumb_px / 2.0
                    };
                    let mut f = ((coord - origin - offset) / usable).clamp(0.0, 1.0);
                    if vertical {
                        f = 1.0 - f;
                    }
                    let v = snap(min + f * (max - min), min, max, step);
                    let (lo, hi) = value.get_untracked();
                    let (lo, hi) = (sane(lo, min, max), sane(hi, min, max));
                    match thumb {
                        Thumb::Lo => value.set((v.min(hi), hi)),
                        Thumb::Hi => value.set((lo, v.max(lo))),
                    }
                })
            };
            // The thumb whose centre is nearest the pointer (grab, or a bare track click moves the closer
            // thumb — intuitive even at `lo == hi`, where the two centres are `thumb` apart).
            let pick: Rc<dyn Fn(Point) -> Thumb> = {
                let bounds = bounds.clone();
                Rc::new(move |p: Point| {
                    let b = bounds.get();
                    let (coord, origin, len) = if vertical {
                        (p.y, b.origin.y, b.size.h)
                    } else {
                        (p.x, b.origin.x, b.size.w)
                    };
                    let usable = (len - 2.0 * thumb_px).max(0.0);
                    let (lo, hi) = value.get_untracked();
                    let (flo, fhi) = (fraction(lo, min, max), fraction(hi, min, max));
                    let (c_lo, c_hi) = if vertical {
                        (
                            (1.0 - flo) * usable + 3.0 * thumb_px / 2.0,
                            (1.0 - fhi) * usable + thumb_px / 2.0,
                        )
                    } else {
                        (
                            flo * usable + thumb_px / 2.0,
                            fhi * usable + 3.0 * thumb_px / 2.0,
                        )
                    };
                    let rel = coord - origin;
                    if (rel - c_lo).abs() <= (rel - c_hi).abs() {
                        Thumb::Lo
                    } else {
                        Thumb::Hi
                    }
                })
            };
            let active = Rc::new(Cell::new(None::<Thumb>));
            let (active_d, active_m, active_u) = (active.clone(), active.clone(), active);
            let (set_d, set_m) = (set.clone(), set);
            let (focus_lo_d, focus_hi_d) = (focus_lo.clone(), focus_hi.clone());
            root = root
                .on_mouse_down(move |ev, cx| {
                    if let MouseKind::Down(MouseButton::Left) = ev.kind {
                        let thumb = pick(ev.pos);
                        cx.capture_pointer();
                        active_d.set(Some(thumb));
                        // Focus the grabbed thumb so subsequent arrow keys move it.
                        let f = if matches!(thumb, Thumb::Lo) {
                            &focus_lo_d
                        } else {
                            &focus_hi_d
                        };
                        if let Some(f) = f {
                            f.focus();
                        }
                        set_d(thumb, ev.pos);
                    }
                })
                .on_mouse_move(move |ev, _cx| {
                    if let Some(thumb) = active_m.get() {
                        set_m(thumb, ev.pos);
                    }
                })
                .on_mouse_up(move |ev, cx| {
                    if let MouseKind::Up(MouseButton::Left) = ev.kind {
                        if active_u.take().is_some() {
                            cx.release_pointer();
                        }
                    }
                });
        }

        // filled (between) sits at the min side: [before, thumbLo, between, thumbHi, after] horizontally
        // (left = min); vertically the order reverses so the filled range stays between the thumbs (top = max).
        root = if vertical {
            root.child(after)
                .child(thumb_hi)
                .child(between)
                .child(thumb_lo)
                .child(before)
        } else {
            root.child(before)
                .child(thumb_lo)
                .child(between)
                .child(thumb_hi)
                .child(after)
        };
        root.into_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc as StdArc;

    use kagari_base::{NodeId, Size as BSize};
    use kagari_core::arena::Arena;
    use kagari_core::damage::DamageState;
    use kagari_core::event::FocusRegistry;
    use kagari_core::paint::render_tree;
    use kagari_core::reactive::{Owner, provide_context};
    use kagari_core::{
        DispatchState, HitTest, KeyEvent, Modifiers, MouseButton, MouseEvent, MouseKind,
        dispatch_key, dispatch_mouse,
    };
    use kagari_layout::LayoutTree;
    use kagari_render::Scene;
    use kagari_style::Theme;
    use kagari_text::{FontDb, TextSystem};

    const VIEWPORT: BSize = BSize { w: 220.0, h: 220.0 };
    /// The slot the range slider fills in tests → a definite track length for the drag map.
    const SLOT: f32 = 200.0;

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

    /// Builds `rs` inside a `SLOT`-sized flex slot (definite track length) and frames it. `w`/`h` set the slot.
    fn harness_sized(rs: RangeSlider, w: f32, h: f32) -> Harness {
        let focus = FocusRegistry::new();
        provide_context(focus.clone());
        let root = div().flex().size(BSize { w, h }).child(rs).into_element();
        let mut h = Harness {
            root,
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

    fn harness(rs: RangeSlider) -> Harness {
        harness_sized(rs, SLOT, 40.0)
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

    /// The range-slider root node (the sized slot's only child).
    fn slider_node(h: &Harness) -> NodeId {
        h.arena.get(h.root_id).unwrap().children[0]
    }

    /// The nth child of the range-slider root (segments/thumbs, in child order).
    fn child(h: &Harness, n: usize) -> NodeId {
        h.arena.get(slider_node(h)).unwrap().children[n]
    }

    fn press(h: &mut Harness, focused: NodeId, code: KeyCode) {
        let kev = KeyEvent::new(code, Modifiers::default(), true, false);
        dispatch_key(&mut h.root, &h.arena, Some(focused), &kev);
    }

    /// Dispatches a mouse-down (+ optional moves) then up at the given main-axis coords.
    fn drag(h: &mut Harness, coords: &[f32], cross: f32, vertical: bool) {
        let pt = |c: f32| {
            if vertical {
                kagari_base::Point::new(cross, c)
            } else {
                kagari_base::Point::new(c, cross)
            }
        };
        let mut st = DispatchState::default();
        for (i, &c) in coords.iter().enumerate() {
            let kind = if i == 0 {
                MouseKind::Down(MouseButton::Left)
            } else {
                MouseKind::Move
            };
            let ev = MouseEvent::new(kind, pt(c), Modifiers::default());
            dispatch_mouse(&mut h.root, &h.arena, &h.hit, &ev, &mut st);
        }
        let up = MouseEvent::new(
            MouseKind::Up(MouseButton::Left),
            pt(*coords.last().unwrap()),
            Modifiers::default(),
        );
        dispatch_mouse(&mut h.root, &h.arena, &h.hit, &up, &mut st);
    }

    #[test]
    fn range_slider_should_bound_range() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new((0.25_f32, 0.75_f32));
        let h = harness(range_slider(value)); // range 0..1, thumb 18 (Md); usable = 200 - 36 = 164
        // between is the 3rd child ([before, thumbLo, between, thumbHi, after]); width ≈ (0.75-0.25)*164 = 82.
        let between = h.layout.absolute_layout(child(&h, 2)).size.w;
        assert!(
            (between - 82.0).abs() < 12.0,
            "the filled range reflects hi-lo (~82, got {between})"
        );
        drop(owner);
    }

    #[test]
    fn range_slider_thumbs_should_not_cross() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new((0.4_f32, 0.6_f32));
        let mut h = harness(range_slider(value));
        // Grab lo (press near its centre ≈ 0.4*164+9 = 74.6), then drag past the right end → clamps to hi.
        drag(&mut h, &[74.0, 300.0], 20.0, false);
        let (lo, hi) = value.get_untracked();
        assert!(lo <= hi, "lo never exceeds hi (lo={lo}, hi={hi})");
        assert!(
            (lo - hi).abs() < 1e-3,
            "dragging lo past hi clamps them together (lo={lo}, hi={hi})"
        );
        drop(owner);
    }

    #[test]
    fn range_slider_drag_should_move_nearest_thumb() {
        let owner = Owner::new();
        owner.set();
        // Press near lo → lo moves, hi unchanged.
        let value = RwSignal::new((0.2_f32, 0.8_f32));
        let mut h = harness(range_slider(value));
        drag(&mut h, &[60.0], 20.0, false); // nearer lo (c_lo≈41.8) than hi (c_hi≈158.2)
        let (lo, hi) = value.get_untracked();
        assert!(lo > 0.2, "pressing near lo raises lo (got {lo})");
        assert!((hi - 0.8).abs() < 1e-3, "hi is untouched (got {hi})");
        // Press near hi → hi moves, lo unchanged.
        let value2 = RwSignal::new((0.2_f32, 0.8_f32));
        let mut h2 = harness(range_slider(value2));
        drag(&mut h2, &[150.0], 20.0, false);
        let (lo2, hi2) = value2.get_untracked();
        assert!(hi2 < 0.8, "pressing near hi lowers hi (got {hi2})");
        assert!((lo2 - 0.2).abs() < 1e-3, "lo is untouched (got {lo2})");
        drop(owner);
    }

    #[test]
    fn range_slider_keyboard_should_move_focused_thumb() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new((3.0_f32, 7.0_f32));
        let mut h = harness(range_slider(value).range(0.0, 10.0).step(1.0));
        // Horizontal children: [before, thumbLo(1), between, thumbHi(3), after]. Focus lo, ArrowRight → lo+1.
        let thumb_lo = child(&h, 1);
        press(&mut h, thumb_lo, KeyCode::ArrowRight);
        assert_eq!(
            value.get_untracked().0,
            4.0,
            "ArrowRight on lo adds one step"
        );
        // Focus hi, ArrowRight → hi+1.
        let thumb_hi = child(&h, 3);
        press(&mut h, thumb_hi, KeyCode::ArrowRight);
        assert_eq!(
            value.get_untracked().1,
            8.0,
            "ArrowRight on hi adds one step"
        );
        // Home on lo → min; End on hi → max.
        press(&mut h, thumb_lo, KeyCode::Home);
        assert_eq!(value.get_untracked().0, 0.0, "Home sends lo to min");
        press(&mut h, thumb_hi, KeyCode::End);
        assert_eq!(value.get_untracked().1, 10.0, "End sends hi to max");
        drop(owner);
    }

    #[test]
    fn range_slider_vertical_drag_should_map_bottom_min() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new((0.3_f32, 0.5_f32));
        // A vertical range slider in a tall slot: bottom = min, top = max.
        let mut h = harness_sized(range_slider(value).vertical(), 40.0, SLOT);
        // Press near the top (small y) → nearest thumb is hi (top), and it rises toward max.
        drag(&mut h, &[12.0], 20.0, true);
        let (lo, hi) = value.get_untracked();
        assert!(
            hi > 0.85 && lo <= hi,
            "dragging the top thumb up raises hi toward max (lo={lo}, hi={hi})"
        );
        drop(owner);
    }

    #[test]
    fn range_slider_external_write_should_move_thumbs() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new((0.4_f32, 0.5_f32));
        let mut h = harness(range_slider(value));
        let between = child(&h, 2);
        let w_before = h.layout.absolute_layout(between).size.w;
        value.set((0.1, 0.9));
        h.root_id = frame(&mut h);
        let w_after = h.layout.absolute_layout(between).size.w;
        assert!(
            w_after > w_before + 40.0,
            "widening the range widens the filled portion ({w_before} -> {w_after})"
        );
        drop(owner);
    }

    #[test]
    fn range_slider_disabled_should_be_inert() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new((0.3_f32, 0.6_f32));
        let mut h = harness(range_slider(value).disabled(true));
        drag(&mut h, &[100.0, 180.0], 20.0, false);
        assert_eq!(
            value.get_untracked(),
            (0.3, 0.6),
            "a disabled range slider ignores drag"
        );
        drop(owner);
    }
}
