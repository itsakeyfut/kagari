//! The [`Menu`] widget (#77, MVP): a dropdown menu popup — items + separators anchored on the #219 portal,
//! with arrow-key navigation, Enter to select, and Esc / outside-click to dismiss.
//!
//! Visibility is a controlled `RwSignal<bool>` and the popup anchors to an [`AnchorHandle`] (Popover's
//! pattern). While open, the content mounts via `dyn_if` (so its focusables register only while open) and
//! **focuses itself** so arrows/Enter route to it; a **highlight** signal drives which row is active
//! (arrows move it, skipping separators; Enter runs that item's `on_select` and closes). Clicking a row
//! selects it (reaches the handler via the #289 event descent). `.open()` returns focus to the trigger on
//! close.
//!
//! MVP scope: submenus, `MenuBar`, type-ahead, and hover-open are follow-up issues.

use std::rc::Rc;

use kagari_base::{Point, SharedString};
use kagari_core::element::{Overlay, dyn_if};
use kagari_core::reactive::prelude::*;
use kagari_core::reactive::{Prop, RwSignal, rx};
use kagari_core::{
    AnchorHandle, AnyElement, IntoElement, KeyCode, Placement, div, overlay, text, use_focus_handle,
};
use kagari_style::{ColorRole, Styled};

/// A menu item's select callback — shared (`Rc`) so the menu content can be rebuilt each time it opens
/// while the same callback stays callable.
type OnSelect = Rc<dyn Fn()>;

/// One row of a menu: a selectable item or a divider.
enum MenuEntry {
    Item {
        label: SharedString,
        on_select: OnSelect,
    },
    Separator,
}

impl MenuEntry {
    fn is_item(&self) -> bool {
        matches!(self, MenuEntry::Item { .. })
    }
}

/// A dropdown menu popup bound to a `bool` signal (#77, MVP). Build with [`menu`]; add rows with
/// `.item(label, on_select)` / `.separator()`, chain `.placement(..)`. Returns `impl IntoElement`. While
/// `open` is true the menu renders anchored over the trigger; arrows navigate, Enter selects, Esc / an
/// outside press dismisses.
pub struct Menu {
    anchor: AnchorHandle,
    open: RwSignal<bool>,
    entries: Vec<MenuEntry>,
    placement: Placement,
}

/// Creates a menu anchored to `anchor` (tag the trigger with `div().anchor_ref(&anchor)`), its visibility
/// controlled by `open`. Add rows with [`Menu::item`] / [`Menu::separator`].
pub fn menu(anchor: &AnchorHandle, open: RwSignal<bool>) -> Menu {
    Menu {
        anchor: anchor.clone(),
        open,
        entries: Vec::new(),
        placement: Placement::Below,
    }
}

impl Menu {
    /// Adds a selectable item: its `label` and an `on_select` callback run when the item is chosen (by
    /// click or Enter), after which the menu closes.
    pub fn item(mut self, label: impl Into<SharedString>, on_select: impl Fn() + 'static) -> Self {
        self.entries.push(MenuEntry::Item {
            label: label.into(),
            on_select: Rc::new(on_select),
        });
        self
    }

    /// Adds a horizontal separator between items.
    pub fn separator(mut self) -> Self {
        self.entries.push(MenuEntry::Separator);
        self
    }

    /// Sets the anchor placement relative to the trigger (default [`Placement::Below`]).
    pub fn placement(mut self, placement: Placement) -> Self {
        self.placement = placement;
        self
    }
}

/// The index of the first selectable (non-separator) entry, or 0 if there are none.
fn first_selectable(entries: &[MenuEntry]) -> usize {
    entries.iter().position(MenuEntry::is_item).unwrap_or(0)
}

/// The next selectable index from `from` in the given direction, skipping separators and wrapping.
/// Returns `from` if there is no other selectable entry.
fn step_selectable(entries: &[MenuEntry], from: usize, down: bool) -> usize {
    let n = entries.len();
    if n == 0 {
        return 0;
    }
    let mut i = from;
    for _ in 0..n {
        i = if down { (i + 1) % n } else { (i + n - 1) % n };
        if entries[i].is_item() {
            return i;
        }
    }
    from
}

