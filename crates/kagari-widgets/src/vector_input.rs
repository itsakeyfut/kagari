//! The [`VectorInput`] widget (#233): 2–4 linked numeric fields bound to an `RwSignal<[f64; N]>` — a
//! transform's position/rotation/scale, or any small vector. The core of a game-engine inspector.
//!
//! Each axis is a [`number_input`] (#76). Since `number_input` takes a concrete `RwSignal<f64>` (not a lens),
//! each array component is projected onto its own per-axis signal and kept in step with the array by a
//! **dedup'd bidirectional sync**: an `array → axes` effect pushes external (and linked-uniform) writes down
//! to the axis signals, and one `axis → array` effect per component writes edits back up. Two guards make it
//! robust (a signal re-runs its subscribers even on an equal write, and `create_effect` runs once on
//! creation): the write-back fires **only when its axis has actually diverged from the array** — so the
//! effect's seed run is a no-op (a `linked` mount never uniformises a non-uniform value) and the linked
//! broadcast can't re-fire once every axis matches (no cascade); and the equality is **NaN-safe** (`NaN` in
//! the value settles instead of recursing the two directions into a stack overflow). Axis letters are
//! colour-coded (X/Y/Z/W) and exposed to a11y via the [`label`] widget (`Role::Label`); `.linked` makes an
//! edit set every axis uniformly.

use kagari_base::SharedString;
use kagari_core::reactive::prelude::*;
use kagari_core::reactive::{RwSignal, create_effect};
use kagari_core::{AnyElement, IntoElement, div};
use kagari_style::{ColorRole, Styled};

use crate::control::ControlSize;
use crate::label::label;
use crate::number_input::number_input;

/// The default axis letter for column `i` (`X`/`Y`/`Z`/`W`); empty past four (only `vector2/3/4` exist).
fn default_label(i: usize) -> &'static str {
    ["X", "Y", "Z", "W"].get(i).copied().unwrap_or("")
}

/// The axis colour convention (X = Danger/red, Y = Success/green, Z = Info/blue, W = muted).
fn axis_color(i: usize) -> ColorRole {
    match i {
        0 => ColorRole::Danger,
        1 => ColorRole::Success,
        2 => ColorRole::Info,
        _ => ColorRole::TextMuted,
    }
}

/// NaN-safe equality for the sync guards. A plain `!=` never settles a `NaN` (`NaN != NaN`), so a bound
/// value carrying `NaN` (an external `value.set([NaN, …])`, e.g. a 0/0 scale) would make the two sync
/// directions write each other forever and overflow the stack. Treating `NaN == NaN` as "same" lets a
/// non-finite value sit still (it displays as an empty field via `number_input`).
fn same(a: f64, b: f64) -> bool {
    a == b || (a.is_nan() && b.is_nan())
}

/// A linked group of `N` numeric fields bound to an `RwSignal<[f64; N]>` (#233). Build with [`vector2`] /
/// [`vector3`] / [`vector4`]; chain [`.labels`](Self::labels)/[`.range`](Self::range)/[`.step`](Self::step)/
/// [`.precision`](Self::precision)/[`.size`](Self::size)/[`.disabled`](Self::disabled)/[`.linked`](Self::linked).
/// Returns `impl IntoElement`; `Styled` is **not** on its surface. Controlled via the array signal (D2).
pub struct VectorInput<const N: usize> {
    value: RwSignal<[f64; N]>,
    labels: [SharedString; N],
    min: f64,
    max: f64,
    step: f64,
    precision: Option<u8>,
    size: ControlSize,
    disabled: bool,
    linked: Option<RwSignal<bool>>,
}

/// Shared constructor: default `X/Y/Z/W` labels, unbounded range, unit step, no precision, `Md`, enabled, unlinked.
fn new_vector<const N: usize>(value: RwSignal<[f64; N]>) -> VectorInput<N> {
    VectorInput {
        value,
        labels: core::array::from_fn(|i| SharedString::from(default_label(i))),
        min: f64::NEG_INFINITY,
        max: f64::INFINITY,
        step: 1.0,
        precision: None,
        size: ControlSize::Md,
        disabled: false,
        linked: None,
    }
}

/// A two-field vector input bound to `value` (controlled, D2).
pub fn vector2(value: RwSignal<[f64; 2]>) -> VectorInput<2> {
    new_vector(value)
}

/// A three-field vector input bound to `value` (controlled, D2).
pub fn vector3(value: RwSignal<[f64; 3]>) -> VectorInput<3> {
    new_vector(value)
}

