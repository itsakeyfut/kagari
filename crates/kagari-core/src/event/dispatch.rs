//! Mouse dispatch (#48, specs §1.5): turn a pointer position into handler calls.
//!
//! Two-phase **capture+bubble** over the element tree. The arena (#30) gives the dispatch *path*
//! (a node's ancestor chain); the `Div` elements hold the handlers and are reached by walking the
//! element tree along that path (the arena stores only the `NodeId` graph, not the elements). A
//! recursive descent yields capture on the way down and bubble on the way up.
//!
//! Handlers are imperative `FnMut` callbacks that write to signals (single-direction dataflow,
//! design.md §3) — not reactive effects, so RK-005 does not apply; they drop with their `Div` when
//! the node is removed (RK-006).
//!
//! Public handlers are bubble-phase only (`on_mouse_down`/…); the capture phase is modeled here for
//! ordering (and future `_capture` variants / gesture & DnD interception). Live winit→dispatch
//! wiring is deferred to the first interactive consumer; [`dispatch_mouse`] is the headless entry.

use kagari_base::{NodeId, Point};

use crate::arena::Arena;
use crate::element::{AnyElement, Event, EventCx};
use crate::event::HitTest;

/// Physical key identity (#49). Re-exported from winit (W3C UI Events `code`): kagari is desktop-only
/// and winit is a permanent dependency, so the standard `KeyCode` is used directly rather than
/// mirrored (RK-002 — classify keys physically). Pre-1.0 this can be swapped for a kagari enum if
/// publish-time decoupling from winit's `KeyCode` semver is wanted.
pub use winit::keyboard::KeyCode;

/// Active keyboard modifiers at the time of a mouse event. `#[non_exhaustive]`: delivered to
/// handlers (not user-constructed), so more modifier state can be added without a breaking change.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub struct Modifiers {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub meta: bool,
}

/// A mouse button. `Other` carries the platform button index for extra buttons.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Other(u16),
}

/// What a mouse event represents. `Click` is synthesized (a press + release of the same button on
/// the same node); `Enter`/`Leave` are synthesized by hover tracking. The rest are raw.
#[derive(Clone, Copy, PartialEq, Debug)]
#[non_exhaustive]
pub enum MouseKind {
    Down(MouseButton),
    Up(MouseButton),
    Move,
    Wheel { dx: f32, dy: f32 },
    Click(MouseButton),
    Enter,
    Leave,
}

/// A resolved mouse event delivered to elements: its `kind`, the pointer `pos` (logical px, window
/// coordinates), and the `modifiers` held. `#[non_exhaustive]`: delivered to handlers (not
/// user-constructed), so fields (e.g. timestamp) can be added without a breaking change.
#[derive(Clone, Copy, PartialEq, Debug)]
#[non_exhaustive]
pub struct MouseEvent {
    pub kind: MouseKind,
    pub pos: Point,
    pub modifiers: Modifiers,
}

/// A resolved keyboard event delivered to elements (#49): the physical `code`, the `modifiers` held,
/// whether it is a press (`pressed`) or release, and whether it is an auto-`repeat`. `#[non_exhaustive]`:
/// delivered to handlers (not user-constructed), so fields can be added without a breaking change.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub struct KeyEvent {
    pub code: KeyCode,
    pub modifiers: Modifiers,
    pub pressed: bool,
    pub repeat: bool,
}

/// Which key-listener a builder setter registers (matched against a [`KeyEvent`]'s `pressed` flag).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum KeyListenerKind {
    Down,
    Up,
}

impl KeyListenerKind {
    /// Whether this listener fires for a key event with the given `pressed` state.
    pub(crate) fn matches(self, pressed: bool) -> bool {
        matches!(
            (self, pressed),
            (KeyListenerKind::Down, true) | (KeyListenerKind::Up, false)
        )
    }
}

/// A boxed keyboard handler stored on an element (#49). Imperative (writes to signals), not a
/// reactive effect.
pub(crate) type KeyHandler = Box<dyn FnMut(&KeyEvent, &mut EventCx)>;

