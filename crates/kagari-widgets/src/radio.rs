//! The [`RadioGroup`] widget (#74): exclusive selection binding one `RwSignal<T>` — selecting an option
//! sets the signal to that option's `T` (exclusive by `PartialEq`). Composes the #245 state visuals; the
//! mark is an outer ring + inner dot (both drawn with a **border/fill role**, not a bare surface, per the
//! light-theme contrast rule).
//!
//! **Focus (Option B)**: the group is **one focus stop**; arrow keys move the *selection* (wrapping), and
//! the focus-ring follows the selected option (visual roving). Full WAI-ARIA roving tabindex is not
//! pursued — kagari's tab order is a static build-once list, and the pro-editor consumers are
//! mouse/shortcut-driven, not Tab-navigation/screen-reader driven.

use kagari_base::{SharedString, Size};
use kagari_core::reactive::prelude::*;
use kagari_core::reactive::{RwSignal, rx};
use kagari_core::{AnyElement, IntoElement, KeyCode, Role, div, text, use_focus_handle};
use kagari_style::{ColorRole, Styled};

use crate::control::{ControlSize, label_px, radio_dot_px, radio_mark_px};

/// An exclusive radio group bound to a `T` signal (#74). Build with [`radio_group`]; add options with
/// `.option(value, label)`, then chain `.size(..)` / `.disabled(..)`. Selecting an option (click, or an
/// arrow key while focused) sets the bound signal to that option's `T`.
pub struct RadioGroup<T> {
    value: RwSignal<T>,
    options: Vec<(T, SharedString)>,
    size: ControlSize,
    disabled: bool,
}

/// Creates a radio group bound to `value` (controlled: selecting an option sets `value` to its `T`).
pub fn radio_group<T: Clone + PartialEq + Send + Sync + 'static>(
    value: RwSignal<T>,
) -> RadioGroup<T> {
    RadioGroup {
        value,
        options: Vec::new(),
        size: ControlSize::Md,
        disabled: false,
    }
}

impl<T: Clone + PartialEq + Send + Sync + 'static> RadioGroup<T> {
    /// Adds an option: its bound `value` and its label. Selecting it sets the group signal to `value`.
    pub fn option(mut self, value: T, label: impl Into<SharedString>) -> Self {
        self.options.push((value, label.into()));
        self
    }

    /// Sets the control size (D3).
    pub fn size(mut self, size: ControlSize) -> Self {
        self.size = size;
        self
    }

    /// Disables the group: muted styling, inert (no selection), and not focusable.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