/// A four-field vector input bound to `value` (controlled, D2).
pub fn vector4(value: RwSignal<[f64; 4]>) -> VectorInput<4> {
    new_vector(value)
}

impl<const N: usize> VectorInput<N> {
    /// Overrides the per-axis letters (default `["X","Y","Z","W"]`). Generic over the element type, so it
    /// accepts `[&'static str; N]` (the zero-copy static path), `[String; N]`, or `[SharedString; N]` —
    /// runtime/localised labels included.
    pub fn labels<S: Into<SharedString>>(mut self, labels: [S; N]) -> Self {
        self.labels = labels.map(Into::into);
        self
    }

    /// Clamps every axis into `[min, max]` (shared across all axes). A reversed range is normalised.
    pub fn range(mut self, min: f64, max: f64) -> Self {
        self.min = min.min(max);
        self.max = min.max(max);
        self
    }

    /// Sets the stepper/arrow increment shared across all axes.
    pub fn step(mut self, step: f64) -> Self {
        self.step = step;
        self
    }

    /// Displays every axis with a fixed number of decimal places (aligned columns), #233.
    pub fn precision(mut self, precision: u8) -> Self {
        self.precision = Some(precision);
        self
    }

    /// Sets the control size (shared across all axes).
    pub fn size(mut self, size: ControlSize) -> Self {
        self.size = size;
        self
    }

    /// Disables every field: muted, display-only, and not focusable.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Binds a "linked/uniform" toggle: while it reads `true`, editing any axis sets **all** axes to that
    /// value (uniform edit). Toggling it does not retroactively uniformise — it affects the next edit.
    pub fn linked(mut self, linked: RwSignal<bool>) -> Self {
        self.linked = Some(linked);
        self
    }
}

