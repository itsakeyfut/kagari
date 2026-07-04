//! The [`Button`] widget (#70) — the first kagari-widget, establishing the §3.6 API conventions: a
//! variant-constructor builder that returns `impl IntoElement`, hides `Styled`, and composes core
//! elements with **semantic tokens**.
//!
//! **Scope**: variant (primary/ghost/danger) + [`ControlSize`] + disabled + `on_click` (mouse **and**
//! Space/Enter) + a controlled `toggle` signal + focusability ([`use_focus_handle`], #218), with the
//! visual state feedback wired via #245: a **focus-ring** border shown while focused, a **toggle-pressed**
//! color bound to the toggle signal, and a theme-reactive **role-based** label color. A leading icon
//! (`.icon()`, #246) tints to match the label.

use std::rc::Rc;

use kagari_base::SharedString;
use kagari_core::reactive::prelude::*;
use kagari_core::reactive::{RwSignal, rx};
use kagari_core::{
    AnyElement, IconId, IntoElement, KeyCode, Role, div, icon, text, use_focus_handle,
};
use kagari_style::{ColorRole, Styled};

use crate::control::{ControlSize, apply_size, label_px};

/// A button's visual variant → its `(background, foreground)` semantic roles.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Variant {
    Primary,
    Ghost,
    Danger,
}

/// A clickable button (#70). Build with [`button`]; chain `.primary()`/`.ghost()`/`.danger()`,
/// `.size(..)`, `.disabled(..)`, `.on_click(..)`, `.toggle(..)`. Returns `impl IntoElement` — `Styled` is
/// **not** on its surface (external style overrides are disallowed, rules/design.md §1).
pub struct Button {
    label: SharedString,
    variant: Variant,
    size: ControlSize,
    disabled: bool,
    on_click: Option<Rc<dyn Fn()>>,
    toggle: Option<RwSignal<bool>>,
    icon_id: Option<IconId>,
}

/// Creates a primary, medium button with `label`.
pub fn button(label: impl Into<SharedString>) -> Button {
    Button {
        label: label.into(),
        variant: Variant::Primary,
        size: ControlSize::Md,
        disabled: false,
        on_click: None,
        toggle: None,
        icon_id: None,
    }
}

impl Button {
    /// The primary (accent) variant — the default.
    pub fn primary(mut self) -> Self {
        self.variant = Variant::Primary;
        self
    }

    /// The ghost (surface) variant — low-emphasis.
    pub fn ghost(mut self) -> Self {
        self.variant = Variant::Ghost;
        self
    }

    /// The danger (destructive) variant.
    pub fn danger(mut self) -> Self {
        self.variant = Variant::Danger;
        self
    }

    /// Sets the control size (D3).
    pub fn size(mut self, size: ControlSize) -> Self {
        self.size = size;
        self
    }

    /// Disables the button: muted styling and inert (no click / key activation).
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Runs `f` on activation — a mouse click **or** Space/Enter while focused (suppressed when disabled).
    pub fn on_click(mut self, f: impl Fn() + 'static) -> Self {
        self.on_click = Some(Rc::new(f));
        self
    }

    /// Binds a toggle state: activation flips `signal` (controlled, D2), and the pressed *visual* follows
    /// it — pressed uses the variant's active roles, unpressed a subtle Surface (reactive, #245).
    pub fn toggle(mut self, signal: RwSignal<bool>) -> Self {
        self.toggle = Some(signal);
        self
    }

    /// Adds a leading icon (#246), tinted to match the label and sized to the label font. Composed
    /// before the label, separated by the control's inner gap.
    pub fn icon(mut self, icon_id: IconId) -> Self {
        self.icon_id = Some(icon_id);
        self
    }
}