/// A dispatch phase. Describes the capture→target→bubble visit order the recursive delivery
/// follows; verified by `visit_order` (test-only — public handlers are bubble-phase, so no MVP
/// handler fires in the capture phase).
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    Capture,
    Bubble,
}

/// A pointer-capture request raised by a handler, applied to [`DispatchState`] after delivery.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CaptureOp {
    Capture(NodeId),
    Release,
}

/// Which listener kind a builder setter registers (matched against an incoming [`MouseKind`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ListenerKind {
    Down,
    Up,
    Move,
    Wheel,
    Click,
    Enter,
    Leave,
}

impl ListenerKind {
    /// Whether this listener fires for an event of `kind`.
    pub(crate) fn matches(self, kind: MouseKind) -> bool {
        matches!(
            (self, kind),
            (ListenerKind::Down, MouseKind::Down(_))
                | (ListenerKind::Up, MouseKind::Up(_))
                | (ListenerKind::Move, MouseKind::Move)
                | (ListenerKind::Wheel, MouseKind::Wheel { .. })
                | (ListenerKind::Click, MouseKind::Click(_))
                | (ListenerKind::Enter, MouseKind::Enter)
                | (ListenerKind::Leave, MouseKind::Leave)
        )
    }
}

/// A boxed event handler stored on an element. Imperative (writes to signals), not a reactive
/// effect.
pub(crate) type MouseHandler = Box<dyn FnMut(&MouseEvent, &mut EventCx)>;

/// How a delivery pass walks the path: bubble (all path nodes fire on the way up) or filtered
/// (only nodes in `only` fire — used for hover enter/leave over an ancestor-chain subset).
#[derive(Clone, Copy)]
pub(crate) enum Delivery<'a> {
    Bubble {
        path: &'a [NodeId],
    },
    Filtered {
        path: &'a [NodeId],
        only: &'a [NodeId],
    },
}

impl Delivery<'_> {
    fn path(&self) -> &[NodeId] {
        match self {
            Delivery::Bubble { path } | Delivery::Filtered { path, .. } => path,
        }
    }

    /// The node after `id` on the path (the child to recurse into).
    pub(crate) fn next_after(&self, id: NodeId) -> Option<NodeId> {
        let path = self.path();
        let i = path.iter().position(|&n| n == id)?;
        path.get(i + 1).copied()
    }

    /// Whether `id` fires in this delivery (every path node bubbles; only the filtered set fires
    /// on an enter/leave pass).
    pub(crate) fn should_fire(&self, id: NodeId) -> bool {
        match self {
            Delivery::Bubble { .. } => true,
            Delivery::Filtered { only, .. } => only.contains(&id),
        }
    }
}

/// Cross-event dispatch state for one pointer: the grabbed (captured) node, the node a press
/// landed on (for click synthesis), and the current hover ancestor-path. Lives on the window once
/// live winit wiring lands; the headless [`dispatch_mouse`] threads it explicitly.
#[derive(Default, Debug)]
pub struct DispatchState {
    pub captured: Option<NodeId>,
    press_target: Option<NodeId>,
    hover_path: Vec<NodeId>,
}

/// The ancestor path root→`node` (walks `Node::parent` up, then reverses). The dispatch path; also
/// reused by focus-within (#49).
pub(crate) fn ancestor_path(arena: &Arena, node: NodeId) -> Vec<NodeId> {
    let mut chain = vec![node];
    let mut cur = node;
    while let Some(parent) = arena.get(cur).and_then(|n| n.parent) {
        chain.push(parent);
        cur = parent;
    }
    chain.reverse();
    chain
}

/// The capture→target→bubble visit order over `path`: capture root→target, then bubble target→root.
/// Test-only: documents and verifies the order the recursive delivery follows structurally.
#[cfg(test)]
fn visit_order(path: &[NodeId]) -> Vec<(NodeId, Phase)> {
    let mut order = Vec::with_capacity(path.len() * 2);
    order.extend(path.iter().map(|&n| (n, Phase::Capture)));
    order.extend(path.iter().rev().map(|&n| (n, Phase::Bubble)));
    order
}

