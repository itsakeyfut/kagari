//! The [`Segmented`] and [`ToggleGroup`] widgets (#230): a row of button-styled segments.
//!
//! [`Segmented`] is **exclusive** — it binds one `RwSignal<T>`, and selecting a segment sets the signal
//! to that segment's `T` (like [`RadioGroup`](crate::RadioGroup), but button-styled instead of a mark).
//! [`ToggleGroup`] is **multi-select** — it binds a `RwSignal<HashSet<T>>`, and each segment toggles its
//! `T` in/out of the set. Both are common editor controls (view/tool modes, transform space, alignment).
//!
//! **Focus**: the group is one focus stop. In [`Segmented`] the arrow keys move the *selection*
//! (wrapping), mirroring [`RadioGroup`](crate::RadioGroup). In [`ToggleGroup`] the arrows move a *roving
//! focus* index and Space/Enter toggles the focused segment (a value can't double as a cursor when many
//! are selected). Each unselected segment carries a visible `Border` outline so it stays legible on a
//! light (white) surface, where the raised-surface fill has no contrast; the ring recolors that same
//! border when the group is focus-visible.

use std::collections::HashSet;
use std::hash::Hash;

use kagari_base::SharedString;
use kagari_core::reactive::prelude::*;
use kagari_core::reactive::{Prop, RwSignal, rx};
use kagari_core::{
    AnyElement, IconId, IntoElement, KeyCode, Role, div, icon, text, use_focus_handle,
};
use kagari_style::{ColorRole, Styled};

use crate::control::{ControlSize, apply_size, label_px};

/// The content of a segment: a text label or a bundled [`IconId`]. Built from `&str` / `String` /
/// [`SharedString`] (→ a label) or an [`IconId`] (→ an icon), so `.segment(v, "Left")` and
/// `.segment(v, IconId::Check)` both work through `impl Into<SegmentContent>`.
pub enum SegmentContent {
    /// A text label.
    Label(SharedString),
    /// A bundled monochrome icon (tinted by the segment's state, like a label).
    Icon(IconId),
}

impl From<&'static str> for SegmentContent {
    fn from(s: &'static str) -> Self {
        SegmentContent::Label(s.into())
    }
}

impl From<String> for SegmentContent {
    fn from(s: String) -> Self {
        SegmentContent::Label(s.into())
    }
}

impl From<SharedString> for SegmentContent {
    fn from(s: SharedString) -> Self {
        SegmentContent::Label(s)
    }
}

impl From<IconId> for SegmentContent {
    fn from(id: IconId) -> Self {
        SegmentContent::Icon(id)
    }
}

/// Renders a segment's content tinted by a reactive `fg` role at the size's label metrics. Both leaf
/// kinds take a reactive `color_role`, so the tint follows the selection signal live (a reactive prop,
/// not a mounted/unmounted `dyn_if`).
fn render_content(content: SegmentContent, fg: Prop<ColorRole>, size: ControlSize) -> AnyElement {
    let px = label_px(size);
    match content {
        SegmentContent::Label(s) => text(s).color_role(fg).size(px).into_element(),
        SegmentContent::Icon(id) => icon(id).color_role(fg).size(px).into_element(),
    }
}

/// The screen-reader label for a segment: the text of a [`SegmentContent::Label`], or `None` for an
/// icon-only segment (an accessible name for icon segments is a follow-up).
fn content_a11y_label(content: &SegmentContent) -> Option<SharedString> {
    match content {
        SegmentContent::Label(s) => Some(s.clone()),
        SegmentContent::Icon(_) => None,
    }
}

/// Base styling shared by every segment box (both widgets): a centered, rounded, bordered cell padded to
/// the control size. Callers add the `bg` / `border_color` / `a11y` / handlers for their state model.
fn segment_base(size: ControlSize, role: Role) -> kagari_core::element::Div {
    let seg = div()
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .border_w_2()
        .role(role);
    apply_size(seg, size)
}

