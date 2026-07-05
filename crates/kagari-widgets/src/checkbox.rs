//! The [`Checkbox`] widget (#73): a `bool` bound to an `RwSignal<bool>` (controlled), toggled by a click
//! or Space/Enter. Composes the #245 state visuals (reactive role colors + focus-ring border) and the
//! #246 [`icon`] checkmark. Shares the size/disabled/label builders and the focusable root with the
//! sibling [`Switch`](crate::Switch) via `control` (`control_builders!` / `control_root`).
//!
//! **Animated check (#266)**: the check *pops in* via an `Animated<f32>` **scale** (0 → 1) applied by a
//! center-origin paint-time [`transform`] — sub-pixel smooth (no relayout, not
//! quantized by taffy rounding), retargeted on toggle and driven by the app's per-frame ticker (#253).
//! Without an App context (a GPU-free test) it reaches the target on the next manual `Ticker::tick_all`
//! (like `scroll`). The box bg/border fill instantly.

use kagari_base::{SharedString, Size};
use kagari_core::reactive::prelude::*;
use kagari_core::reactive::{RwSignal, create_effect, rx};
use kagari_core::{
    Animated, AnyElement, IconId, IntoElement, Role, TransformOrigin, div, icon, transform,
};
use kagari_style::{ColorRole, Styled};

use crate::control::{
    CONTROL_SPRING, ControlSize, check_glyph_px, checkbox_box_px, control_builders, control_root,
};

