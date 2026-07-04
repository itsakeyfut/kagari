//! The [`Checkbox`] and [`Switch`] widgets (#73): a `bool` bound to an `RwSignal<bool>` (controlled),
//! toggled by a click or Space/Enter. They compose the #245 state visuals (reactive role colors +
//! focus-ring border) and, for the checkbox, the #246 [`icon`] checkmark.
//!
//! **Instant, not yet animated**: the app does not drive `Animated::tick` live yet (the deferred frame
//! loop that `scroll` also awaits), so the check/knob switch state *instantly* — a live-correct MVP. The
//! animated transition is relocated to a follow-up (it would leave the value visually stuck otherwise).

use kagari_base::{SharedString, Size};
use kagari_core::element::Div;
use kagari_core::reactive::prelude::*;
use kagari_core::reactive::{RwSignal, rx};
use kagari_core::{
    AnyElement, IconId, IntoElement, KeyCode, Role, div, icon, text, use_focus_handle,
};
use kagari_style::{ColorRole, Styled};

use crate::control::{ControlSize, check_glyph_px, checkbox_box_px, label_px, switch_dims};

/// A checkbox bound to a `bool` signal (#73). Build with [`checkbox`]; chain `.size(..)`,
/// `.disabled(..)`, `.label(..)`. Returns `impl IntoElement`.
pub struct Checkbox {
    value: RwSignal<bool>,
    size: ControlSize,
    disabled: bool,
    label: Option<SharedString>,
}

/// A switch (toggle) bound to a `bool` signal (#73) — the same controlled model as [`Checkbox`], drawn
/// as a track + sliding knob. Build with [`switch`].
pub struct Switch {
    value: RwSignal<bool>,
    size: ControlSize,
    disabled: bool,
    label: Option<SharedString>,
}

/// Creates a medium checkbox bound to `value` (controlled: activation flips `value`).
pub fn checkbox(value: RwSignal<bool>) -> Checkbox {
    Checkbox {
        value,
        size: ControlSize::Md,
        disabled: false,
        label: None,
    }
}

/// Creates a medium switch bound to `value` (controlled: activation flips `value`).
pub fn switch(value: RwSignal<bool>) -> Switch {
    Switch {
        value,
        size: ControlSize::Md,
        disabled: false,
        label: None,
    }
}

macro_rules! control_builders {
    ($t:ty) => {
        impl $t {
            /// Sets the control size (D3).
            pub fn size(mut self, size: ControlSize) -> Self {
                self.size = size;
                self
            }

            /// Disables the control: muted styling, inert (no toggle), and not focusable.
            pub fn disabled(mut self, disabled: bool) -> Self {
                self.disabled = disabled;
                self
            }

            /// Adds a trailing text label (clickable together with the control).
            pub fn label(mut self, label: impl Into<SharedString>) -> Self {
                self.label = Some(label.into());
                self
            }
        }
    };
}
control_builders!(Checkbox);
control_builders!(Switch);

/// Wraps a control's `indicator` (checkbox box / switch track) with the shared root: a focusable,
/// clickable row with an optional label. The focus ring (a reactive `FocusRing` border) and
/// focusability are attached only when enabled (RK-027); activation (click **or** Space/Enter) flips
/// `value`. Mirrors [`Button`](crate::Button).
fn control_root(
    value: RwSignal<bool>,
    size: ControlSize,
    disabled: bool,
    label: Option<SharedString>,
    indicator: Div,
) -> Div {
    let mut root = div().flex().items_center().gap_2().role(Role::CheckBox);
    if let Some(l) = &label {
        root = root.a11y_label(l.clone());
    }
    if !disabled {
        let handle = use_focus_handle();
        let h = handle.clone();
        root = root
            .rounded_md()
            .border_w_2()
            .border_color(rx(move || {
                h.is_focus_visible().then_some(ColorRole::FocusRing)
            }))
            .track_focus(&handle)
            .on_click(move |_ev, _cx| value.set(!value.get_untracked()))
            .on_key_down(move |kev, _cx| {
                if !kev.repeat
                    && matches!(
                        kev.code,
                        KeyCode::Space | KeyCode::Enter | KeyCode::NumpadEnter
                    )
                {
                    value.set(!value.get_untracked());
                }
            });
    }
    root = root.child(indicator);
    if let Some(l) = label {
        let color = if disabled {
            ColorRole::TextMuted
        } else {
            ColorRole::Text
        };
        root = root.child(text(l).color_role(color).size(label_px(size)));
    }
    root
}