/// An exclusive segmented control bound to a `T` signal (#230). Build with [`segmented`]; add segments
/// with `.segment(value, content)`, then chain `.size(..)` / `.disabled(..)`. Selecting a segment (click,
/// or an arrow key while focused) sets the bound signal to that segment's `T`.
pub struct Segmented<T> {
    value: RwSignal<T>,
    segments: Vec<(T, SegmentContent)>,
    size: ControlSize,
    disabled: bool,
}

/// Creates an exclusive segmented control bound to `value` (controlled: selecting a segment sets `value`
/// to its `T`).
pub fn segmented<T: Clone + PartialEq + Send + Sync + 'static>(value: RwSignal<T>) -> Segmented<T> {
    Segmented {
        value,
        segments: Vec::new(),
        size: ControlSize::Md,
        disabled: false,
    }
}

impl<T: Clone + PartialEq + Send + Sync + 'static> Segmented<T> {
    /// Adds a segment: its bound `value` and its content (a label or an icon). Selecting it sets the
    /// group signal to `value`.
    pub fn segment(mut self, value: T, content: impl Into<SegmentContent>) -> Self {
        self.segments.push((value, content.into()));
        self
    }

    /// Sets the control size (D3).
    pub fn size(mut self, size: ControlSize) -> Self {
        self.size = size;
        self
    }

    /// Disables the control: muted styling, inert (no selection), and not focusable.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

impl<T: Clone + PartialEq + Send + Sync + 'static> IntoElement for Segmented<T> {
    fn into_element(self) -> AnyElement {
        let Segmented {
            value,
            segments,
            size,
            disabled,
        } = self;

        // One focus handle for the whole group — enabled only.
        let handle = (!disabled).then(use_focus_handle);

        let mut row = div().flex().gap_1().role(Role::RadioGroup);

        // Arrow keys move the selection within the group (wrapping); the group is the focused node.
        if let Some(h) = &handle {
            let vals: Vec<T> = segments.iter().map(|(v, _)| v.clone()).collect();
            let n = vals.len();
            row = row.track_focus(h).on_key_down(move |kev, _cx| {
                if kev.repeat || n == 0 {
                    return;
                }
                let down = matches!(kev.code, KeyCode::ArrowDown | KeyCode::ArrowRight);
                let up = matches!(kev.code, KeyCode::ArrowUp | KeyCode::ArrowLeft);
                if !down && !up {
                    return;
                }
                let cur = vals.iter().position(|v| *v == value.get_untracked());
                let next = match cur {
                    Some(i) if down => (i + 1) % n,
                    Some(i) => (i + n - 1) % n,
                    None if down => 0,
                    None => n - 1,
                };
                value.set(vals[next].clone());
            });
        }

        for (seg_val, content) in segments {
            let a11y = content_a11y_label(&content);

            // Content tint: AccentFg over the selected (Accent) fill, Text otherwise — reactive so it
            // follows the selection. Disabled is muted and static.
            let fg: Prop<ColorRole> = if disabled {
                ColorRole::TextMuted.into()
            } else {
                let sv = seg_val.clone();
                rx(move || {
                    if value.get() == sv {
                        ColorRole::AccentFg
                    } else {
                        ColorRole::Text
                    }
                })
            };

            let mut seg = segment_base(size, Role::Radio);
            if let Some(l) = &a11y {
                seg = seg.a11y_label(l.clone());
            }
            // a11y state is announced even when disabled.
            let sck = seg_val.clone();
            seg = seg.a11y_checked(rx(move || value.get() == sck));

            if disabled {
                let sel = value.get_untracked() == seg_val;
                seg = seg
                    .bg(if sel {
                        ColorRole::Accent
                    } else {
                        ColorRole::SurfaceRaised
                    })
                    .border_color(Some(ColorRole::Border));
            } else {
                let sbg = seg_val.clone();
                seg = seg.bg(rx(move || {
                    if value.get() == sbg {
                        ColorRole::Accent
                    } else {
                        ColorRole::SurfaceRaised
                    }
                }));
                if let Some(h) = &handle {
                    let hb = h.clone();
                    let sbd = seg_val.clone();
                    let hc = h.clone();
                    let sc = seg_val.clone();
                    // The Border keeps an unselected (white) segment visible; it becomes the FocusRing
                    // on the selected segment while the group is focus-visible.
                    seg = seg
                        .border_color(rx(move || {
                            Some(if hb.is_focus_visible() && value.get() == sbd {
                                ColorRole::FocusRing
                            } else {
                                ColorRole::Border
                            })
                        }))
                        .on_click(move |_ev, _cx| {
                            value.set(sc.clone());
                            hc.focus();
                        });
                }
            }

            seg = seg.child(render_content(content, fg, size));
            row = row.child(seg);
        }

        row.into_element()
    }
}

