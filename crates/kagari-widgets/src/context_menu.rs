//! The [`ContextMenu`] widget (#78): a right-click-triggered, pointer-anchored menu.
//!
//! Wraps a `trigger` element; a secondary (right) button press opens a [`Menu`] (#77) at the pointer via
//! the #219 overlay substrate (absolute [`position`](kagari_core::element::Overlay::position) instead of
//! an element anchor). The menu reuses the dropdown's items / arrow-nav / select / focus / outside-dismiss
//! unchanged — only the trigger and placement differ.

use kagari_base::Point;
use kagari_core::reactive::prelude::*;
use kagari_core::reactive::{RwSignal, rx};
use kagari_core::{AnyElement, IntoElement, MouseButton, MouseKind, div};

use crate::menu::Menu;

/// A right-click-triggered menu bound to a `trigger` element (#78). Build with [`context_menu`]; the passed
/// [`Menu`] supplies the items (its anchor is unused — the popup opens at the pointer, and the menu's `open`
/// signal is its visibility). Returns `impl IntoElement`. A secondary-button press on the trigger opens the
/// menu at the pointer; arrows navigate, Enter / click selects, Esc / an outside press dismisses.
pub struct ContextMenu {
    trigger: AnyElement,
    menu: Menu,
}

/// Creates a context menu: a secondary (right) button press on `trigger` opens `menu` at the pointer.
/// Build `menu` with [`menu`](crate::menu()) (its anchor is unused — pass `AnchorHandle::new()`); the
/// menu's `open` signal controls visibility.
pub fn context_menu(trigger: impl IntoElement, menu: Menu) -> ContextMenu {
    ContextMenu {
        trigger: trigger.into_element(),
        menu,
    }
}