/// A checkbox bound to a `bool` signal (#73). Build with [`checkbox`]; chain `.size(..)`,
/// `.disabled(..)`, `.label(..)`. Returns `impl IntoElement`.
pub struct Checkbox {
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

control_builders!(Checkbox);

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
            // The check **pops in** on toggle (#266): the icon keeps a constant layout size and is scaled
            // about its center by an `Animated<f32>` (0 → 1) via a paint-time `transform` — sub-pixel smooth
            // (no relayout, not quantized by taffy rounding, unlike animating the layout size in #264). The
            // tint stays a constant AccentFg (a ~0-scale check is invisible), so no reactive tint blend is
            // needed. The box bg/border fill instantly. The guard avoids a spurious start when the effect
            // first runs at the already-current value.
            let anim = Animated::new(
                if value.get_untracked() { 1.0 } else { 0.0 },
                CONTROL_SPRING,
            );
            let retarget = anim.clone();
            create_effect(move || {
                let target = if value.get() { 1.0 } else { 0.0 };
                if retarget.target() != target {
                    retarget.set_target(target);
                }
            });
            let check = transform(
                icon(IconId::Check)
                    .color_role(ColorRole::AccentFg)
                    .size(glyph_px),
            )
            .scale(rx(move || anim.get()))
            .origin(TransformOrigin::CENTER);
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
                .child(check);
        }

        control_root(value, size, disabled, label, Role::CheckBox, boxed).into_element()
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
    fn checkbox_check_size_should_reflect_state() {
        let owner = Owner::new();
        owner.set();
        let theme = Theme::light();
        let glyph = check_glyph_px(ControlSize::Md).0;
        // The check is always laid out with a constant AccentFg tint; its *painted* size shows state via a
        // center-origin scale (#266) — full (scale 1) when checked, ~0 (scale MIN_SCALE) when not. A checkbox
        // built checked shows the full check at once.
        let on = build(checkbox(RwSignal::new(true)), &theme);
        assert_eq!(on.scene.icons.len(), 1);
        assert_eq!(on.scene.icons[0].icon, IconId::Check);
        assert_eq!(
            on.scene.icons[0].tint,
            theme.resolve_color(ColorRole::AccentFg),
            "the check tint is a constant AccentFg"
        );
        assert_eq!(
            on.scene.icons[0].bounds.size.w, glyph,
            "a checked box shows the full-size check (scale 1)"
        );
        let off = build(checkbox(RwSignal::new(false)), &theme);
        assert!(
            off.scene.icons[0].bounds.size.w < 0.1,
            "an unchecked box's check is scaled to ~0 (hidden): {}",
            off.scene.icons[0].bounds.size.w
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
    fn checkbox_external_write_should_update_check() {
        // A write to the bound signal between frames updates the visual: the box bg fills instantly
        // (Surface → Accent). The check *size* pops via the ticker — see checkbox_check_should_pop_on_toggle.
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(false);
        let mut root = checkbox(value).into_element();
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let theme = Theme::light();
        let damage = Arc::new(DamageState::default());
        let accent = Background::Solid(theme.resolve_color(ColorRole::Accent));
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
        let has_accent = |s: &Scene| s.quads.iter().any(|q| q.bg == accent);
        let scene = render(&mut root, &mut arena, &mut layout, &mut text);
        assert!(
            !has_accent(&scene),
            "frame 1 (false): the box is not accent-filled"
        );
        value.set(true);
        let scene = render(&mut root, &mut arena, &mut layout, &mut text);
        assert!(
            has_accent(&scene),
            "an external write to true fills the box Accent"
        );
        drop(owner);
    }

    #[test]
    fn checkbox_check_should_pop_on_toggle() {
        // End-to-end (#266): with a `Ticker` in context, toggling on retargets the check's `Animated` scale;
        // driving the ticker a few frames grows the painted check from ~0 (mirrors the switch knob slide).
        use kagari_core::Ticker;
        use std::time::Duration;

        let owner = Owner::new();
        owner.set();
        let ticker = Ticker::new();
        provide_context(ticker.clone());
        let value = RwSignal::new(false);
        let theme = Theme::light();

        let mut root = checkbox(value).into_element();
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let damage = Arc::new(DamageState::default());
        let check_w = |root: &mut AnyElement,
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
                .icons
                .iter()
                .find(|s| s.icon == IconId::Check)
                .expect("the check icon paints")
                .bounds
                .size
                .w
        };

        let w0 = check_w(&mut root, &mut arena, &mut layout, &mut text);
        assert!(
            w0 < 0.1,
            "an unchecked box's check starts scaled to ~0: {w0}"
        );
        value.set(true); // the retarget effect fires set_target(1.0) → registers into the ticker
        // Drive a few frames: the check grows toward its full size (the spring settles over ~200ms, so a
        // handful of 16ms frames is mid-flight — grown but not yet full).
        for _ in 0..3 {
            ticker.tick_all(Duration::from_millis(16));
        }
        let w1 = check_w(&mut root, &mut arena, &mut layout, &mut text);
        assert!(
            w1 > w0,
            "after checking, the driven check has grown from ~0: w0={w0} w1={w1}"
        );
        drop(owner);
    }

    #[test]
    fn checkbox_check_pop_should_scale_smoothly() {
        // The #266 fix: the check grows via a paint-time *scale* (float), so its painted width is sub-pixel
        // (fractional) each frame — unlike #264's layout-size animation, which taffy rounding snapped to
        // whole pixels (0,2,4,6,…). Drive a few frames and assert the widths are fractional and grow.
        use kagari_core::Ticker;
        use std::time::Duration;

        let owner = Owner::new();
        owner.set();
        let ticker = Ticker::new();
        provide_context(ticker.clone());
        let value = RwSignal::new(false);
        let theme = Theme::light();

        let mut root = checkbox(value).into_element();
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let damage = Arc::new(DamageState::default());
        let check_w = |root: &mut AnyElement,
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
                .icons
                .iter()
                .find(|s| s.icon == IconId::Check)
                .expect("the check icon paints")
                .bounds
                .size
                .w
        };

        value.set(true);
        let mut widths = Vec::new();
        for _ in 0..8 {
            ticker.tick_all(Duration::from_millis(16));
            widths.push(check_w(&mut root, &mut arena, &mut layout, &mut text));
        }

        // Sub-pixel: at least one frame has a fractional width — a paint-time float scale, not the
        // taffy-rounded integer steps of the #264 layout-size approach.
        assert!(
            widths.iter().any(|w| w.fract().abs() > 1e-4),
            "the check grows in sub-pixel (fractional) steps, not integer ones: {widths:?}"
        );
        // And it grows over the drive (a coherent pop, not stuck).
        assert!(
            *widths.last().unwrap() > widths[0],
            "the check grows toward full size over the drive: {widths:?}"
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
}