/// Builds the menu panel for one open cycle: a focused, keyboard-navigable list of item rows + separators.
/// Fresh focus handle + highlight signal each mount, so re-opening resets the highlight and re-focuses.
fn build_panel(entries: Rc<Vec<MenuEntry>>, open: RwSignal<bool>) -> impl IntoElement {
    let handle = use_focus_handle();
    let highlight = RwSignal::new(first_selectable(&entries));
    // Focus the menu on mount (= on open) so arrows / Enter / Esc route here.
    handle.focus();

    let nav = Rc::clone(&entries);
    let mut panel = div()
        .flex_col()
        .min_w_40()
        .py_1()
        .rounded_md()
        .bg(ColorRole::SurfaceRaised)
        .border_w_1()
        .border_color(Some(ColorRole::Border))
        .shadow_lg()
        .track_focus(&handle)
        .on_key_down(move |kev, _cx| {
            if kev.repeat {
                return;
            }
            match kev.code {
                KeyCode::ArrowDown => {
                    highlight.set(step_selectable(&nav, highlight.get_untracked(), true));
                }
                KeyCode::ArrowUp => {
                    highlight.set(step_selectable(&nav, highlight.get_untracked(), false));
                }
                KeyCode::Enter => {
                    if let Some(MenuEntry::Item { on_select, .. }) =
                        nav.get(highlight.get_untracked())
                    {
                        on_select();
                        open.set(false);
                    }
                }
                KeyCode::Escape => open.set(false),
                _ => {}
            }
        });

    for (i, entry) in entries.iter().enumerate() {
        match entry {
            MenuEntry::Item { label, on_select } => {
                let on_select = Rc::clone(on_select);
                panel = panel.child(
                    div()
                        .px_3()
                        .py_1p5()
                        // Highlighted row: Accent fill + AccentFg text; otherwise blends with the panel.
                        .bg(rx(move || {
                            if highlight.get() == i {
                                ColorRole::Accent
                            } else {
                                ColorRole::SurfaceRaised
                            }
                        }))
                        .on_mouse_enter(move |_, _| highlight.set(i))
                        .on_click(move |_, _| {
                            on_select();
                            open.set(false);
                        })
                        .child(text(label.clone()).color_role(rx(move || {
                            if highlight.get() == i {
                                ColorRole::AccentFg
                            } else {
                                ColorRole::Text
                            }
                        }))),
                );
            }
            MenuEntry::Separator => {
                panel = panel.child(div().h_px().bg(ColorRole::Border));
            }
        }
    }
    panel
}

/// Builds the menu's overlay scaffold: a `dyn_if`-gated panel, bound to `open` (paint-gate + focus
/// save/restore) and dismissed on an outside press. The caller applies positioning — `.anchor()` for the
/// dropdown ([`Menu::into_element`]), `.position()` for a pointer menu ([`Menu::into_positioned`], #78).
fn menu_overlay(entries: Vec<MenuEntry>, open: RwSignal<bool>) -> Overlay {
    let entries = Rc::new(entries);
    overlay(dyn_if(
        move || open.get(),
        move || build_panel(Rc::clone(&entries), open),
    ))
    .open(open)
    .dismiss_on_outside(true)
}

impl Menu {
    /// Re-presents this menu at an absolute window `at` point instead of anchored to an element (for
    /// [`ContextMenu`](crate::ContextMenu), #78). Reuses the same panel (items / nav / focus / dismiss)
    /// driven by the menu's `open`; the anchor / placement set on the menu are unused on this path.
    pub(crate) fn into_positioned(self, at: impl Into<Prop<Point>>) -> AnyElement {
        let Menu { open, entries, .. } = self;
        menu_overlay(entries, open).position(at).into_element()
    }