impl IntoElement for Checkbox {
    fn into_element(self) -> AnyElement {
        let Checkbox {
            value,
            size,
            disabled,
            label,
        } = self;
        let box_px = checkbox_box_px(size).0;
        let glyph_px = check_glyph_px(size);

        let mut boxed = div()
            .flex()
            .items_center()
            .justify_center()
            .rounded_sm()
            .border_w_2()
            .size(Size {
                w: box_px,
                h: box_px,
            });
        // The check is always laid out (centered in the box); it is tinted **reactively** so it shows the
        // white ✓ when checked and blends into the box (its own bg color) when not. A reactive tint
        // updates live via the paint effect (#245/#246), unlike a structural `dyn_if` which needs the
        // deferred reconcile pass (#36).
        if disabled {
            let tint = if value.get_untracked() {
                ColorRole::TextMuted
            } else {
                ColorRole::SurfaceRaised
            };
            boxed = boxed
                .bg(ColorRole::SurfaceRaised)
                .border_color(Some(ColorRole::Border))
                .child(icon(IconId::Check).color_role(tint).size(glyph_px));
        } else {
            boxed = boxed
                .bg(rx(move || {
                    if value.get() {
                        ColorRole::Accent
                    } else {
                        ColorRole::Surface
                    }
                }))
                .border_color(rx(move || {
                    Some(if value.get() {
                        ColorRole::Accent
                    } else {
                        ColorRole::Border
                    })
                }))
                .child(
                    icon(IconId::Check)
                        .color_role(rx(move || {
                            if value.get() {
                                ColorRole::AccentFg
                            } else {
                                ColorRole::Surface
                            }
                        }))
                        .size(glyph_px),
                );
        }

        control_root(value, size, disabled, label, boxed).into_element()
    }
}

impl IntoElement for Switch {
    fn into_element(self) -> AnyElement {
        let Switch {
            value,
            size,
            disabled,
            label,
        } = self;
        let (track_w, track_h, knob_px) = switch_dims(size);
        // The knob is inset 2px on the left (off) and slides right by `on_w - off_w` when on.
        let off_w = 2.0;
        let on_w = track_w.0 - knob_px.0 - 2.0;

        // A reactive-width spacer before the knob positions it left (off) / right (on) — instant (a
        // reactive `.size` relayouts on toggle). The knob is a light circle on both track colors.
        let spacer = if disabled {
            let w = if value.get_untracked() { on_w } else { off_w };
            div().size(Size { w, h: track_h.0 })
        } else {
            div().size(rx(move || Size {
                w: if value.get() { on_w } else { off_w },
                h: track_h.0,
            }))
        };
        let knob_el = div()
            .rounded_full()
            // A drop shadow delineates the light knob against both the (light) off track and the
            // (blue) on track — the same trick real switches use.
            .shadow_sm()
            .size(Size {
                w: knob_px.0,
                h: knob_px.0,
            })
            .bg(if disabled {
                ColorRole::Surface
            } else {
                ColorRole::AccentFg
            });

        let mut track = div().flex().items_center().rounded_full().size(Size {
            w: track_w.0,
            h: track_h.0,
        });
        // The off/disabled track uses a gray border role, not `SurfaceRaised` — in the light theme
        // `SurfaceRaised` resolves to white (same as the page `Surface`), which made the off switch
        // invisible. `Accent` (blue) when on.
        track = if disabled {
            track.bg(ColorRole::Border)
        } else {
            track.bg(rx(move || {
                if value.get() {
                    ColorRole::Accent
                } else {
                    ColorRole::BorderStrong
                }
            }))
        };
        track = track.child(spacer).child(knob_el);

        control_root(value, size, disabled, label, track).into_element()
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
        DispatchState, FocusRegistry, HitTest, KeyCode, KeyEvent, Modifiers, MouseButton,
        MouseEvent, MouseKind, dispatch_key, dispatch_mouse,
    };
    use kagari_layout::LayoutTree;
    use kagari_render::{Background, Scene};
    use kagari_style::{ColorRole, Theme};
    use kagari_text::{FontDb, TextSystem};