impl<T: Clone + PartialEq + Send + Sync + 'static> IntoElement for RadioGroup<T> {
    fn into_element(self) -> AnyElement {
        let RadioGroup {
            value,
            options,
            size,
            disabled,
        } = self;
        let mark_px = radio_mark_px(size).0;
        let dot_px = radio_dot_px(size).0;

        // One focus handle for the whole group (Option B) — enabled only.
        let handle = (!disabled).then(use_focus_handle);

        // `items_start` so each option row hugs its content (mark + label) instead of stretching to the
        // group's full width — otherwise the focus-ring border would frame the whole row width.
        let mut group = div().flex_col().gap_2().items_start().role(Role::Group);

        // Arrow keys move the selection within the group (wrapping); the group is the focused node.
        if let Some(h) = &handle {
            let opt_vals: Vec<T> = options.iter().map(|(v, _)| v.clone()).collect();
            let n = opt_vals.len();
            group = group.track_focus(h).on_key_down(move |kev, _cx| {
                if kev.repeat || n == 0 {
                    return;
                }
                let down = matches!(kev.code, KeyCode::ArrowDown | KeyCode::ArrowRight);
                let up = matches!(kev.code, KeyCode::ArrowUp | KeyCode::ArrowLeft);
                if !down && !up {
                    return;
                }
                let cur = opt_vals.iter().position(|v| *v == value.get_untracked());
                let next = match cur {
                    Some(i) if down => (i + 1) % n,
                    Some(i) => (i + n - 1) % n,
                    None if down => 0,
                    None => n - 1,
                };
                value.set(opt_vals[next].clone());
            });
        }

        for (opt_val, label) in options {
            // Mark: an outer ring (visible border) + an inner dot tinted to show only when selected —
            // reactive props (not `dyn_if`), and a border so the ring is visible even with a white fill.
            let base = div()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .border_w_2()
                .bg(ColorRole::Surface)
                .size(Size {
                    w: mark_px,
                    h: mark_px,
                });
            let dot = div().rounded_full().size(Size {
                w: dot_px,
                h: dot_px,
            });
            let mark = if disabled {
                let sel = value.get_untracked() == opt_val;
                base.border_color(Some(ColorRole::Border))
                    .child(dot.bg(if sel {
                        ColorRole::TextMuted
                    } else {
                        ColorRole::Surface
                    }))
            } else {
                let ovb = opt_val.clone();
                let ovd = opt_val.clone();
                base.border_color(rx(move || {
                    Some(if value.get() == ovb {
                        ColorRole::Accent
                    } else {
                        ColorRole::Border
                    })
                }))
                .child(dot.bg(rx(move || {
                    if value.get() == ovd {
                        ColorRole::Accent
                    } else {
                        ColorRole::Surface
                    }
                })))
            };

            // Option row: the focus-ring follows the selected option while the group is focused; a click
            // selects this option and focuses the group (so subsequent arrows work).
            let mut row = div().flex().items_center().gap_2().rounded_md();
            if let Some(h) = &handle {
                let hring = h.clone();
                let ovr = opt_val.clone();
                let hclick = h.clone();
                let ovc = opt_val.clone();
                row = row
                    .border_w_2()
                    .border_color(rx(move || {
                        (hring.is_focus_visible() && value.get() == ovr)
                            .then_some(ColorRole::FocusRing)
                    }))
                    .on_click(move |_ev, _cx| {
                        value.set(ovc.clone());
                        hclick.focus();
                    });
            }
            row = row.child(mark).child(
                text(label)
                    .color_role(if disabled {
                        ColorRole::TextMuted
                    } else {
                        ColorRole::Text
                    })
                    .size(label_px(size)),
            );
            group = group.child(row);
        }

        group.into_element()
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
    use kagari_core::reactive::{Owner, provide_context};
    use kagari_core::{
        DispatchState, FocusRegistry, HitTest, KeyEvent, Modifiers, MouseButton, MouseEvent,
        MouseKind, dispatch_key, dispatch_mouse,
    };
    use kagari_layout::LayoutTree;
    use kagari_render::{Background, Scene};
    use kagari_style::{ColorRole, Theme};
    use kagari_text::{FontDb, TextSystem};

    const VIEWPORT: BSize = BSize { w: 200.0, h: 200.0 };

    struct Built {
        root: AnyElement,
        arena: Arena,
        layout: LayoutTree,
        scene: Scene,
        hit: HitTest,
        root_id: kagari_base::NodeId,
    }

    fn build(
        widget: impl IntoElement,
        theme: &Theme,
        registry: Option<&mut FocusRegistry>,
    ) -> Built {
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
            registry,
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

    fn group_3(value: RwSignal<i32>) -> RadioGroup<i32> {
        radio_group(value)
            .option(0, "A")
            .option(1, "B")
            .option(2, "C")
    }

    #[test]
    fn radio_arrow_should_move_selection() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(0);
        let mut b = build(group_3(value), &Theme::light(), None);
        press(&mut b, KeyCode::ArrowDown);
        assert_eq!(
            value.get_untracked(),
            1,
            "ArrowDown selects the next option"
        );
        press(&mut b, KeyCode::ArrowDown);
        assert_eq!(value.get_untracked(), 2);
        press(&mut b, KeyCode::ArrowDown);
        assert_eq!(
            value.get_untracked(),
            0,
            "ArrowDown wraps from the last to the first"
        );
        press(&mut b, KeyCode::ArrowUp);
        assert_eq!(
            value.get_untracked(),
            2,
            "ArrowUp wraps from the first to the last"
        );
        drop(owner);
    }

    #[test]
    fn radio_selected_option_should_paint_one_accent_dot() {
        // Exactly one option is selected → exactly one Accent-filled dot (exclusive by PartialEq).
        let owner = Owner::new();
        owner.set();
        let theme = Theme::light();
        let b = build(group_3(RwSignal::new(1)), &theme, None);
        let accent = Background::Solid(theme.resolve_color(ColorRole::Accent));
        let dots = b.scene.quads.iter().filter(|q| q.bg == accent).count();
        assert_eq!(dots, 1, "exactly one option shows an Accent dot");
        drop(owner);
    }

    #[test]
    fn radio_dot_should_be_centered_in_the_mark() {
        // The inner dot's center must coincide with the outer circle's center (no half-pixel snap):
        // dot and outer diameters share parity, so (outer − dot)/2 is a whole number.
        let owner = Owner::new();
        owner.set();
        let theme = Theme::light();
        let b = build(group_3(RwSignal::new(1)), &theme, None);
        let accent = Background::Solid(theme.resolve_color(ColorRole::Accent));
        let mark = crate::control::radio_mark_px(ControlSize::Md).0;
        // The selected dot is the unique Accent quad.
        let dot = b
            .scene
            .quads
            .iter()
            .find(|q| q.bg == accent)
            .expect("selected dot");
        // Its outer circle is the mark-sized quad that vertically contains the dot.
        let dot_cy = dot.bounds.origin.y + dot.bounds.size.h / 2.0;
        let circle = b
            .scene
            .quads
            .iter()
            .find(|q| {
                (q.bounds.size.w - mark).abs() < 0.01
                    && q.bounds.origin.y <= dot_cy
                    && dot_cy <= q.bounds.origin.y + q.bounds.size.h
            })
            .expect("outer circle");
        let dot_cx = dot.bounds.origin.x + dot.bounds.size.w / 2.0;
        let circle_cx = circle.bounds.origin.x + circle.bounds.size.w / 2.0;
        let circle_cy = circle.bounds.origin.y + circle.bounds.size.h / 2.0;
        assert!(
            (dot_cx - circle_cx).abs() < 0.01,
            "dot cx {dot_cx} vs circle cx {circle_cx}"
        );
        assert!(
            (dot_cy - circle_cy).abs() < 0.01,
            "dot cy {dot_cy} vs circle cy {circle_cy}"
        );
        drop(owner);
    }

    #[test]
    fn radio_click_should_select_option() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(2);
        let mut b = build(group_3(value), &Theme::light(), None);
        // The first option row sits at the top of the group; click within its mark.
        let bounds = b.layout.layout(b.root_id);
        let pos = Point::new(bounds.origin.x + 6.0, bounds.origin.y + 6.0);
        click_at(&mut b, pos);
        assert_eq!(
            value.get_untracked(),
            0,
            "clicking the first option selects it"
        );
        drop(owner);
    }

    #[test]
    fn radio_disabled_should_not_select() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(1);
        let mut b = build(group_3(value).disabled(true), &Theme::light(), None);
        press(&mut b, KeyCode::ArrowDown);
        let bounds = b.layout.layout(b.root_id);
        click_at(
            &mut b,
            Point::new(bounds.origin.x + 6.0, bounds.origin.y + 6.0),
        );
        assert_eq!(value.get_untracked(), 1, "a disabled group is inert");
        drop(owner);
    }

    #[test]
    fn radio_focused_should_paint_focus_ring_on_selection() {
        // Focusing the group paints a FocusRing on the selected option's row.
        let owner = Owner::new();
        owner.set();
        let mut registry = FocusRegistry::new();
        provide_context(registry.clone());
        let theme = Theme::light();
        let ring = theme.resolve_color(ColorRole::FocusRing);

        let mut root = group_3(RwSignal::new(1)).into_element();
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let damage = Arc::new(DamageState::default());
        let render = |root: &mut AnyElement,
                      arena: &mut Arena,
                      layout: &mut LayoutTree,
                      text: &mut TextSystem,
                      registry: &mut FocusRegistry| {
            let mut scene = Scene::new();
            render_tree(
                root,
                arena,
                layout,
                text,
                None,
                None,
                None,
                Some(registry),
                None,
                None,
                &mut scene,
                VIEWPORT,
                &damage,
                &theme,
            )
            .unwrap();
            scene
        };
        let has_ring = |s: &Scene| {
            s.quads
                .iter()
                .any(|q| q.border.color == ring && q.border.widths.top > 0.0)
        };

        let scene = render(&mut root, &mut arena, &mut layout, &mut text, &mut registry);
        assert!(!has_ring(&scene), "an unfocused group paints no focus ring");

        // Focused via a non-keyboard modality (e.g. a click) → the ring stays hidden (`:focus-visible`).
        registry.focus_next();
        let scene = render(&mut root, &mut arena, &mut layout, &mut text, &mut registry);
        assert!(
            !has_ring(&scene),
            "a focused-but-not-focus-visible group paints no ring"
        );

        // A key press marks keyboard modality → the ring appears on the selected option.
        registry.set_focus_visible(true);
        let scene = render(&mut root, &mut arena, &mut layout, &mut text, &mut registry);
        let ring = scene
            .quads
            .iter()
            .find(|q| q.border.color == ring && q.border.widths.top > 0.0)
            .expect("a focus-visible group rings its selected option");
        // The ring hugs the option row (mark + label), not the group's full width (`items_start`).
        assert!(
            ring.bounds.size.w < VIEWPORT.w * 0.75,
            "focus ring width {} should hug content, not span the viewport {}",
            ring.bounds.size.w,
            VIEWPORT.w
        );
        drop(owner);
    }
}