impl IntoElement for ContextMenu {
    fn into_element(self) -> AnyElement {
        let ContextMenu { trigger, menu } = self;
        let open = menu.open_signal();
        // The last right-click position; the overlay reads it each paint to place the popup (#219 absolute).
        let pointer = RwSignal::new(Point::new(0.0, 0.0));

        // The trigger: a secondary (right) button press records the pointer and opens the menu there. Wrap
        // it in a div so the handler attaches to any `IntoElement`; on_mouse_down is bubble-phase, so a
        // press anywhere on the trigger reaches it.
        let target = div()
            .on_mouse_down(move |ev, _cx| {
                if let MouseKind::Down(MouseButton::Right) = ev.kind {
                    pointer.set(ev.pos);
                    open.set(true);
                }
            })
            .child(trigger);

        // The menu, re-presented at the pointer. As a portal it does not affect the trigger's layout.
        let popup = menu.into_positioned(rx(move || pointer.get()));

        div().child(target).child(popup).into_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use kagari_base::{NodeId, Size as BSize};
    use kagari_core::arena::Arena;
    use kagari_core::damage::DamageState;
    use kagari_core::event::{DispatchState, FocusRegistry};
    use kagari_core::overlay::{OverlayLayers, OverlayRegistry};
    use kagari_core::paint::render_tree;
    use kagari_core::reactive::{Owner, provide_context};
    use kagari_core::{
        AnchorHandle, HitTest, Modifiers, MouseEvent, dispatch_mouse, text, use_focus_handle,
    };
    use kagari_layout::LayoutTree;
    use kagari_render::Scene;
    use kagari_style::{ColorRole, Styled, Theme};
    use kagari_text::{FontDb, TextSystem};

    use crate::menu;

    const VIEWPORT: BSize = BSize { w: 300.0, h: 300.0 };
    const OVERLAY_BAND: u32 = 1 << 28;

    struct Harness {
        root: AnyElement,
        arena: Arena,
        layout: LayoutTree,
        text: TextSystem,
        hit: HitTest,
        reg: OverlayRegistry,
        damage: Arc<DamageState>,
        theme: Theme,
    }

    fn harness(root: AnyElement) -> Harness {
        let mut h = Harness {
            root,
            arena: Arena::new(),
            layout: LayoutTree::new(),
            text: TextSystem::new(FontDb::new()),
            hit: HitTest::new(),
            reg: OverlayRegistry::new(),
            damage: Arc::new(DamageState::default()),
            theme: Theme::light(),
        };
        frame(&mut h);
        h
    }

    /// Renders one frame, returning the root node id.
    fn frame(h: &mut Harness) -> NodeId {
        let mut scene = Scene::new();
        render_tree(
            &mut h.root,
            &mut h.arena,
            &mut h.layout,
            &mut h.text,
            None,
            Some(&mut h.hit),
            Some(&mut h.reg),
            None,
            None,
            None,
            &mut scene,
            VIEWPORT,
            &h.damage,
            &h.theme,
        )
        .unwrap()
    }

    fn overlay_band_quads(h: &mut Harness) -> usize {
        let mut scene = Scene::new();
        render_tree(
            &mut h.root,
            &mut h.arena,
            &mut h.layout,
            &mut h.text,
            None,
            Some(&mut h.hit),
            Some(&mut h.reg),
            None,
            None,
            None,
            &mut scene,
            VIEWPORT,
            &h.damage,
            &h.theme,
        )
        .unwrap();
        scene
            .quads
            .iter()
            .filter(|q| q.order >= OVERLAY_BAND)
            .count()
    }

    /// A sized, focusable right-click surface with a label, so it has real bounds to click within
    /// ([`trigger_center`] derives the click point from them).
    fn trigger() -> impl IntoElement {
        div()
            .min_w_40()
            .p_8()
            .bg(ColorRole::Surface)
            .track_focus(&use_focus_handle())
            .child(text("Right-click me").color_role(ColorRole::Text))
    }

    /// Sends a mouse-button press of `button` at `pos` (a press, not a full click).
    fn press(h: &mut Harness, button: MouseButton, pos: Point) {
        let mut dispatch = DispatchState::default();
        let ev = MouseEvent::new(MouseKind::Down(button), pos, Modifiers::default());
        dispatch_mouse(&mut h.root, &h.arena, &h.hit, &ev, &mut dispatch);
    }

    /// The center of the trigger (root → target wrapper), a point guaranteed to hit the right-click surface.
    fn trigger_center(h: &mut Harness) -> Point {
        let root = frame(h);
        let wrapper = h.arena.get(root).unwrap().children[0];
        let b = h.layout.absolute_layout(wrapper);
        Point::new(b.origin.x + b.size.w / 2.0, b.origin.y + b.size.h / 2.0)
    }

    #[test]
    fn context_menu_right_click_should_open_at_pointer() {
        let owner = Owner::new();
        owner.set();
        provide_context(OverlayLayers::new());
        provide_context(FocusRegistry::new());

        let open = RwSignal::new(false);
        let mut h = harness(
            context_menu(
                trigger(),
                menu(&AnchorHandle::new(), open)
                    .item("A", || {})
                    .item("B", || {}),
            )
            .into_element(),
        );
        assert_eq!(overlay_band_quads(&mut h), 0, "the menu starts closed");

        // Right-click near the trigger's bottom-right corner: the popup opens with its top-left there, so a
        // probe just beyond that corner sits *outside* the trigger and *outside* where an origin-anchored
        // popup would be. It is empty while closed and lands inside a menu row once open — proving the popup
        // opened at the pointer, not at the origin.
        let root = frame(&mut h);
        let wrapper = h.arena.get(root).unwrap().children[0];
        let b = h.layout.absolute_layout(wrapper);
        let p = Point::new(b.origin.x + b.size.w - 2.0, b.origin.y + b.size.h - 2.0);
        let probe = Point::new(p.x + 6.0, p.y + 12.0);
        assert!(
            h.hit.pick(probe).is_none(),
            "nothing is painted beyond the trigger while the menu is closed"
        );

        press(&mut h, MouseButton::Right, p);
        frame(&mut h);

        assert!(open.get_untracked(), "a right-click opens the menu");
        assert!(
            overlay_band_quads(&mut h) > 0,
            "the menu renders in the overlay band"
        );
        assert!(
            h.hit.pick(probe).is_some(),
            "the popup opened at the pointer (beyond the trigger), not at the origin"
        );

        drop(owner);
    }

    #[test]
    fn context_menu_left_click_should_not_open() {
        let owner = Owner::new();
        owner.set();
        provide_context(OverlayLayers::new());
        provide_context(FocusRegistry::new());

        let open = RwSignal::new(false);
        let mut h = harness(
            context_menu(trigger(), menu(&AnchorHandle::new(), open).item("A", || {}))
                .into_element(),
        );

        // A primary (left) press is not the context-menu trigger — the menu stays closed.
        let c = trigger_center(&mut h);
        press(&mut h, MouseButton::Left, c);
        frame(&mut h);

        assert!(
            !open.get_untracked(),
            "a left-click does not open the context menu"
        );
        assert_eq!(
            overlay_band_quads(&mut h),
            0,
            "the menu stays closed on a left-click"
        );

        drop(owner);
    }

    #[test]
    fn context_menu_should_dismiss_on_outside_click() {
        let owner = Owner::new();
        owner.set();
        let layers = OverlayLayers::new();
        provide_context(layers.clone());
        provide_context(FocusRegistry::new());

        let open = RwSignal::new(false);
        let mut h = harness(
            context_menu(trigger(), menu(&AnchorHandle::new(), open).item("A", || {}))
                .into_element(),
        );

        // Open via right-click: the menu registers a dismiss-on-outside layer bound to `open` (#219).
        let c = trigger_center(&mut h);
        press(&mut h, MouseButton::Right, c);
        frame(&mut h);
        assert!(open.get_untracked(), "the menu is open");
        assert!(
            !layers.is_empty(),
            "the open menu registered a dismiss layer"
        );

        // An outside press routes to `dismiss_topmost`, which clears the layer's `open` signal.
        assert!(
            layers.dismiss_topmost(),
            "the dismissible layer is dismissed"
        );
        assert!(
            !open.get_untracked(),
            "dismissing sets the open signal false (#219)"
        );
        assert_eq!(
            overlay_band_quads(&mut h),
            0,
            "the dismissed menu no longer renders its panel"
        );

        drop(owner);
    }
}