/// Toggles `seg` in/out of the `value` set (a controlled `insert`/`remove` on a cloned set).
fn toggle<T: Clone + Eq + Hash + Send + Sync + 'static>(value: RwSignal<HashSet<T>>, seg: &T) {
    let mut set = value.get_untracked();
    if !set.remove(seg) {
        set.insert(seg.clone());
    }
    value.set(set);
}

/// A multi-select toggle group bound to a `HashSet<T>` signal (#230). Build with [`toggle_group`]; add
/// segments with `.segment(value, content)`, then chain `.size(..)` / `.disabled(..)`. Each segment
/// toggles its `T` in/out of the bound set (click, or Space/Enter on the roving-focused segment).
pub struct ToggleGroup<T> {
    value: RwSignal<HashSet<T>>,
    segments: Vec<(T, SegmentContent)>,
    size: ControlSize,
    disabled: bool,
}

/// Creates a multi-select toggle group bound to `value` (controlled: each segment toggles its `T` in the
/// set).
pub fn toggle_group<T: Clone + Eq + Hash + Send + Sync + 'static>(
    value: RwSignal<HashSet<T>>,
) -> ToggleGroup<T> {
    ToggleGroup {
        value,
        segments: Vec::new(),
        size: ControlSize::Md,
        disabled: false,
    }
}

impl<T: Clone + Eq + Hash + Send + Sync + 'static> ToggleGroup<T> {
    /// Adds a segment: its `value` and its content (a label or an icon). Activating it toggles `value` in
    /// the bound set.
    pub fn segment(mut self, value: T, content: impl Into<SegmentContent>) -> Self {
        self.segments.push((value, content.into()));
        self
    }

    /// Sets the control size (D3).
    pub fn size(mut self, size: ControlSize) -> Self {
        self.size = size;
        self
    }

    /// Disables the control: muted styling, inert (no toggling), and not focusable.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