impl<const N: usize> IntoElement for VectorInput<N> {
    fn into_element(self) -> AnyElement {
        let VectorInput {
            value,
            labels,
            min,
            max,
            step,
            precision,
            size,
            disabled,
            linked,
        } = self;

        // Per-axis signals seeded from the array — the concrete `RwSignal<f64>` each `number_input` needs.
        let axes: [RwSignal<f64>; N] =
            core::array::from_fn(|i| RwSignal::new(value.with_untracked(|a| a[i])));

        // array → axes: propagate external writes (and linked-uniform updates) down to the axis signals. The
        // NaN-safe guard makes this idempotent, so it cannot ping-pong with the write-back effects below.
        create_effect(move || {
            let arr = value.get();
            for i in 0..N {
                if !same(axes[i].get_untracked(), arr[i]) {
                    axes[i].set(arr[i]);
                }
            }
        });

        // axes[i] → array: write an edit back up, but ONLY when this axis has actually diverged from the
        // array — i.e. a real per-axis edit. This gate is load-bearing: it makes the effect's mandatory
        // first (seed) run a no-op (so a `linked` mount never uniformises a non-uniform value), stops the
        // linked broadcast from re-firing once every axis matches (no cascade), and — being NaN-safe — lets
        // a non-finite value settle instead of recursing. Linked → set every axis to `v`; else component `i`.
        for i in 0..N {
            let axis = axes[i];
            create_effect(move || {
                let v = axis.get();
                if !same(value.with_untracked(|a| a[i]), v) {
                    // Untracked: toggling the link does not fire this effect — only an actual axis edit does.
                    if linked.map(|l| l.get_untracked()).unwrap_or(false) {
                        value.set([v; N]);
                    } else {
                        value.update(|a| a[i] = v);
                    }
                }
            });
        }

        // The labelled row: a colour-coded, a11y-visible axis letter (the #71 `label` widget = `Role::Label`)
        // next to each field. Groups are left-packed at the field's natural width (uniform for any N), so a
        // 2-field row does not leave a wide gap the way `flex_grow` columns would.
        let mut row = div().flex().gap_2().items_center();
        for i in 0..N {
            let axis_label = label(labels[i].clone()).color(axis_color(i)).size(size);
            let mut field = number_input(axes[i])
                .size(size)
                .disabled(disabled)
                .range(min, max)
                .step(step);
            if let Some(p) = precision {
                field = field.precision(p);
            }
            let group = div()
                .flex()
                .items_center()
                .gap_1()
                .child(axis_label)
                .child(field);
            row = row.child(group);
        }
        row.into_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use kagari_base::{NodeId, Point, Size};
    use kagari_core::KeyCode;
    use kagari_core::arena::Arena;
    use kagari_core::damage::DamageState;
    use kagari_core::event::{
        DispatchState, FocusRegistry, HitTest, KeyEvent, Modifiers, MouseButton, MouseEvent,
        MouseKind, dispatch_key, dispatch_mouse,
    };
    use kagari_core::paint::render_tree;
    use kagari_core::reactive::{Owner, provide_context};
    use kagari_layout::LayoutTree;
    use kagari_render::Scene;
    use kagari_style::Theme;
    use kagari_text::{FontDb, TextSystem};

    const VIEWPORT: Size = Size { w: 400.0, h: 60.0 };

    struct Harness {
        root: AnyElement,
        arena: Arena,
        layout: LayoutTree,
        text: TextSystem,
        hit: HitTest,
        focus: FocusRegistry,
        damage: Arc<DamageState>,
        theme: Theme,
        root_id: NodeId,
    }

    fn harness(widget: impl IntoElement) -> Harness {
        let focus = FocusRegistry::new();
        provide_context(focus.clone());
        let mut h = Harness {
            root: widget.into_element(),
            arena: Arena::new(),
            layout: LayoutTree::new(),
            text: TextSystem::new(FontDb::new()),
            hit: HitTest::new(),
            focus,
            damage: Arc::new(DamageState::default()),
            theme: Theme::light(),
            root_id: NodeId::from_raw(0),
        };
        h.root_id = render(&mut h);
        h
    }

    fn render(h: &mut Harness) -> NodeId {
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

    /// The editor node of axis `i`: row → group[i] → [label, number_input field] → field.children[0] = editor.
    fn axis_editor(h: &Harness, i: usize) -> NodeId {
        let group = h.arena.get(h.root_id).unwrap().children[i];
        let field = h.arena.get(group).unwrap().children[1];
        h.arena.get(field).unwrap().children[0]
    }

    /// The absolute centre of a laid-out node (layout is parent-relative — accumulate ancestor origins).
    fn abs_center(h: &Harness, target: NodeId) -> Point {
        fn walk(h: &Harness, node: NodeId, ox: f32, oy: f32, target: NodeId) -> Option<Point> {
            let r = h.layout.layout(node);
            let (ax, ay) = (ox + r.origin.x, oy + r.origin.y);
            if node == target {
                return Some(Point::new(ax + r.size.w / 2.0, ay + r.size.h / 2.0));
            }
            for &c in &h.arena.get(node)?.children {
                if let Some(p) = walk(h, c, ax, ay, target) {
                    return Some(p);
                }
            }
            None
        }
        walk(h, h.root_id, 0.0, 0.0, target).expect("node laid out")
    }

    /// Clicks (down) at `p` to focus whatever editor is there.
    fn focus_at(h: &mut Harness, p: Point) {
        let mut st = DispatchState::default();
        let ev = MouseEvent::new(MouseKind::Down(MouseButton::Left), p, Modifiers::default());
        dispatch_mouse(&mut h.root, &h.arena, &h.hit, &ev, &mut st);
    }

    /// Presses a key at the currently focused node.
    fn press(h: &mut Harness, code: KeyCode) {
        let kev = KeyEvent::new(code, Modifiers::default(), true, false);
        dispatch_key(&mut h.root, &h.arena, h.focus.focused_node(), &kev);
    }

    #[test]
    fn vector_input_should_render_n_fields() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new([1.0, 2.0, 3.0]);
        let h = harness(vector3(value));
        assert_eq!(
            h.arena.get(h.root_id).unwrap().children.len(),
            3,
            "vector3 renders three axis groups"
        );
        drop(owner);
    }

    #[test]
    fn vector_input_should_edit_each_axis() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new([1.0_f64, 2.0, 3.0]);
        let mut h = harness(vector3(value).step(1.0));
        // Focus axis 1's field and step it up → only value[1] changes (axes[1] → array write-back).
        let ed = axis_editor(&h, 1);
        let p = abs_center(&h, ed);
        focus_at(&mut h, p);
        press(&mut h, KeyCode::ArrowUp);
        assert_eq!(
            value.get_untracked(),
            [1.0, 3.0, 3.0],
            "stepping axis 1 updates only component 1"
        );
        drop(owner);
    }