/// The hover transition between two ancestor paths: nodes newly entered (in `new`, not `old`) and
/// nodes left (in `old`, not `new`).
fn hover_transitions(old: &[NodeId], new: &[NodeId]) -> (Vec<NodeId>, Vec<NodeId>) {
    let entered = new.iter().filter(|n| !old.contains(n)).copied().collect();
    let left = old.iter().filter(|n| !new.contains(n)).copied().collect();
    (entered, left)
}

/// Delivers `ev` along `delivery` by walking the element tree, returning any pointer-capture
/// request a handler raised.
fn deliver(
    root: &mut AnyElement,
    arena: &Arena,
    delivery: Delivery,
    ev: &Event,
) -> Option<CaptureOp> {
    let mut cx = EventCx::new(arena, delivery);
    root.handle_event(ev, &mut cx);
    cx.capture_request()
}

/// Applies a handler's capture request to the dispatch state.
fn apply_capture(state: &mut DispatchState, op: Option<CaptureOp>) {
    match op {
        Some(CaptureOp::Capture(node)) => state.captured = Some(node),
        Some(CaptureOp::Release) => state.captured = None,
        None => {}
    }
}

/// Dispatches one mouse event: picks the target (the captured node during a grab, else the topmost
/// hit), computes its ancestor path, and delivers with capture+bubble. `Move` also diffs hover and
/// delivers Enter/Leave; `Up` synthesizes a `Click` on the press target and ends a grab.
pub fn dispatch_mouse(
    root: &mut AnyElement,
    arena: &Arena,
    hit_test: &HitTest,
    ev: &MouseEvent,
    state: &mut DispatchState,
) {
    let pick = hit_test.pick(ev.pos);
    let target = state.captured.or(pick);

    match ev.kind {
        MouseKind::Move => {
            // Hover follows the actual pointer (the pick), independent of any grab.
            let new_path = pick.map(|n| ancestor_path(arena, n)).unwrap_or_default();
            let (entered, left) = hover_transitions(&state.hover_path, &new_path);
            if !left.is_empty() {
                let leave = Event::Mouse(MouseEvent {
                    kind: MouseKind::Leave,
                    ..*ev
                });
                deliver(
                    root,
                    arena,
                    Delivery::Filtered {
                        path: &state.hover_path,
                        only: &left,
                    },
                    &leave,
                );
            }
            if !entered.is_empty() {
                let enter = Event::Mouse(MouseEvent {
                    kind: MouseKind::Enter,
                    ..*ev
                });
                deliver(
                    root,
                    arena,
                    Delivery::Filtered {
                        path: &new_path,
                        only: &entered,
                    },
                    &enter,
                );
            }
            state.hover_path = new_path;

            if let Some(t) = target {
                let path = ancestor_path(arena, t);
                deliver(
                    root,
                    arena,
                    Delivery::Bubble { path: &path },
                    &Event::Mouse(*ev),
                );
            }
        }
        MouseKind::Down(_) => {
            if let Some(t) = target {
                let path = ancestor_path(arena, t);
                let cap = deliver(
                    root,
                    arena,
                    Delivery::Bubble { path: &path },
                    &Event::Mouse(*ev),
                );
                apply_capture(state, cap);
                state.press_target = Some(t);
            }
        }
        MouseKind::Up(button) => {
            if let Some(t) = target {
                let path = ancestor_path(arena, t);
                deliver(
                    root,
                    arena,
                    Delivery::Bubble { path: &path },
                    &Event::Mouse(*ev),
                );
                // A press+release on the same node is a click.
                if state.press_target == Some(t) {
                    let click = Event::Mouse(MouseEvent {
                        kind: MouseKind::Click(button),
                        ..*ev
                    });
                    deliver(root, arena, Delivery::Bubble { path: &path }, &click);
                }
            }
            state.press_target = None;
            // The matching mouse-up ends a grab (a handler may have already released it).
            state.captured = None;
        }
        MouseKind::Wheel { .. } => {
            if let Some(t) = target {
                let path = ancestor_path(arena, t);
                deliver(
                    root,
                    arena,
                    Delivery::Bubble { path: &path },
                    &Event::Mouse(*ev),
                );
            }
        }
        // Synthesized internally, never received as raw input.
        MouseKind::Click(_) | MouseKind::Enter | MouseKind::Leave => {}
    }
}

