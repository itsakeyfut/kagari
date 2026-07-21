//! The [`Panel`] widget (#228): a titled container — a header (title + action buttons + a collapse caret)
//! over a body — the standalone unit the Dock system (#239) composes.
//!
//! Collapse is a **bound `RwSignal<bool>`** (D2): the body [mounts/unmounts](kagari_core::element::dyn_if)
//! live off the frame loop (an instant toggle, with the ▾/▸ caret reflecting state). The smooth height tween
//! and a scrolling body are a follow-up (they need a reactive scroll viewport the current API lacks).
//! Clicking anywhere on the header (or Space/Enter while it is focused) toggles collapse; action buttons
//! stop propagation so pressing one does not also toggle. `.resizable(bool)` is reserved (the Dock resizes
//! panels via [`Splitter`](crate::Splitter)); a standalone resize grip is a follow-up.
//!
//! Content is a **builder** (`impl Fn() -> impl IntoElement`) rather than a pre-built element, since the
//! body is rebuilt each time it is expanded (the same idiom as [`Tabs`](crate::Tabs)' panels).

use kagari_base::SharedString;
use kagari_core::element::{Div, dyn_if};
use kagari_core::reactive::prelude::*;
use kagari_core::reactive::{RwSignal, rx};
use kagari_core::{AnyElement, DragPayload, IntoElement, KeyCode, div, text, use_focus_handle};
use kagari_style::{ColorRole, Styled};

use crate::button::Button;
use crate::control::{ControlSize, apply_size, label_px};

/// A panel's body **content builder** — called to build the body element (again each time the panel is
/// expanded, so a `dyn_if`-unmounted body can be rebuilt; a pre-built element could not).
type PanelContent = Box<dyn Fn() -> AnyElement>;

/// An applier that makes the header a drag source (set by [`Panel::drag_handle`]). Type-erased so `Panel`
/// stays non-generic while `drag_handle` accepts any payload type — the closure captures the payload and
/// applies `.drag_source(..)` + a title-chip `.drag_image(..)` to the header `Div` at build time.
type HeaderDrag = Box<dyn FnOnce(Div) -> Div>;

/// A titled container (#228): a header (title + [actions](Self::actions) + a collapse caret) over a body.
/// Build with [`panel`]; make it collapse with [`.collapsible`](Self::collapsible). Returns
/// `impl IntoElement`; `Styled` is not on its surface. Standalone (works without docking); the Dock (#239)
/// composes panels into regions.
pub struct Panel {
    title: SharedString,
    content: PanelContent,
    actions: Vec<Button>,
    /// The controlled collapse state; `None` = not collapsible (no caret, body always shown).
    collapsed: Option<RwSignal<bool>>,
    size: ControlSize,
    /// An optional drag-source applier for the header ([`drag_handle`](Self::drag_handle)); `None` = the
    /// header is not draggable.
    header_drag: Option<HeaderDrag>,
}

/// Creates a panel titled `title` whose body is built by `content` (a builder, rebuilt on each expand).
pub fn panel<C: IntoElement>(
    title: impl Into<SharedString>,
    content: impl Fn() -> C + 'static,
) -> Panel {
    Panel {
        title: title.into(),
        content: Box::new(move || content().into_element()),
        actions: Vec::new(),
        collapsed: None,
        size: ControlSize::Md,
        header_drag: None,
    }
}

impl Panel {
    /// Adds header action buttons (#70), shown right-aligned in the header.
    pub fn actions(mut self, actions: impl IntoIterator<Item = Button>) -> Self {
        self.actions.extend(actions);
        self
    }

    /// Makes the panel collapsible, bound to `collapsed` (controlled, D2): clicking anywhere on the
    /// header (or Space/Enter while it is focused) toggles it, and the body mounts only while it is
    /// `false`. The ▾/▸ caret is a pure affordance.
    pub fn collapsible(mut self, collapsed: RwSignal<bool>) -> Self {
        self.collapsed = Some(collapsed);
        self
    }