    /// The visibility signal, so a container (ContextMenu) can open the menu itself (e.g. on right-click).
    pub(crate) fn open_signal(&self) -> RwSignal<bool> {
        self.open
    }
}

impl IntoElement for Menu {
    fn into_element(self) -> AnyElement {
        let Menu {
            anchor,
            open,
            entries,
            placement,
        } = self;
        menu_overlay(entries, open)
            .anchor(&anchor, placement)
            .into_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::Arc;

    use kagari_base::{NodeId, Size as BSize};
    use kagari_core::arena::Arena;
    use kagari_core::damage::DamageState;
    use kagari_core::event::FocusRegistry;
    use kagari_core::overlay::{OverlayLayers, OverlayRegistry};
    use kagari_core::paint::render_tree;
    use kagari_core::reactive::{Owner, provide_context};
    use kagari_core::{HitTest, KeyEvent, Modifiers, dispatch_key};
    use kagari_layout::LayoutTree;
    use kagari_render::Scene;
    use kagari_style::Theme;
    use kagari_text::{FontDb, TextSystem};

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
        root_id: NodeId,
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
            root_id: NodeId::from_raw(0),
        };
        h.root_id = frame(&mut h);
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

    /// The menu panel node (overlay → dyn_if → panel), once mounted.
    fn panel_node(h: &Harness) -> NodeId {
        let dyn_if = h.arena.get(h.root_id).unwrap().children[0];
        h.arena.get(dyn_if).unwrap().children[0]
    }

    fn press(h: &mut Harness, code: KeyCode) {
        let panel = panel_node(h);
        let kev = KeyEvent::new(code, Modifiers::default(), true, false);
        dispatch_key(&mut h.root, &h.arena, Some(panel), &kev);
    }

    #[test]
    fn menu_should_open_and_select() {
        let owner = Owner::new();
        owner.set();
        provide_context(OverlayLayers::new());
        provide_context(FocusRegistry::new());

        let picked = Rc::new(Cell::new(0u32));
        let (a, b) = (Rc::clone(&picked), Rc::clone(&picked));
        let open = RwSignal::new(true);
        let mut h = harness(
            menu(&AnchorHandle::new(), open)
                .item("A", move || a.set(1))
                .item("B", move || b.set(2))
                .into_element(),
        );

        // Enter selects the highlighted (first) item and closes the menu.
        press(&mut h, KeyCode::Enter);
        assert_eq!(picked.get(), 1, "Enter selects the first item (A)");
        assert!(!open.get_untracked(), "selecting an item closes the menu");

        drop(owner);
    }

    #[test]
    fn menu_arrow_should_navigate_items() {
        let owner = Owner::new();
        owner.set();
        provide_context(OverlayLayers::new());
        provide_context(FocusRegistry::new());

        let picked = Rc::new(Cell::new(0u32));
        let (a, b) = (Rc::clone(&picked), Rc::clone(&picked));
        let open = RwSignal::new(true);
        let mut h = harness(
            menu(&AnchorHandle::new(), open)
                .item("A", move || a.set(1))
                .separator()
                .item("B", move || b.set(2))
                .into_element(),
        );

        // ArrowDown moves the highlight from A past the separator to B; Enter selects B.
        press(&mut h, KeyCode::ArrowDown);
        press(&mut h, KeyCode::Enter);
        assert_eq!(
            picked.get(),
            2,
            "ArrowDown skips the separator to B; Enter selects it"
        );

        drop(owner);
    }

    #[test]
    fn menu_esc_should_dismiss() {
        let owner = Owner::new();
        owner.set();
        provide_context(OverlayLayers::new());
        provide_context(FocusRegistry::new());

        let open = RwSignal::new(true);
        let mut h = harness(
            menu(&AnchorHandle::new(), open)
                .item("A", || {})
                .into_element(),
        );

        press(&mut h, KeyCode::Escape);
        assert!(!open.get_untracked(), "Esc dismisses the menu");

        drop(owner);
    }

