//! The [`Switch`] widget (#73): a `bool` bound to an `RwSignal<bool>` (controlled), toggled by a click
//! or Space/Enter — the same model as [`Checkbox`](crate::Checkbox), drawn as a track + sliding knob.
//! Shares the size/disabled/label builders and the focusable root via `control` (`control_builders!` /
//! `control_root`).
//!
//! **Animated knob (#253)**: the knob *slides* via an `Animated<f32>` spacer width, retargeted on toggle
//! and driven by the app's per-frame ticker. Without an App context (a GPU-free test) it reaches the
//! target on the next manual `Ticker::tick_all` (like `scroll`).

use kagari_base::{SharedString, Size};
use kagari_core::reactive::prelude::*;
use kagari_core::reactive::{RwSignal, create_effect, rx};
use kagari_core::{Animated, AnyElement, IntoElement, Role, div};
use kagari_style::{ColorRole, Styled};

use crate::control::{CONTROL_SPRING, ControlSize, control_builders, control_root, switch_dims};

/// A switch bound to a `bool` signal (#73) — the same controlled model as [`Checkbox`](crate::Checkbox),
/// drawn as a track + sliding knob. Build with [`switch`]; chain `.size(..)`, `.disabled(..)`,
/// `.label(..)`. Returns `impl IntoElement`.
///
/// **`Switch` vs [`Button::toggle`](crate::Button::toggle)** — both bind an on/off `bool`, but they are
/// different controls: a `Switch` is the settings-style **track + sliding knob** (a distinct `switch`
/// role in a11y), whereas `Button::toggle` is a **button that stays pressed** when active (e.g. a
/// toolbar Bold/Italic button). Pick `Switch` for an on/off setting, the toggle button for a
/// button-shaped two-state action.
pub struct Switch {
    value: RwSignal<bool>,
    size: ControlSize,
    disabled: bool,
    label: Option<SharedString>,
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

control_builders!(Switch);

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

        // A reactive-width spacer before the knob positions it left (off) / right (on). The knob is a
        // light circle on both track colors.
        let spacer = if disabled {
            let w = if value.get_untracked() { on_w } else { off_w };
            div().size(Size { w, h: track_h.0 })
        } else {
            // The knob **slides** on toggle (#253): an `Animated<f32>` spacer width, retargeted by an
            // effect on the bound signal. Live once the app drives the ticker; a GPU-free test without an
            // App context reaches the target on the next manual tick (like `scroll`). The guard avoids a
            // spurious start when the effect first runs at the already-current value.
            let anim = Animated::new(
                if value.get_untracked() { on_w } else { off_w },
                CONTROL_SPRING,
            );
            let retarget = anim.clone();
            create_effect(move || {
                let target = if value.get() { on_w } else { off_w };
                if retarget.target() != target {
                    retarget.set_target(target);
                }
            });
            div().size(rx(move || Size {
                w: anim.get(),
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

        control_root(value, size, disabled, label, Role::Switch, track).into_element()
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
        DispatchState, HitTest, Modifiers, MouseButton, MouseEvent, MouseKind, dispatch_mouse,
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

    #[test]
    fn switch_knob_should_slide_toward_target_on_toggle() {
        // End-to-end (#253): with a `Ticker` in context, toggling on retargets the knob's `Animated`;
        // driving the ticker one frame slides the knob right (vs its off position) — no manual anim
        // access. Mirrors how the app's redraw drives it.
        use kagari_core::Ticker;
        use kagari_core::reactive::provide_context;
        use std::time::Duration;

        let owner = Owner::new();
        owner.set();
        let ticker = Ticker::new();
        provide_context(ticker.clone());
        let value = RwSignal::new(false);
        let theme = Theme::light();
        let knob_bg = Background::Solid(theme.resolve_color(ColorRole::AccentFg));

        let mut root = switch(value).into_element();
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let damage = Arc::new(DamageState::default());
        let knob_x = |root: &mut AnyElement,
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
                .quads
                .iter()
                .find(|q| q.bg == knob_bg)
                .expect("the knob paints an AccentFg quad")
                .bounds
                .origin
                .x
        };

        let x_off = knob_x(&mut root, &mut arena, &mut layout, &mut text);
        value.set(true); // the retarget effect fires set_target(on) → registers into the ticker
        ticker.tick_all(Duration::from_millis(16)); // one frame toward the on position
        let x_mid = knob_x(&mut root, &mut arena, &mut layout, &mut text);
        assert!(
            x_mid > x_off,
            "one tick after toggling on, the knob has slid right: off={x_off} mid={x_mid}"
        );
        drop(owner);
    }
}