    const VIEWPORT: BSize = BSize { w: 200.0, h: 100.0 };

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

    fn click(b: &mut Built) {
        let bounds = b.layout.layout(b.root_id);
        let pos = Point::new(
            bounds.origin.x + bounds.size.w / 2.0,
            bounds.origin.y + bounds.size.h / 2.0,
        );
        let mut dispatch = DispatchState::default();
        for kind in [
            MouseKind::Down(MouseButton::Left),
            MouseKind::Up(MouseButton::Left),
        ] {
            let ev = MouseEvent::new(kind, pos, Modifiers::default());
            dispatch_mouse(&mut b.root, &b.arena, &b.hit, &ev, &mut dispatch);
        }
    }

    fn press_key(b: &mut Built, code: KeyCode) {
        let kev = KeyEvent::new(code, Modifiers::default(), true, false);
        dispatch_key(&mut b.root, &b.arena, Some(b.root_id), &kev);
    }

    #[test]
    fn checkbox_should_toggle_signal_on_click() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(false);
        let mut b = build(checkbox(value), &Theme::light());
        click(&mut b);
        assert!(value.get_untracked(), "a click checks the box");
        click(&mut b);
        assert!(!value.get_untracked(), "a second click unchecks it");
        drop(owner);
    }

    #[test]
    fn checkbox_should_toggle_on_space() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(false);
        let mut b = build(checkbox(value), &Theme::light());
        press_key(&mut b, KeyCode::Space);
        assert!(value.get_untracked(), "Space checks the box");
        drop(owner);
    }

    #[test]
    fn checkbox_check_tint_should_reflect_state() {
        let owner = Owner::new();
        owner.set();
        let theme = Theme::light();
        // The check is always laid out; its tint shows it (AccentFg) when checked, hides it (the box's
        // own Surface color) when not.
        let on = build(checkbox(RwSignal::new(true)), &theme);
        assert_eq!(on.scene.icons.len(), 1);
        assert_eq!(on.scene.icons[0].icon, IconId::Check);
        assert_eq!(
            on.scene.icons[0].tint,
            theme.resolve_color(ColorRole::AccentFg),
            "a checked box shows a white check"
        );
        let off = build(checkbox(RwSignal::new(false)), &theme);
        assert_eq!(
            off.scene.icons[0].tint,
            theme.resolve_color(ColorRole::Surface),
            "an unchecked box's check blends into the box"
        );
        drop(owner);
    }

    #[test]
    fn checkbox_disabled_should_not_toggle() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(false);
        let mut b = build(checkbox(value).disabled(true), &Theme::light());
        click(&mut b);
        press_key(&mut b, KeyCode::Space);
        assert!(!value.get_untracked(), "a disabled checkbox is inert");
        drop(owner);
    }

    #[test]
    fn switch_should_toggle_signal_on_click() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(false);
        let mut b = build(switch(value), &Theme::light());
        click(&mut b);
        assert!(value.get_untracked(), "a click turns the switch on");
        drop(owner);
    }

    #[test]
    fn switch_on_should_paint_accent_track() {
        let owner = Owner::new();
        owner.set();
        let theme = Theme::light();
        let on = build(switch(RwSignal::new(true)), &theme);
        assert!(
            on.scene
                .quads
                .iter()
                .any(|q| q.bg == Background::Solid(theme.resolve_color(ColorRole::Accent))),
            "an on switch paints an Accent track"
        );
        drop(owner);
    }

    #[test]
    fn switch_off_should_paint_a_visible_track() {
        // Regression: the off track must be a gray border role, not `SurfaceRaised` — in the light
        // theme `SurfaceRaised` resolves to white (== the page `Surface`), which made the off switch
        // invisible (white track + white knob on a white page).
        let owner = Owner::new();
        owner.set();
        let theme = Theme::light();
        let off = build(switch(RwSignal::new(false)), &theme);
        assert!(
            off.scene
                .quads
                .iter()
                .any(|q| q.bg == Background::Solid(theme.resolve_color(ColorRole::BorderStrong))),
            "an off switch paints a visible (BorderStrong) track"
        );
        assert_ne!(
            theme.resolve_color(ColorRole::BorderStrong),
            theme.resolve_color(ColorRole::Surface),
            "the off track color is distinct from the page Surface"
        );
        drop(owner);
    }

    #[test]
    fn checkbox_external_write_should_update_check() {
        // A write to the bound signal between frames updates the visual (the check appears).
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(false);
        let mut root = checkbox(value).into_element();
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let theme = Theme::light();
        let damage = Arc::new(DamageState::default());
        let render = |root: &mut AnyElement,
                      arena: &mut Arena,
                      layout: &mut LayoutTree,
                      text: &mut TextSystem| {
            let mut scene = Scene::new();
            render_tree(
                root, arena, layout, text, None, None, None, None, None, None, &mut scene,
                VIEWPORT, &damage, &theme,
            )
            .unwrap();
            scene
        };
        let scene = render(&mut root, &mut arena, &mut layout, &mut text);
        assert_eq!(
            scene.icons[0].tint,
            theme.resolve_color(ColorRole::Surface),
            "frame 1 (false): the check blends into the box"
        );
        value.set(true);
        let scene = render(&mut root, &mut arena, &mut layout, &mut text);
        assert_eq!(
            scene.icons[0].tint,
            theme.resolve_color(ColorRole::AccentFg),
            "an external write to true reveals the white check"
        );
        drop(owner);
    }

    #[test]
    fn checkbox_focused_should_paint_focus_ring() {
        // Focusing the checkbox (via a context FocusRegistry) paints a FocusRing border on its root.
        let owner = Owner::new();
        owner.set();
        let mut registry = FocusRegistry::new();
        provide_context(registry.clone());
        let mut root = checkbox(RwSignal::new(false)).into_element();

        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let theme = Theme::light();
        let damage = Arc::new(DamageState::default());
        let ring = theme.resolve_color(ColorRole::FocusRing);
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
        assert!(
            !has_ring(&scene),
            "an unfocused checkbox paints no focus ring"
        );
        // Focused via a non-keyboard modality → the ring stays hidden (`:focus-visible`).
        registry.focus_next();
        let scene = render(&mut root, &mut arena, &mut layout, &mut text, &mut registry);
        assert!(
            !has_ring(&scene),
            "a focused-but-not-focus-visible checkbox paints no ring"
        );
        // A key press marks keyboard modality → the ring appears.
        registry.set_focus_visible(true);
        let scene = render(&mut root, &mut arena, &mut layout, &mut text, &mut registry);
        assert!(
            has_ring(&scene),
            "a focus-visible checkbox paints a FocusRing border"
        );
        drop(owner);
    }

    #[test]
    fn switch_knob_should_shift_when_on() {
        // The knob (the AccentFg circle) sits further right when the switch is on (the reactive spacer
        // widens). Compare the knob quad's x between a fresh off and on switch.
        let owner = Owner::new();
        owner.set();
        let theme = Theme::light();
        let knob_bg = Background::Solid(theme.resolve_color(ColorRole::AccentFg));
        let knob_x = |on: bool| {
            let b = build(switch(RwSignal::new(on)), &theme);
            b.scene
                .quads
                .iter()
                .find(|q| q.bg == knob_bg)
                .expect("the knob paints an AccentFg quad")
                .bounds
                .origin
                .x
        };
        assert!(
            knob_x(true) > knob_x(false),
            "the on knob sits right of the off knob"
        );
        drop(owner);
    }
}