    /// Reserved: whether the panel can be resized. Currently a no-op — the Dock (#239) resizes panels via
    /// [`Splitter`](crate::Splitter); a standalone resize grip is a follow-up.
    pub fn resizable(self, _resizable: bool) -> Self {
        self
    }

    /// Sets the control size (header padding + title font).
    pub fn size(mut self, size: ControlSize) -> Self {
        self.size = size;
        self
    }

    /// Makes the **header** a drag source emitting `payload` (#178) — dragging the title bar past the drag
    /// threshold begins a drag, while a plain click still toggles collapse (the shell distinguishes the two).
    /// Payload-generic, so `Panel` stays dock-neutral: the Dock (#239) passes a `PanelId` to drive re-docking,
    /// but any `DragPayload` works. A title chip is shown as the cursor-tracked drag image.
    pub fn drag_handle<P: DragPayload + Clone>(mut self, payload: P) -> Self {
        let label = self.title.clone();
        let size = self.size;
        self.header_drag = Some(Box::new(move |h: Div| {
            h.drag_source(move || payload.clone())
                .drag_image(move || drag_chip(label.clone(), size))
        }));
        self
    }

    /// The panel's title (the Dock #239 uses it as a tab label when a panel is docked into a `Tabs` region).
    pub(crate) fn title(&self) -> SharedString {
        self.title.clone()
    }

    /// The panel's **body only** — the content without the header, border, or separator chrome. The Dock
    /// (#239) renders this inside a `Tabs` region, where the tab bar is the chrome (so the panel's own
    /// header would be redundant); a `.collapsible` binding is inert here (the tab bar owns visibility).
    pub(crate) fn into_body(self) -> AnyElement {
        build_body(self.size, &self.content)
    }
}

/// The panel body content (no header/border/separator): the content builder run inside a sized flex column.
/// Shared by the collapsible and non-collapsible body arms and by [`Panel::into_body`] — one body path, so
/// the three consumers cannot drift.
fn build_body(size: ControlSize, content: &PanelContent) -> AnyElement {
    apply_size(div().flex_col(), size)
        .child(content())
        .into_element()
}

/// The cursor-tracked drag image for a dragged panel header: a small surface chip showing the title.
fn drag_chip(label: SharedString, size: ControlSize) -> impl IntoElement {
    apply_size(div().flex().items_center(), size)
        .bg(ColorRole::SurfaceRaised)
        .rounded_md()
        .child(text(label).color_role(ColorRole::Text).size(label_px(size)))
}

/// A 1px full-width divider between the header and the body (light `Surface` ≈ `Bg`, so a border is what
/// separates them — RK-030). `min_h` fixes the height while the flex column stretches its width.
fn separator() -> AnyElement {
    div().min_h(1.0).bg(ColorRole::Border).into_element()
}