/// Dispatches one key event to the `focused` node, bubbling up its ancestor chain (#49). Keyboard
/// events go to the focus chain (not a pointer hit), so the caller resolves the focused node from the
/// focus registry. A `None` focus (nothing focused) drops the event. Reuses the same bubble delivery
/// as the mouse path. Unhandled keys fall through to #50's keymap/actions (not yet wired).
pub fn dispatch_key(root: &mut AnyElement, arena: &Arena, focused: Option<NodeId>, ev: &KeyEvent) {
    if let Some(node) = focused {
        let path = ancestor_path(arena, node);
        deliver(
            root,
            arena,
            Delivery::Bubble { path: &path },
            &Event::Keyboard(*ev),
        );
    }
}

/// Dispatches a resolved [`Action`](crate::event::Action) to the `focused` node, bubbling up its
/// ancestor chain to `on_action` handlers (#50). Like [`dispatch_key`], the caller resolves the
/// focused node (from the focus registry); a `None` focus drops the action. Reuses the same bubble
/// delivery as the key/mouse paths.
pub fn dispatch_action(
    root: &mut AnyElement,
    arena: &Arena,
    focused: Option<NodeId>,
    action: &crate::event::Action,
) {
    if let Some(node) = focused {
        let path = ancestor_path(arena, node);
        deliver(
            root,
            arena,
            Delivery::Bubble { path: &path },
            &Event::Action(action.clone()),
        );
    }
}