impl<T: Clone + Eq + Hash + Send + Sync + 'static> IntoElement for ToggleGroup<T> {
    fn into_element(self) -> AnyElement {
        let ToggleGroup {
            value,
            segments,
            size,
            disabled,
        } = self;

        let handle = (!disabled).then(use_focus_handle);
        // Roving focus: the arrows move this index; Space/Enter toggles the segment it points at.
        let focused = RwSignal::new(0_usize);
        let n = segments.len();

        let mut row = div().flex().gap_1().role(Role::Group);

        if let Some(h) = &handle {
            let vals: Vec<T> = segments.iter().map(|(v, _)| v.clone()).collect();
            row = row.track_focus(h).on_key_down(move |kev, _cx| {
                if kev.repeat || n == 0 {
                    return;
                }
                match kev.code {
                    KeyCode::ArrowRight | KeyCode::ArrowDown => {
                        focused.set((focused.get_untracked() + 1) % n);
                    }
                    KeyCode::ArrowLeft | KeyCode::ArrowUp => {
                        focused.set((focused.get_untracked() + n - 1) % n);
                    }
                    KeyCode::Space | KeyCode::Enter | KeyCode::NumpadEnter => {
                        if let Some(seg) = vals.get(focused.get_untracked()) {
                            toggle(value, seg);
                        }
                    }
                    _ => {}
                }
            });
        }

        for (i, (seg_val, content)) in segments.into_iter().enumerate() {
            let a11y = content_a11y_label(&content);

            let fg: Prop<ColorRole> = if disabled {
                ColorRole::TextMuted.into()
            } else {
                let sv = seg_val.clone();
                rx(move || {
                    if value.get().contains(&sv) {
                        ColorRole::AccentFg
                    } else {
                        ColorRole::Text
                    }
                })
            };

            let mut seg = segment_base(size, Role::CheckBox);
            if let Some(l) = &a11y {
                seg = seg.a11y_label(l.clone());
            }
            let sck = seg_val.clone();
            seg = seg.a11y_checked(rx(move || value.get().contains(&sck)));

            if disabled {
                let sel = value.get_untracked().contains(&seg_val);
                seg = seg
                    .bg(if sel {
                        ColorRole::Accent
                    } else {
                        ColorRole::SurfaceRaised
                    })
                    .border_color(Some(ColorRole::Border));
            } else {
                let sbg = seg_val.clone();
                seg = seg.bg(rx(move || {
                    if value.get().contains(&sbg) {
                        ColorRole::Accent
                    } else {
                        ColorRole::SurfaceRaised
                    }
                }));
                if let Some(h) = &handle {
                    let hb = h.clone();
                    let hc = h.clone();
                    let sc = seg_val.clone();
                    // The focus ring follows the roving index, not the selection (many may be selected).
                    seg = seg
                        .border_color(rx(move || {
                            Some(if hb.is_focus_visible() && focused.get() == i {
                                ColorRole::FocusRing
                            } else {
                                ColorRole::Border
                            })
                        }))
                        .on_click(move |_ev, _cx| {
                            toggle(value, &sc);
                            focused.set(i);
                            hc.focus();
                        });
                }
            }

            seg = seg.child(render_content(content, fg, size));
            row = row.child(seg);
        }

        row.into_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use kagari_base::{Point, Size as BSize};
    use kagari_core::arena::Arena;
    use kagari_core::damage::DamageState;
    use kagari_core::paint::render_tree;
    use kagari_core::reactive::Owner;
    use kagari_core::{
        DispatchState, FocusRegistry, HitTest, KeyEvent, Modifiers, MouseButton, MouseEvent,
        MouseKind, dispatch_key, dispatch_mouse,
    };
    use kagari_layout::LayoutTree;
    use kagari_render::{Background, Scene};
    use kagari_style::{ColorRole, Theme};
    use kagari_text::{FontDb, TextSystem};

    const VIEWPORT: BSize = BSize { w: 300.0, h: 200.0 };

    struct Built {
        root: AnyElement,
        arena: Arena,
        layout: LayoutTree,
        scene: Scene,
        hit: HitTest,
        root_id: kagari_base::NodeId,
    }

    fn build(widget: impl IntoElement, theme: &Theme) -> Built {
        let mut root = widget.into_element();
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let mut scene = Scene::new();
        let mut hit = HitTest::new();
        let damage = Arc::new(DamageState::default());
        let root_id = render_tree(
            &mut root,
            &mut arena,
            &mut layout,
            &mut text,
            None,
            Some(&mut hit),
            None,
            None,
            None,
            None,
            &mut scene,
            VIEWPORT,
            &damage,
            theme,
        )
        .unwrap();
        Built {
            root,
            arena,
            layout,
            scene,
            hit,
            root_id,
        }
    }

    fn press(b: &mut Built, code: KeyCode) {
        let kev = KeyEvent::new(code, Modifiers::default(), true, false);
        dispatch_key(&mut b.root, &b.arena, Some(b.root_id), &kev);
    }

    fn click_at(b: &mut Built, pos: Point) {
        let mut dispatch = DispatchState::default();
        for kind in [
            MouseKind::Down(MouseButton::Left),
            MouseKind::Up(MouseButton::Left),
        ] {
            let ev = MouseEvent::new(kind, pos, Modifiers::default());
            dispatch_mouse(&mut b.root, &b.arena, &b.hit, &ev, &mut dispatch);
        }
    }

    fn accent_count(b: &Built, theme: &Theme) -> usize {
        let accent = Background::Solid(theme.resolve_color(ColorRole::Accent));
        b.scene.quads.iter().filter(|q| q.bg == accent).count()
    }

    fn seg_3(value: RwSignal<i32>) -> Segmented<i32> {
        segmented(value)
            .segment(0, "A")
            .segment(1, "B")
            .segment(2, "C")
    }

    fn tgroup_3(value: RwSignal<HashSet<i32>>) -> ToggleGroup<i32> {
        toggle_group(value)
            .segment(0, "A")
            .segment(1, "B")
            .segment(2, "C")
    }

    #[test]
    fn segmented_should_select_exclusive() {
        // Exactly one segment is selected → exactly one Accent-filled box (exclusive by PartialEq).
        let owner = Owner::new();
        owner.set();
        let theme = Theme::light();
        let b = build(seg_3(RwSignal::new(1)), &theme);
        assert_eq!(
            accent_count(&b, &theme),
            1,
            "exactly one segment shows an Accent fill"
        );
        drop(owner);
    }

    #[test]
    fn segmented_arrow_should_move_selection() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(0);
        let mut b = build(seg_3(value), &Theme::light());
        press(&mut b, KeyCode::ArrowRight);
        assert_eq!(
            value.get_untracked(),
            1,
            "ArrowRight selects the next segment"
        );
        press(&mut b, KeyCode::ArrowRight);
        assert_eq!(value.get_untracked(), 2);
        press(&mut b, KeyCode::ArrowRight);
        assert_eq!(
            value.get_untracked(),
            0,
            "ArrowRight wraps from the last to the first"
        );
        press(&mut b, KeyCode::ArrowLeft);
        assert_eq!(
            value.get_untracked(),
            2,
            "ArrowLeft wraps from the first to the last"
        );
        drop(owner);
    }

    #[test]
    fn segmented_disabled_should_not_select() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(1);
        let mut b = build(seg_3(value).disabled(true), &Theme::light());
        press(&mut b, KeyCode::ArrowRight);
        let bounds = b.layout.layout(b.root_id);
        click_at(
            &mut b,
            Point::new(bounds.origin.x + 6.0, bounds.origin.y + 6.0),
        );
        assert_eq!(value.get_untracked(), 1, "a disabled control is inert");
        drop(owner);
    }

    #[test]
    fn segmented_click_should_select() {
        let owner = Owner::new();
        owner.set();
        let mut registry = FocusRegistry::new();
        kagari_core::reactive::provide_context(registry.clone());
        let value = RwSignal::new(2);
        let mut b = build(seg_3(value), &Theme::light());
        // The first segment sits at the left of the row; click within it.
        let bounds = b.layout.layout(b.root_id);
        click_at(
            &mut b,
            Point::new(bounds.origin.x + 6.0, bounds.origin.y + 6.0),
        );
        assert_eq!(
            value.get_untracked(),
            0,
            "clicking the first segment selects it"
        );
        let _ = &mut registry;
        drop(owner);
    }

    #[test]
    fn toggle_group_should_allow_multiple() {
        // Two members in the set → two Accent-filled boxes (multiple selection).
        let owner = Owner::new();
        owner.set();
        let theme = Theme::light();
        let set: HashSet<i32> = [0, 2].into_iter().collect();
        let b = build(tgroup_3(RwSignal::new(set)), &theme);
        assert_eq!(
            accent_count(&b, &theme),
            2,
            "two selected segments show two Accent fills"
        );
        drop(owner);
    }

    #[test]
    fn toggle_group_space_should_toggle_focused() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(HashSet::<i32>::new());
        let mut b = build(tgroup_3(value), &Theme::light());
        // Move the roving focus to the second segment, then Space toggles *it* (not the first).
        press(&mut b, KeyCode::ArrowRight);
        press(&mut b, KeyCode::Space);
        assert!(
            value.get_untracked().contains(&1),
            "Space toggles the focused (second) segment into the set"
        );
        assert!(
            !value.get_untracked().contains(&0),
            "the unfocused first segment is untouched"
        );
        // Space again toggles it back out.
        press(&mut b, KeyCode::Space);
        assert!(
            !value.get_untracked().contains(&1),
            "Space again toggles the focused segment out"
        );
        drop(owner);
    }
}