    #[test]
    fn vector_input_linked_should_edit_uniformly() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new([1.0_f64, 2.0, 3.0]);
        let linked = RwSignal::new(true);
        let mut h = harness(vector3(value).step(1.0).linked(linked));
        // Step axis 0 (1 → 2); linked → every axis becomes 2.
        let ed = axis_editor(&h, 0);
        let p = abs_center(&h, ed);
        focus_at(&mut h, p);
        press(&mut h, KeyCode::ArrowUp);
        assert_eq!(
            value.get_untracked(),
            [2.0, 2.0, 2.0],
            "a linked edit sets every axis uniformly"
        );
        drop(owner);
    }

    #[test]
    fn vector_input_external_set_should_update_axes() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new([1.0_f64, 2.0, 3.0]);
        let mut h = harness(vector3(value).step(1.0));
        // External write → the array→axes effect updates the per-axis signals (and their displays). Prove it
        // by stepping axis 1: it builds on the new 8 (→ 9), not the stale 2 (→ 3).
        value.set([7.0, 8.0, 9.0]);
        let ed = axis_editor(&h, 1);
        let p = abs_center(&h, ed);
        focus_at(&mut h, p);
        press(&mut h, KeyCode::ArrowUp);
        assert_eq!(
            value.get_untracked(),
            [7.0, 9.0, 9.0],
            "an external set updates the axis display; a step builds on it (8 → 9)"
        );
        drop(owner);
    }

    #[test]
    fn vector_input_disabled_should_be_inert() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new([1.0_f64, 2.0, 3.0]);
        let mut h = harness(vector3(value).disabled(true));
        let ed = axis_editor(&h, 0);
        let p = abs_center(&h, ed);
        focus_at(&mut h, p);
        assert_eq!(
            h.focus.focused_node(),
            None,
            "a disabled vector input mounts no editors and is not focusable"
        );
        drop(owner);
    }

    #[test]
    fn vector_input_should_render_two_and_four_fields() {
        let owner = Owner::new();
        owner.set();
        let h2 = harness(vector2(RwSignal::new([0.0_f64, 0.0])));
        assert_eq!(
            h2.arena.get(h2.root_id).unwrap().children.len(),
            2,
            "vector2 renders two axis groups"
        );
        let h4 = harness(vector4(RwSignal::new([0.0_f64, 0.0, 0.0, 0.0])));
        assert_eq!(
            h4.arena.get(h4.root_id).unwrap().children.len(),
            4,
            "vector4 renders four axis groups (exercises the W axis)"
        );
        drop(owner);
    }

    #[test]
    fn vector_input_axis_metadata_should_follow_convention() {
        // Pure helpers — the X/Y/Z/W colour + letter convention (the W branch has no other test path).
        assert_eq!(axis_color(0), ColorRole::Danger);
        assert_eq!(axis_color(1), ColorRole::Success);
        assert_eq!(axis_color(2), ColorRole::Info);
        assert_eq!(
            axis_color(3),
            ColorRole::TextMuted,
            "W and beyond are muted"
        );
        assert_eq!(default_label(0), "X");
        assert_eq!(default_label(3), "W");
    }

    #[test]
    fn vector_input_linked_mount_should_not_clobber() {
        let owner = Owner::new();
        owner.set();
        // Mounting with linked=true and a NON-uniform value must not uniformise it (the effect's seed run
        // is a no-op; `.linked` only affects the next real edit). Regression for the mount-clobber bug.
        let value = RwSignal::new([1.0_f64, 2.0, 3.0]);
        let _h = harness(vector3(value).linked(RwSignal::new(true)));
        assert_eq!(
            value.get_untracked(),
            [1.0, 2.0, 3.0],
            "a linked mount leaves a non-uniform value untouched"
        );
        drop(owner);
    }

    #[test]
    fn vector_input_linked_external_set_should_be_respected() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new([0.0_f64, 0.0, 0.0]);
        let _h = harness(vector3(value).linked(RwSignal::new(true)));
        // An external non-uniform write while linked is respected as-is (no cascade to `[last; N]`).
        value.set([1.0, 2.0, 3.0]);
        assert_eq!(
            value.get_untracked(),
            [1.0, 2.0, 3.0],
            "an external set is not re-broadcast into a uniform value"
        );
        drop(owner);
    }

    #[test]
    fn vector_input_nonfinite_should_not_recurse() {
        let owner = Owner::new();
        owner.set();
        // A NaN in the bound value must not make the array↔axes sync recurse (it would overflow the stack).
        // Reaching the assertion (no stack overflow) is the test; the finite axis still round-trips.
        let value = RwSignal::new([f64::NAN, 1.0]);
        let _h = harness(vector2(value));
        assert_eq!(
            value.get_untracked()[1],
            1.0,
            "the finite axis is unaffected"
        );
        drop(owner);
    }
}
