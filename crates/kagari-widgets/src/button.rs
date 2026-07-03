//! The [`Button`] widget (#70) — the first kagari-widget, establishing the §3.6 API conventions: a
//! variant-constructor builder that returns `impl IntoElement`, hides `Styled`, and composes core
//! elements with **semantic tokens**.
//!
//! **Lean MVP scope**: variant (primary/ghost/danger) + [`ControlSize`] + disabled + `on_click` (mouse
//! **and** Space/Enter) + a controlled `toggle` signal + focusability ([`use_focus_handle`], #218). The
//! *visual* state feedback (focus-ring outline, toggle-pressed color, icon) needs infra that is not yet
//! built (border rendering is deferred; `Text` color is static; there is no icon element) — those are
//! relocated to follow-up prerequisites; here `toggle`/focus are wired functionally without the visuals.

use std::rc::Rc;
use std::sync::Arc;

use kagari_base::SharedString;
use kagari_core::reactive::prelude::*;
use kagari_core::reactive::{ReadSignal, RwSignal, use_context};
use kagari_core::{AnyElement, IntoElement, KeyCode, Role, div, text, use_focus_handle};
use kagari_style::{ColorRole, Styled, Theme};

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

    /// Binds a toggle state: activation flips `signal` (controlled, D2). The pressed *visual* is a
    /// follow-up (reactive state styling); this wires the state flip functionally.
    pub fn toggle(mut self, signal: RwSignal<bool>) -> Self {
        self.toggle = Some(signal);
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

/// Resolves a role to a concrete label color at build time from the theme in context (the app provides
/// it, #45), falling back to the default theme when absent (GPU-free tests). The background *role*
/// stays theme-reactive (resolved at paint from `cx.theme`); a swap-reactive label color is a follow-up.
fn resolve_label_color(role: ColorRole) -> kagari_base::Color {
    use_context::<ReadSignal<Arc<Theme>>>()
        .map(|theme| theme.get_untracked().resolve_color(role))
        .unwrap_or_else(|| Theme::default().resolve_color(role))
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
        } = self;
        let (bg_role, fg_role) = roles(variant, disabled);
        let fg_color = resolve_label_color(fg_role);
        let handle = use_focus_handle();

        let mut root = div()
            .flex()
            .items_center()
            .justify_center()
            .bg(bg_role)
            .rounded_md()
            .track_focus(&handle)
            .role(Role::Button)
            .a11y_label(label.clone());
        root = apply_size(root, size);

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

        root.child(text(label).color(fg_color).size(label_px(size)))
            .into_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    use kagari_base::{Point, Size};
    use kagari_core::arena::Arena;
    use kagari_core::damage::DamageState;
    use kagari_core::paint::render_tree;
    use kagari_core::reactive::Owner;
    use kagari_core::{
        DispatchState, HitTest, KeyEvent, Modifiers, MouseButton, MouseEvent, MouseKind,
        dispatch_key, dispatch_mouse,
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
}
