//! The [`Switch`] widget (#73): a `bool` bound to an `RwSignal<bool>` (controlled), toggled by a click
//! or Space/Enter — the same model as [`Checkbox`](crate::Checkbox), drawn as a track + sliding knob.
//! Shares the size/disabled/label builders and the focusable root via `control` (`control_builders!` /
//! `control_root`).
//!
//! **Instant, not yet animated**: the app does not drive `Animated::tick` live yet (the deferred frame
//! loop that `scroll` also awaits), so the knob switches *instantly* — a live-correct MVP. The animated
//! transition is relocated to a follow-up (it would leave the value visually stuck otherwise).

use kagari_base::{SharedString, Size};
use kagari_core::reactive::prelude::*;
use kagari_core::reactive::{RwSignal, rx};
use kagari_core::{AnyElement, IntoElement, div};
use kagari_style::{ColorRole, Styled};

use crate::control::{ControlSize, control_builders, control_root, switch_dims};

/// A switch (toggle) bound to a `bool` signal (#73) — the same controlled model as
/// [`Checkbox`](crate::Checkbox), drawn as a track + sliding knob. Build with [`switch`]; chain
/// `.size(..)`, `.disabled(..)`, `.label(..)`. Returns `impl IntoElement`.
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
}