impl IntoElement for Panel {
    fn into_element(self) -> AnyElement {
        let Panel {
            title,
            content,
            actions,
            collapsed,
            size,
            header_drag,
        } = self;
        let font = label_px(size);

        // Header row: [caret?, title, spacer, ...actions]. Collapsible → the whole header is the collapse
        // target (click anywhere, or Space/Enter while focused) and is focusable; the caret is a pure
        // affordance. Action buttons stop propagation so pressing one does not also toggle the panel.
        let mut header = apply_size(div().flex().items_center(), size).bg(ColorRole::Surface);
        if let Some(sig) = collapsed {
            let handle = use_focus_handle();
            let ring = handle.clone();
            let caret = div().child(
                text(rx(move || {
                    SharedString::from(if sig.get() { "\u{25B8}" } else { "\u{25BE}" })
                }))
                .color_role(ColorRole::TextMuted)
                .size(font),
            );
            header = header
                .track_focus(&handle)
                .border_w_1()
                .border_color(rx(move || {
                    ring.is_focus_visible().then_some(ColorRole::FocusRing)
                }))
                .on_click(move |_ev, _cx| {
                    sig.update(|c| *c = !*c);
                })
                .on_key_down(move |kev, _cx| {
                    if !kev.repeat
                        && matches!(
                            kev.code,
                            KeyCode::Space | KeyCode::Enter | KeyCode::NumpadEnter
                        )
                    {
                        sig.update(|c| *c = !*c);
                    }
                })
                .child(caret);
        }
        header = header
            .child(text(title).color_role(ColorRole::Text).size(font))
            .child(div().flex_grow(1.0));
        for action in actions {
            if collapsed.is_some() {
                // The header toggles collapse on click AND on Space/Enter; wrap the action so neither
                // its click nor its key activation bubbles up to that toggle (target→root bubble,
                // halted here — the same `deliver` path honors stop_propagation for mouse and keys).
                header = header.child(
                    div()
                        .on_click(|_ev, cx| cx.stop_propagation())
                        .on_key_down(|kev, cx| {
                            if matches!(
                                kev.code,
                                KeyCode::Space | KeyCode::Enter | KeyCode::NumpadEnter
                            ) {
                                cx.stop_propagation();
                            }
                        })
                        .child(action),
                );
            } else {
                header = header.child(action);
            }
        }
        // Make the header a drag source if `drag_handle` was set (#239: drag the title bar to re-dock).
        if let Some(apply) = header_drag {
            header = apply(header);
        }

        // Body: the separator + built content, gated on the collapse signal (mounted only while expanded).
        let body: AnyElement = match collapsed {
            Some(sig) => dyn_if(
                move || !sig.get(),
                move || {
                    div()
                        .flex_col()
                        .child(separator())
                        .child(build_body(size, &content))
                },
            )
            .into_element(),
            None => div()
                .flex_col()
                .child(separator())
                .child(build_body(size, &content))
                .into_element(),
        };

        div()
            .flex_col()
            .border_w_1()
            .border_color(Some(ColorRole::Border))
            .rounded_md()
            .child(header)
            .child(body)
            .into_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;
    use std::sync::Arc as StdArc;

    use kagari_base::{Color, NodeId, Point, Size as BSize};
    use kagari_core::arena::Arena;
    use kagari_core::damage::DamageState;
    use kagari_core::event::FocusRegistry;
    use kagari_core::paint::render_tree;
    use kagari_core::reactive::{Owner, provide_context};
    use kagari_core::{
        DispatchState, HitTest, KeyEvent, Modifiers, MouseButton, MouseEvent, MouseKind,
        dispatch_drag_start, dispatch_key, dispatch_mouse,
    };
    use kagari_layout::LayoutTree;
    use kagari_render::{Background, Scene};
    use kagari_style::Theme;
    use kagari_text::{FontDb, TextSystem};

    use crate::button::button;

    const VIEWPORT: BSize = BSize { w: 300.0, h: 300.0 };

    fn green() -> Background {
        Background::Solid(Color::new(0.0, 1.0, 0.0, 1.0))
    }

    /// A distinctive 100×50 body so its presence is checkable in the scene.
    fn body_content() -> AnyElement {
        div()
            .size(BSize { w: 100.0, h: 50.0 })
            .background(green())
            .into_element()
    }

    struct Harness {
        root: AnyElement,
        arena: Arena,
        layout: LayoutTree,
        text: TextSystem,
        hit: HitTest,
        focus: FocusRegistry,
        damage: StdArc<DamageState>,
        theme: Theme,
        root_id: NodeId,
    }

    fn harness(root: AnyElement) -> Harness {
        let focus = FocusRegistry::new();
        provide_context(focus.clone());
        let mut h = Harness {
            root,
            arena: Arena::new(),
            layout: LayoutTree::new(),
            text: TextSystem::new(FontDb::new()),
            hit: HitTest::new(),
            focus,
            damage: StdArc::new(DamageState::default()),
            theme: Theme::light(),
            root_id: NodeId::from_raw(0),
        };
        let (id, _) = frame(&mut h);
        h.root_id = id;
        h
    }

    fn frame(h: &mut Harness) -> (NodeId, Scene) {
        let mut scene = Scene::new();
        let id = render_tree(
            &mut h.root,
            &mut h.arena,
            &mut h.layout,
            &mut h.text,
            None,
            Some(&mut h.hit),
            None,
            Some(&mut h.focus),
            None,
            None,
            &mut scene,
            VIEWPORT,
            &h.damage,
            &h.theme,
        )
        .unwrap();
        (id, scene)
    }

    fn has_body(scene: &Scene) -> bool {
        scene.quads.iter().any(|q| q.bg == green())
    }

    fn header(h: &Harness) -> NodeId {
        h.arena.get(h.root_id).unwrap().children[0]
    }

    fn header_child(h: &Harness, i: usize) -> NodeId {
        h.arena.get(header(h)).unwrap().children[i]
    }

    fn click(h: &mut Harness, node: NodeId) {
        let r = h.layout.absolute_layout(node);
        let p = Point::new(r.origin.x + r.size.w / 2.0, r.origin.y + r.size.h / 2.0);
        let mut st = DispatchState::default();
        for kind in [
            MouseKind::Down(MouseButton::Left),
            MouseKind::Up(MouseButton::Left),
        ] {
            let ev = MouseEvent::new(kind, p, Modifiers::default());
            dispatch_mouse(&mut h.root, &h.arena, &h.hit, &ev, &mut st);
        }
    }

    fn press(h: &mut Harness, focused: NodeId, code: KeyCode) {
        let kev = KeyEvent::new(code, Modifiers::default(), true, false);
        dispatch_key(&mut h.root, &h.arena, Some(focused), &kev);
    }

    #[test]
    fn panel_should_render_title_and_content() {
        let owner = Owner::new();
        owner.set();
        let mut h = harness(panel("Inspector", body_content).into_element());
        let (_, scene) = frame(&mut h);
        assert!(has_body(&scene), "a non-collapsible panel renders its body");
        drop(owner);
    }

    #[test]
    fn panel_should_collapse_body() {
        let owner = Owner::new();
        owner.set();
        let collapsed = RwSignal::new(false);
        let mut h = harness(
            panel("P", body_content)
                .collapsible(collapsed)
                .into_element(),
        );
        let (_, scene) = frame(&mut h);
        assert!(has_body(&scene), "expanded: the body is mounted");

        collapsed.set(true);
        let (_, scene) = frame(&mut h);
        assert!(!has_body(&scene), "collapsed: the body is unmounted");

        collapsed.set(false);
        let (_, scene) = frame(&mut h);
        assert!(has_body(&scene), "re-expanded: the body mounts again");
        drop(owner);
    }

    #[test]
    fn panel_non_collapsible_should_always_show_body() {
        let owner = Owner::new();
        owner.set();
        let mut h = harness(panel("P", body_content).into_element());
        // The header has no caret (first header child is the title, not a caret click target).
        let (_, scene) = frame(&mut h);
        assert!(
            has_body(&scene),
            "a non-collapsible panel always shows its body"
        );
        drop(owner);
    }

    #[test]
    fn panel_header_action_should_fire() {
        let owner = Owner::new();
        owner.set();
        let fired = Rc::new(Cell::new(false));
        let f = fired.clone();
        let action = button("Add").on_click(move || f.set(true));
        let mut h = harness(panel("P", body_content).actions([action]).into_element());
        // Header children (non-collapsible): [title, spacer, action] → the action is index 2.
        let action_id = header_child(&h, 2);
        click(&mut h, action_id);
        assert!(fired.get(), "clicking a header action fires its handler");
        drop(owner);
    }

    #[test]
    fn panel_caret_click_should_toggle_collapsed() {
        let owner = Owner::new();
        owner.set();
        let collapsed = RwSignal::new(false);
        let mut h = harness(
            panel("P", body_content)
                .collapsible(collapsed)
                .into_element(),
        );
        // Collapsible header children: [caret, title, spacer] → the caret is index 0.
        let caret = header_child(&h, 0);
        click(&mut h, caret);
        assert!(collapsed.get_untracked(), "clicking the caret collapses");
        click(&mut h, caret);
        assert!(!collapsed.get_untracked(), "clicking it again expands");
        drop(owner);
    }

    #[test]
    fn panel_title_click_should_toggle_collapsed() {
        let owner = Owner::new();
        owner.set();
        let collapsed = RwSignal::new(false);
        let mut h = harness(
            panel("P", body_content)
                .collapsible(collapsed)
                .into_element(),
        );
        // Collapsible header children: [caret, title, spacer] → the title is index 1. Clicking the title
        // (not only the caret) toggles, because the whole header is the collapse target.
        let title = header_child(&h, 1);
        click(&mut h, title);
        assert!(collapsed.get_untracked(), "clicking the title collapses");
        click(&mut h, title);
        assert!(!collapsed.get_untracked(), "clicking it again expands");
        drop(owner);
    }

    #[test]
    fn panel_action_click_should_not_toggle_collapsed() {
        let owner = Owner::new();
        owner.set();
        let collapsed = RwSignal::new(false);
        let fired = Rc::new(Cell::new(false));
        let f = fired.clone();
        let action = button("Add").on_click(move || f.set(true));
        let mut h = harness(
            panel("P", body_content)
                .collapsible(collapsed)
                .actions([action])
                .into_element(),
        );
        // Collapsible header children: [caret, title, spacer, action-wrapper] → the action is index 3.
        let action_wrapper = header_child(&h, 3);
        click(&mut h, action_wrapper);
        assert!(fired.get(), "the action still fires on click");
        assert!(
            !collapsed.get_untracked(),
            "the action's click stops propagation, so the header does not toggle"
        );
        drop(owner);
    }

    #[test]
    fn panel_drag_handle_should_make_header_a_drag_source() {
        // .drag_handle(payload) makes the header a drag source (#239: drag the title bar to re-dock).
        let owner = Owner::new();
        owner.set();
        let mut h = harness(
            panel("Scene", body_content)
                .drag_handle(7u32)
                .into_element(),
        );
        let hdr = header(&h);
        let produced = dispatch_drag_start(&mut h.root, &h.arena, hdr);
        let (payload, image) = produced.expect("the header is a drag source");
        assert_eq!(
            payload.into_any().downcast::<u32>().ok().map(|b| *b),
            Some(7),
            "the header emits the drag_handle payload"
        );
        assert!(image.is_some(), "a title-chip drag image is provided");
        drop(owner);
    }

    #[test]
    fn panel_action_keyboard_should_not_toggle_collapsed() {
        let owner = Owner::new();
        owner.set();
        let collapsed = RwSignal::new(false);
        let fired = Rc::new(Cell::new(false));
        let f = fired.clone();
        let action = button("Add").on_click(move || f.set(true));
        let mut h = harness(
            panel("P", body_content)
                .collapsible(collapsed)
                .actions([action])
                .into_element(),
        );
        // The focusable action button sits inside its stop-propagation wrapper (header child 3).
        let action_wrapper = header_child(&h, 3);
        let action_button = h.arena.get(action_wrapper).unwrap().children[0];
        // Activating the action from the keyboard (as if it were focused) fires it, but the key must
        // not bubble to the header's collapse toggle.
        press(&mut h, action_button, KeyCode::Space);
        assert!(fired.get(), "the action still activates on Space");
        assert!(
            !collapsed.get_untracked(),
            "the action's key activation stops propagation, so the header does not toggle"
        );
        drop(owner);
    }

    #[test]
    fn panel_keyboard_should_toggle_collapsed() {
        let owner = Owner::new();
        owner.set();
        let collapsed = RwSignal::new(false);
        let mut h = harness(
            panel("P", body_content)
                .collapsible(collapsed)
                .into_element(),
        );
        let hdr = header(&h);
        press(&mut h, hdr, KeyCode::Space);
        assert!(
            collapsed.get_untracked(),
            "Space on the focused header collapses"
        );
        press(&mut h, hdr, KeyCode::Enter);
        assert!(!collapsed.get_untracked(), "Enter toggles it back");
        drop(owner);
    }
}