    #[test]
    fn menu_should_render_when_open() {
        let owner = Owner::new();
        owner.set();
        provide_context(OverlayLayers::new());
        provide_context(FocusRegistry::new());

        let open = RwSignal::new(false);
        let mut h = harness(
            menu(&AnchorHandle::new(), open)
                .item("A", || {})
                .item("B", || {})
                .into_element(),
        );

        assert_eq!(
            overlay_band_quads(&mut h),
            0,
            "a closed menu renders nothing"
        );

        open.set(true);
        assert!(
            overlay_band_quads(&mut h) > 0,
            "an open menu renders its panel + items in the overlay band"
        );

        drop(owner);
    }

    #[test]
    fn menu_should_focus_on_open() {
        let owner = Owner::new();
        owner.set();
        provide_context(OverlayLayers::new());
        let reg = FocusRegistry::new();
        provide_context(reg.clone());

        let open = RwSignal::new(true);
        let _h = harness(
            menu(&AnchorHandle::new(), open)
                .item("A", || {})
                .into_element(),
        );

        assert!(
            reg.current().get_untracked().is_some(),
            "an open menu focuses itself so arrows/Enter route to it"
        );

        drop(owner);
    }

    #[test]
    fn menu_click_should_select_item() {
        use kagari_base::Point;
        use kagari_core::{DispatchState, MouseButton, MouseEvent, MouseKind, dispatch_mouse};

        let owner = Owner::new();
        owner.set();
        provide_context(OverlayLayers::new());
        provide_context(FocusRegistry::new());

        let picked = Rc::new(Cell::new(0u32));
        let (a, b) = (Rc::clone(&picked), Rc::clone(&picked));
        let open = RwSignal::new(true);
        let mut h = harness(
            menu(&AnchorHandle::new(), open)
                .item("A", move || a.set(1))
                .item("B", move || b.set(2))
                .into_element(),
        );

        // Click the second item's row (the menu paints at the origin for an unbound anchor, and it is the
        // root, so its laid-out bounds are the painted bounds). Its on_select runs and the menu closes.
        let panel = panel_node(&h);
        let item_b = h.arena.get(panel).unwrap().children[1];
        let bounds = h.layout.absolute_layout(item_b);
        let center = Point::new(
            bounds.origin.x + bounds.size.w / 2.0,
            bounds.origin.y + bounds.size.h / 2.0,
        );
        let mut dispatch = DispatchState::default();
        for kind in [
            MouseKind::Down(MouseButton::Left),
            MouseKind::Up(MouseButton::Left),
        ] {
            let ev = MouseEvent::new(kind, center, Modifiers::default());
            dispatch_mouse(&mut h.root, &h.arena, &h.hit, &ev, &mut dispatch);
        }
        assert_eq!(picked.get(), 2, "clicking item B runs its on_select");
        assert!(!open.get_untracked(), "clicking an item closes the menu");

        drop(owner);
    }

    #[test]
    fn menu_close_should_return_focus() {
        let owner = Owner::new();
        owner.set();
        provide_context(OverlayLayers::new());
        let reg = FocusRegistry::new();
        provide_context(reg.clone());

        // A trigger holds focus before the menu opens.
        let trigger = reg.handle();
        trigger.focus();

        let open = RwSignal::new(false);
        let mut h = harness(
            menu(&AnchorHandle::new(), open)
                .item("A", || {})
                .into_element(),
        );

        // Open → the overlay saves the trigger (before the menu focuses itself); the menu takes focus.
        open.set(true);
        frame(&mut h);
        assert_ne!(
            reg.current().get_untracked(),
            Some(trigger.id()),
            "the open menu holds focus, not the trigger"
        );

        // Close → the overlay returns focus to the pre-open trigger.
        open.set(false);
        assert_eq!(
            reg.current().get_untracked(),
            Some(trigger.id()),
            "closing the menu returns focus to the trigger"
        );

        drop(owner);
    }
}