/// Dispatches a recognized [`GestureEvent`](crate::event::GestureEvent) to the `target` node (the
/// topmost hit at the cursor), bubbling up its ancestor chain to `on_gesture` handlers (#51). Like
/// the other dispatch entries the caller resolves `target` (from the hit-test); a `None` target drops
/// the gesture. Reuses the same bubble delivery.
pub fn dispatch_gesture(
    root: &mut AnyElement,
    arena: &Arena,
    target: Option<NodeId>,
    ev: &crate::event::GestureEvent,
) {
    if let Some(node) = target {
        let path = ancestor_path(arena, node);
        deliver(
            root,
            arena,
            Delivery::Bubble { path: &path },
            &Event::Gesture(*ev),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arena::{Arena, Node};
    use crate::element::{DamageSink, Div, IntoElement, LayoutCx, div};
    use crate::event::{Action, FocusRegistry, GestureEvent, HitRegion, InteractFlags};
    use crate::reactive::Owner;
    use kagari_base::Rect;
    use kagari_layout::LayoutTree;
    use kagari_style::Theme;
    use kagari_text::{FontDb, TextSystem};
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;

    fn nid(raw: u64) -> NodeId {
        NodeId::from_raw(raw)
    }

    #[test]
    fn ancestor_path_should_walk_root_to_node() {
        // root → child → leaf, linked via parents in a hand-built arena.
        let mut arena = Arena::new();
        let root = arena.insert(Node::default());
        let child = arena.insert(Node {
            parent: Some(root),
            children: Vec::new(),
        });
        let leaf = arena.insert(Node {
            parent: Some(child),
            children: Vec::new(),
        });
        assert_eq!(ancestor_path(&arena, leaf), vec![root, child, leaf]);
        assert_eq!(ancestor_path(&arena, root), vec![root]);
    }

    #[test]
    fn dispatch_should_capture_then_bubble_in_order() {
        // The visit order over a path is capture root→target, then bubble target→root.
        let path = [nid(1), nid(2), nid(3)];
        assert_eq!(
            visit_order(&path),
            vec![
                (nid(1), Phase::Capture),
                (nid(2), Phase::Capture),
                (nid(3), Phase::Capture),
                (nid(3), Phase::Bubble),
                (nid(2), Phase::Bubble),
                (nid(1), Phase::Bubble),
            ],
        );
    }

    #[test]
    fn hover_transitions_should_diff_paths() {
        let old = [nid(1), nid(2), nid(3)];
        let new = [nid(1), nid(2), nid(4)];
        let (entered, left) = hover_transitions(&old, &new);
        assert_eq!(entered, vec![nid(4)]);
        assert_eq!(left, vec![nid(3)]);
    }

    /// Builds a retained `root` into a fresh arena/layout (so the arena holds parent/children) and
    /// returns the arena plus the root element. The element tree is what dispatch walks.
    fn build(root: Div) -> (AnyElement, Arena, NodeId) {
        struct NoopDamage;
        impl DamageSink for NoopDamage {
            fn mark_paint_dirty(&self, _id: NodeId) {}
        }
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let theme = Theme::default();
        let damage: std::sync::Arc<dyn DamageSink> = std::sync::Arc::new(NoopDamage);
        let mut root: AnyElement = root.into_element();
        let root_id = root.request_layout(&mut LayoutCx {
            arena: &mut arena,
            layout: &mut layout,
            text: &mut text,
            damage,
            theme: &theme,
            focus: None,
            cursor: None,
        });
        (root, arena, root_id)
    }

    /// A hit-test with one full-window region per node, ordered so later ids sit in front.
    fn hit_with(nodes: &[NodeId]) -> HitTest {
        let mut ht = HitTest::new();
        for (i, &node) in nodes.iter().enumerate() {
            ht.push(HitRegion {
                node,
                bounds: Rect::from_xywh(0.0, 0.0, 100.0, 100.0),
                clip: Rect::from_xywh(0.0, 0.0, 100.0, 100.0),
                order: i as u32,
                flags: InteractFlags {
                    mouse: true,
                    ..InteractFlags::default()
                },
            });
        }
        ht
    }

    fn down(pos: f32) -> MouseEvent {
        MouseEvent {
            kind: MouseKind::Down(MouseButton::Left),
            pos: Point::new(pos, pos),
            modifiers: Modifiers::default(),
        }
    }

    fn up(pos: f32) -> MouseEvent {
        MouseEvent {
            kind: MouseKind::Up(MouseButton::Left),
            pos: Point::new(pos, pos),
            modifiers: Modifiers::default(),
        }
    }

    #[test]
    fn stop_propagation_should_halt_remaining_path() {
        // outer > inner, both with on_mouse_down recording their label; inner stops propagation, so
        // the bubble must not reach outer.
        let log: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));
        let l_inner = Rc::clone(&log);
        let l_outer = Rc::clone(&log);
        let root = div()
            .on_mouse_down(move |_ev, cx| {
                l_outer.borrow_mut().push("outer");
                let _ = cx;
            })
            .child(div().on_mouse_down(move |_ev, cx| {
                l_inner.borrow_mut().push("inner");
                cx.stop_propagation();
            }));

        let (mut root, arena, root_id) = build(root);
        let inner_id = arena.get(root_id).unwrap().children[0];
        let ht = hit_with(&[root_id, inner_id]); // inner on top (larger order)
        let mut state = DispatchState::default();

        dispatch_mouse(&mut root, &arena, &ht, &down(5.0), &mut state);

        assert_eq!(
            *log.borrow(),
            vec!["inner"],
            "inner stops propagation, so outer's bubble handler never runs"
        );
    }

    #[test]
    fn dispatch_should_bubble_target_to_root_in_order() {
        // A normal (non-stopped) dispatch fires bubble handlers target→root through the *live*
        // recursive delivery (the capture→bubble contract `visit_order` documents, observed for real).
        let log: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));
        let (l_outer, l_inner) = (Rc::clone(&log), Rc::clone(&log));
        let root = div()
            .on_mouse_down(move |_e, _c| l_outer.borrow_mut().push("outer"))
            .child(div().on_mouse_down(move |_e, _c| l_inner.borrow_mut().push("inner")));

        let (mut root, arena, root_id) = build(root);
        let inner = arena.get(root_id).unwrap().children[0];
        let ht = hit_with(&[root_id, inner]); // inner on top
        let mut state = DispatchState::default();

        dispatch_mouse(&mut root, &arena, &ht, &down(5.0), &mut state);

        assert_eq!(
            *log.borrow(),
            vec!["inner", "outer"],
            "the event bubbles from the target up to the root"
        );
    }

    #[test]
    fn click_should_synthesize_on_press_release_same_node() {
        // Down then Up on the same node synthesizes exactly one Click (on_click matches Click only).
        let clicks = Rc::new(RefCell::new(0u32));
        let c = Rc::clone(&clicks);
        let root = div().on_click(move |_e, _c| *c.borrow_mut() += 1);

        let (mut root, arena, root_id) = build(root);
        let ht = hit_with(&[root_id]);
        let mut state = DispatchState::default();

        dispatch_mouse(&mut root, &arena, &ht, &down(5.0), &mut state);
        dispatch_mouse(&mut root, &arena, &ht, &up(5.0), &mut state);

        assert_eq!(
            *clicks.borrow(),
            1,
            "press+release on the same node is one click"
        );
    }

    #[test]
    fn click_should_not_synthesize_across_different_nodes() {
        // Press on A, release on B → no click (press_target != release target).
        let clicks = Rc::new(RefCell::new(0u32));
        let c = Rc::clone(&clicks);
        let root = div()
            .child(div().on_click(move |_e, _c| *c.borrow_mut() += 1))
            .child(div());

        let (mut root, arena, root_id) = build(root);
        let a = arena.get(root_id).unwrap().children[0];
        let b = arena.get(root_id).unwrap().children[1];
        let mut state = DispatchState::default();

        dispatch_mouse(
            &mut root,
            &arena,
            &hit_with(&[root_id, a]),
            &down(5.0),
            &mut state,
        );
        dispatch_mouse(
            &mut root,
            &arena,
            &hit_with(&[root_id, b]),
            &up(6.0),
            &mut state,
        );

        assert_eq!(
            *clicks.borrow(),
            0,
            "a release on a different node than the press does not click"
        );
    }

    #[test]
    fn wheel_should_deliver_delta_to_handler() {
        let seen = Rc::new(RefCell::new((0.0f32, 0.0f32)));
        let s = Rc::clone(&seen);
        let root = div().on_wheel(move |e, _c| {
            if let MouseKind::Wheel { dx, dy } = e.kind {
                *s.borrow_mut() = (dx, dy);
            }
        });

        let (mut root, arena, root_id) = build(root);
        let ht = hit_with(&[root_id]);
        let mut state = DispatchState::default();
        let wheel = MouseEvent {
            kind: MouseKind::Wheel { dx: 3.0, dy: -5.0 },
            pos: Point::new(5.0, 5.0),
            modifiers: Modifiers::default(),
        };

        dispatch_mouse(&mut root, &arena, &ht, &wheel, &mut state);

        assert_eq!(
            *seen.borrow(),
            (3.0, -5.0),
            "wheel delta reaches the handler"
        );
    }

    #[test]
    fn key_should_deliver_to_focused_and_bubble() {
        // A key event delivered to the focused (inner) node bubbles up its ancestor chain: the inner
        // node's on_key_down fires, then the outer's. Needs an Owner (the focus signal) — RK-005.
        let owner = Owner::new();
        owner.set();

        let log: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));
        let (l_outer, l_inner) = (Rc::clone(&log), Rc::clone(&log));

        let mut reg = FocusRegistry::new();
        let inner_focus = reg.handle();
        let root = div()
            .on_key_down(move |_e, _c| l_outer.borrow_mut().push("outer"))
            .child(
                div()
                    .track_focus(&inner_focus)
                    .on_key_down(move |_e, _c| l_inner.borrow_mut().push("inner")),
            );

        // Build with the registry attached so the inner FocusId → NodeId is recorded.
        struct NoopDamage;
        impl DamageSink for NoopDamage {
            fn mark_paint_dirty(&self, _id: NodeId) {}
        }
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let theme = Theme::default();
        let damage: Arc<dyn DamageSink> = Arc::new(NoopDamage);
        let mut root: AnyElement = root.into_element();
        root.request_layout(&mut LayoutCx {
            arena: &mut arena,
            layout: &mut layout,
            text: &mut text,
            damage,
            theme: &theme,
            focus: Some(&mut reg),
            cursor: None,
        });

        inner_focus.focus();
        let key = KeyEvent {
            code: KeyCode::KeyA,
            modifiers: Modifiers::default(),
            pressed: true,
            repeat: false,
        };
        dispatch_key(&mut root, &arena, reg.focused_node(), &key);

        assert_eq!(
            *log.borrow(),
            vec!["inner", "outer"],
            "the key bubbles from the focused node up to the root"
        );

        drop(owner);
    }

    #[test]
    fn key_should_not_deliver_when_no_focus() {
        // With nothing focused, dispatch_key drops the event — no handler fires. No signal/registry
        // is created here, so no Owner is needed.
        let fired = Rc::new(RefCell::new(false));
        let f = Rc::clone(&fired);
        let root = div().on_key_down(move |_e, _c| *f.borrow_mut() = true);

        let (mut root, arena, _root_id) = build(root);
        let key = KeyEvent {
            code: KeyCode::KeyA,
            modifiers: Modifiers::default(),
            pressed: true,
            repeat: false,
        };
        dispatch_key(&mut root, &arena, None, &key);

        assert!(
            !*fired.borrow(),
            "no focused node → the key is dropped and no handler fires"
        );
    }

    #[test]
    fn action_should_dispatch_up_focus_chain() {
        // A resolved action dispatched to the inner node bubbles up to the outer node's on_action.
        let log: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));
        let (l_outer, l_inner) = (Rc::clone(&log), Rc::clone(&log));
        let root = div()
            .on_action(move |_a, _c| l_outer.borrow_mut().push("outer"))
            .child(div().on_action(move |_a, _c| l_inner.borrow_mut().push("inner")));

        let (mut root, arena, root_id) = build(root);
        let inner = arena.get(root_id).unwrap().children[0];

        dispatch_action(&mut root, &arena, Some(inner), &Action::FocusNext);

        assert_eq!(
            *log.borrow(),
            vec!["inner", "outer"],
            "the action bubbles from the focused node up to the root"
        );

        // With nothing focused, the action is dropped.
        log.borrow_mut().clear();
        dispatch_action(&mut root, &arena, None, &Action::FocusNext);
        assert!(
            log.borrow().is_empty(),
            "no focused node → the action is dropped"
        );
    }

    #[test]
    fn gesture_should_dispatch_up_hit_path() {
        // A gesture dispatched to the inner node bubbles up to the outer node's on_gesture, and the
        // handler receives the gesture (a surface scrolls/zooms on pan/zoom).
        let log: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));
        let seen = Rc::new(RefCell::new(None));
        let (l_outer, l_inner) = (Rc::clone(&log), Rc::clone(&log));
        let seen_inner = Rc::clone(&seen);
        let root = div()
            .on_gesture(move |_g, _c| l_outer.borrow_mut().push("outer"))
            .child(div().on_gesture(move |g, _c| {
                l_inner.borrow_mut().push("inner");
                *seen_inner.borrow_mut() = Some(*g);
            }));

        let (mut root, arena, root_id) = build(root);
        let inner = arena.get(root_id).unwrap().children[0];

        let pan = GestureEvent::Pan { dx: 4.0, dy: -2.0 };
        dispatch_gesture(&mut root, &arena, Some(inner), &pan);

        assert_eq!(
            *log.borrow(),
            vec!["inner", "outer"],
            "the gesture bubbles from the hit node up to the root"
        );
        assert_eq!(
            *seen.borrow(),
            Some(pan),
            "the handler receives the gesture"
        );

        // With no target, the gesture is dropped.
        log.borrow_mut().clear();
        dispatch_gesture(&mut root, &arena, None, &pan);
        assert!(
            log.borrow().is_empty(),
            "no target → the gesture is dropped"
        );
    }

    #[test]
    fn key_up_should_deliver_to_release_handler_only() {
        // A release (`pressed: false`) fires `on_key_up` but not `on_key_down`.
        let owner = Owner::new();
        owner.set();

        let log: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));
        let (l_down, l_up) = (Rc::clone(&log), Rc::clone(&log));

        let mut reg = FocusRegistry::new();
        let focus = reg.handle();
        let root = div()
            .track_focus(&focus)
            .on_key_down(move |_e, _c| l_down.borrow_mut().push("down"))
            .on_key_up(move |_e, _c| l_up.borrow_mut().push("up"));

        struct NoopDamage;
        impl DamageSink for NoopDamage {
            fn mark_paint_dirty(&self, _id: NodeId) {}
        }
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let theme = Theme::default();
        let damage: Arc<dyn DamageSink> = Arc::new(NoopDamage);
        let mut root: AnyElement = root.into_element();
        root.request_layout(&mut LayoutCx {
            arena: &mut arena,
            layout: &mut layout,
            text: &mut text,
            damage,
            theme: &theme,
            focus: Some(&mut reg),
            cursor: None,
        });

        focus.focus();
        let key_up = KeyEvent {
            code: KeyCode::KeyA,
            modifiers: Modifiers::default(),
            pressed: false,
            repeat: false,
        };
        dispatch_key(&mut root, &arena, reg.focused_node(), &key_up);

        assert_eq!(
            *log.borrow(),
            vec!["up"],
            "a release fires on_key_up only, not on_key_down"
        );

        drop(owner);
    }

    #[test]
    fn pointer_capture_should_route_to_captured_node() {
        // With outer captured, a move whose topmost hit is inner still delivers the Move to the
        // captured (outer) path, not inner.
        let moved: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));
        let m_outer = Rc::clone(&moved);
        let m_inner = Rc::clone(&moved);
        let root = div()
            .on_mouse_move(move |_ev, _cx| m_outer.borrow_mut().push("outer"))
            .child(div().on_mouse_move(move |_ev, _cx| m_inner.borrow_mut().push("inner")));

        let (mut root, arena, root_id) = build(root);
        let inner_id = arena.get(root_id).unwrap().children[0];
        let ht = hit_with(&[root_id, inner_id]); // pick = inner
        let mut state = DispatchState {
            captured: Some(root_id),
            ..DispatchState::default()
        };

        let mv = MouseEvent {
            kind: MouseKind::Move,
            pos: Point::new(5.0, 5.0),
            modifiers: Modifiers::default(),
        };
        dispatch_mouse(&mut root, &arena, &ht, &mv, &mut state);

        assert_eq!(
            *moved.borrow(),
            vec!["outer"],
            "the grab routes Move to the captured node, not the picked inner node"
        );
    }

    #[test]
    fn hover_should_emit_enter_leave_on_path_change() {
        // Two sibling children under a root; moving from one to the other fires leave on the first
        // and enter on the second (the root stays hovered = no enter/leave for it).
        let log: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let (la, lb) = (Rc::clone(&log), Rc::clone(&log));
        let (la2, lb2) = (Rc::clone(&log), Rc::clone(&log));
        let root = div()
            .child(
                div()
                    .on_mouse_enter(move |_e, _c| la.borrow_mut().push("enter a".into()))
                    .on_mouse_leave(move |_e, _c| la2.borrow_mut().push("leave a".into())),
            )
            .child(
                div()
                    .on_mouse_enter(move |_e, _c| lb.borrow_mut().push("enter b".into()))
                    .on_mouse_leave(move |_e, _c| lb2.borrow_mut().push("leave b".into())),
            );

        let (mut root, arena, root_id) = build(root);
        let a = arena.get(root_id).unwrap().children[0];
        let b = arena.get(root_id).unwrap().children[1];
        let mut state = DispatchState::default();

        // Move over A: enter a.
        let ht_a = hit_with(&[root_id, a]);
        let mv = |p: f32| MouseEvent {
            kind: MouseKind::Move,
            pos: Point::new(p, p),
            modifiers: Modifiers::default(),
        };
        dispatch_mouse(&mut root, &arena, &ht_a, &mv(5.0), &mut state);
        // Move over B: leave a, enter b.
        let ht_b = hit_with(&[root_id, b]);
        dispatch_mouse(&mut root, &arena, &ht_b, &mv(6.0), &mut state);

        let log = log.borrow();
        assert!(log.contains(&"enter a".to_string()), "entered A first");
        assert!(log.contains(&"leave a".to_string()), "left A on crossing");
        assert!(
            log.contains(&"enter b".to_string()),
            "entered B on crossing"
        );
        assert!(!log.contains(&"leave b".to_string()), "B was never left");
    }
}