/// The `(background, foreground)` roles for `variant`, or muted roles when `disabled`.
fn roles(variant: Variant, disabled: bool) -> (ColorRole, ColorRole) {
    if disabled {
        return (ColorRole::SurfaceRaised, ColorRole::TextMuted);
    }
    match variant {
        Variant::Primary => (ColorRole::Accent, ColorRole::AccentFg),
        Variant::Ghost => (ColorRole::Surface, ColorRole::Text),
        Variant::Danger => (ColorRole::Danger, ColorRole::AccentFg),
    }
}

impl IntoElement for Button {
    fn into_element(self) -> AnyElement {
        let Button {
            label,
            variant,
            size,
            disabled,
            on_click,
            toggle,
            icon_id,
        } = self;
        let (bg_role, fg_role) = roles(variant, disabled);

        let mut root = div()
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .role(Role::Button)
            .a11y_label(label.clone());
        // Focusability + focus ring only when enabled (#245, RK-027): a disabled control is inert and
        // must skip the tab order — and so must not paint a focus ring. The ring reuses the element
        // border, shown only while focus-visible (keyboard modality; a tracked read → fine-grained repaint).
        if !disabled {
            let handle = use_focus_handle();
            let h = handle.clone();
            root = root
                .border_w_2()
                .border_color(rx(move || {
                    h.is_focus_visible().then_some(ColorRole::FocusRing)
                }))
                .track_focus(&handle);
        }
        root = apply_size(root, size);

        // Background + foreground. A toggle binds them reactively to its signal (pressed = the variant's
        // active roles, unpressed = a subtle Surface/Text); otherwise they are static roles (theme-reactive
        // at paint, muted when disabled). The label and an optional leading icon share the foreground tint.
        let (label_el, icon_el) = match toggle {
            Some(signal) if !disabled => {
                let (on_bg, on_fg) = roles(variant, false);
                root = root.bg(rx(move || {
                    if signal.get() {
                        on_bg
                    } else {
                        ColorRole::Surface
                    }
                }));
                let label_el = text(label)
                    .color_role(rx(
                        move || if signal.get() { on_fg } else { ColorRole::Text },
                    ))
                    .size(label_px(size));
                let icon_el = icon_id.map(|ic| {
                    icon(ic).size(label_px(size)).color_role(rx(move || {
                        if signal.get() { on_fg } else { ColorRole::Text }
                    }))
                });
                (label_el, icon_el)
            }
            _ => {
                root = root.bg(bg_role);
                let label_el = text(label).color_role(fg_role).size(label_px(size));
                let icon_el = icon_id.map(|ic| icon(ic).size(label_px(size)).color_role(fg_role));
                (label_el, icon_el)
            }
        };

        if !disabled {
            // One shared activation closure for both the pointer click and Space/Enter: flip the toggle
            // signal (if any), then run the user callback (if any).
            let activate: Rc<dyn Fn()> = Rc::new(move || {
                if let Some(signal) = toggle {
                    signal.set(!signal.get_untracked());
                }
                if let Some(cb) = &on_click {
                    cb();
                }
            });
            let on_press = activate.clone();
            let on_key = activate;
            root = root
                .on_click(move |_ev, _cx| on_press())
                .on_key_down(move |kev, _cx| {
                    // Activate once per physical press: `on_key_down` already fires only on press, but
                    // it also fires on OS auto-repeat (`repeat = true`) — guard against that so a held
                    // Space/Enter doesn't re-run the callback / rapidly flip the toggle.
                    if !kev.repeat
                        && matches!(
                            kev.code,
                            KeyCode::Space | KeyCode::Enter | KeyCode::NumpadEnter
                        )
                    {
                        on_key();
                    }
                });
        }

        if let Some(icon_el) = icon_el {
            root = root.child(icon_el);
        }
        root.child(label_el).into_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::Arc;

    use kagari_base::{Point, Size};
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
    use kagari_style::Theme;
    use kagari_text::{FontDb, TextSystem};

    const VIEWPORT: Size = Size { w: 200.0, h: 100.0 };

    /// Builds `widget` through the core paint pass into a scene + hit-test (atlas `None`, so label
    /// glyphs are skipped — only the background quad emits). Assumes an ambient `Owner` (set by the
    /// caller: `use_focus_handle` mints a signal). Returns the tree so tests can dispatch onto it.
    struct Built {
        root: AnyElement,
        arena: Arena,
        layout: LayoutTree,
        scene: Scene,
        hit: HitTest,
        root_id: kagari_base::NodeId,
    }

    fn build(widget: impl IntoElement) -> Built {
        let mut root = widget.into_element();
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let mut scene = Scene::new();
        let mut hit = HitTest::new();
        let damage = Arc::new(DamageState::default());
        let theme = Theme::default();
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
            &theme,
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

    /// Dispatches a Space key-press to `root_id` (as if it were focused).
    fn press_key(b: &mut Built, code: KeyCode) {
        let kev = KeyEvent::new(code, Modifiers::default(), true, false);
        dispatch_key(&mut b.root, &b.arena, Some(b.root_id), &kev);
    }

    /// Dispatches a full click (Down then Up) at the center of the button.
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
    fn button_primary_should_use_accent_bg() {
        let owner = Owner::new();
        owner.set();
        let b = build(button("OK").primary());
        let theme = Theme::default();
        let bg = b
            .scene
            .quads
            .iter()
            .find(|q| q.bg == Background::Solid(theme.resolve_color(ColorRole::Accent)));
        assert!(
            bg.is_some(),
            "the primary button paints an Accent background"
        );
        drop(owner);
    }

    #[test]
    fn button_should_fire_on_click() {
        let owner = Owner::new();
        owner.set();
        let fired = Rc::new(Cell::new(0));
        let f = fired.clone();
        let mut b = build(button("OK").on_click(move || f.set(f.get() + 1)));
        click(&mut b);
        assert_eq!(fired.get(), 1, "a click fires on_click once");
        drop(owner);
    }

    #[test]
    fn button_should_activate_on_space_and_enter() {
        let owner = Owner::new();
        owner.set();
        let fired = Rc::new(Cell::new(0));
        let f = fired.clone();
        let mut b = build(button("OK").on_click(move || f.set(f.get() + 1)));
        press_key(&mut b, KeyCode::Space);
        press_key(&mut b, KeyCode::Enter);
        assert_eq!(fired.get(), 2, "Space and Enter each activate the button");
        drop(owner);
    }

    #[test]
    fn button_should_not_activate_on_key_repeat() {
        let owner = Owner::new();
        owner.set();
        let fired = Rc::new(Cell::new(0));
        let f = fired.clone();
        let mut b = build(button("OK").on_click(move || f.set(f.get() + 1)));
        // An OS auto-repeat event (pressed = true, repeat = true) must not re-activate the button.
        let repeat = KeyEvent::new(KeyCode::Enter, Modifiers::default(), true, true);
        dispatch_key(&mut b.root, &b.arena, Some(b.root_id), &repeat);
        assert_eq!(
            fired.get(),
            0,
            "an auto-repeat key event does not activate the button"
        );
        drop(owner);
    }

    #[test]
    fn button_disabled_should_not_fire_click() {
        let owner = Owner::new();
        owner.set();
        let fired = Rc::new(Cell::new(0));
        let f = fired.clone();
        let mut b = build(
            button("OK")
                .disabled(true)
                .on_click(move || f.set(f.get() + 1)),
        );
        click(&mut b);
        press_key(&mut b, KeyCode::Enter);
        assert_eq!(fired.get(), 0, "a disabled button is inert");
        drop(owner);
    }

    #[test]
    fn button_toggle_should_flip_signal() {
        let owner = Owner::new();
        owner.set();
        let pressed = RwSignal::new(false);
        let mut b = build(button("Bold").toggle(pressed));
        press_key(&mut b, KeyCode::Enter);
        assert!(
            pressed.get_untracked(),
            "activation flips the toggle signal on"
        );
        press_key(&mut b, KeyCode::Enter);
        assert!(!pressed.get_untracked(), "activation flips it back off");
        drop(owner);
    }

    #[test]
    fn button_toggle_pressed_should_change_bg_role() {
        // The toggle-pressed visual (#245): the background role follows the toggle signal — unpressed
        // paints Surface, pressed paints the variant's active role (Accent). Two frames, signal written
        // between them (RK-009); serial + `owner` alive (RK-005).
        let owner = Owner::new();
        owner.set();
        let pressed = RwSignal::new(false);
        let mut root = button("Bold").toggle(pressed).into_element();

        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let theme = Theme::default();
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
        assert!(
            scene
                .quads
                .iter()
                .any(|q| q.bg == Background::Solid(theme.resolve_color(ColorRole::Surface))),
            "unpressed toggle paints a Surface background"
        );

        pressed.set(true);
        let scene = render(&mut root, &mut arena, &mut layout, &mut text);
        assert!(
            scene
                .quads
                .iter()
                .any(|q| q.bg == Background::Solid(theme.resolve_color(ColorRole::Accent))),
            "pressed toggle paints an Accent background"
        );
        drop(owner);
    }

    #[test]
    fn button_focused_should_paint_focus_ring() {
        // The focus-ring visual (#245): a focused button paints a FocusRing-colored border; an unfocused
        // one paints none. Drives Button's own focus composition (use_focus_handle from a context
        // FocusRegistry + track_focus + is_focused), not just the Div-level mechanism. `Theme::light`
        // has a real FocusRing color + B2 width. Serial + `owner` alive (RK-005).
        let owner = Owner::new();
        owner.set();
        let mut registry = FocusRegistry::new();
        provide_context(registry.clone());
        let mut root = button("OK").into_element();

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
        let has_ring = |scene: &Scene| {
            scene
                .quads
                .iter()
                .any(|q| q.border.color == ring && q.border.widths.top > 0.0)
        };

        // Frame 1: unfocused → no focus ring.
        let scene = render(&mut root, &mut arena, &mut layout, &mut text, &mut registry);
        assert!(
            !has_ring(&scene),
            "an unfocused button paints no focus ring"
        );

        // Frame 2: focused but via a non-keyboard modality (`focus_next` without a key stamp) → the ring
        // stays hidden (`:focus-visible`).
        registry.focus_next();
        let scene = render(&mut root, &mut arena, &mut layout, &mut text, &mut registry);
        assert!(
            !has_ring(&scene),
            "a focused-but-not-focus-visible button paints no ring"
        );

        // Frame 3: a key press marks keyboard modality → the focus ring appears.
        registry.set_focus_visible(true);
        let scene = render(&mut root, &mut arena, &mut layout, &mut text, &mut registry);
        assert!(
            has_ring(&scene),
            "a focus-visible button paints a FocusRing border"
        );
        drop(owner);
    }

    #[test]
    fn button_icon_should_emit_icon_sprite() {
        // `.icon(..)` (#246) composes a leading icon leaf: the deferred model emits it into `scene.icons`
        // even without an atlas, so a GPU-free scene-structure test can assert the request.
        let owner = Owner::new();
        owner.set();
        let b = build(button("Confirm").icon(IconId::Check));
        assert_eq!(
            b.scene.icons.len(),
            1,
            "an icon button emits one icon request"
        );
        assert_eq!(b.scene.icons[0].icon, IconId::Check);
        drop(owner);
    }

    #[test]
    fn button_disabled_icon_should_use_muted_tint() {
        // The icon shares the label's foreground tint — muted (TextMuted) when disabled.
        let owner = Owner::new();
        owner.set();
        let b = build(button("x").icon(IconId::Check).disabled(true));
        assert_eq!(b.scene.icons.len(), 1);
        assert_eq!(
            b.scene.icons[0].tint,
            Theme::default().resolve_color(ColorRole::TextMuted),
            "a disabled button's icon uses the muted foreground role"
        );
        drop(owner);
    }
}
