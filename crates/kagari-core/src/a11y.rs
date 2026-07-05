//! Accessibility (#67, specs §1.12): elements carry optional semantic info (role/label/value), and
//! the element tree derives an `accesskit` tree — nodes + laid-out bounds + parent/child + the focused
//! node — synced to the OS (Windows UIA etc.) via `accesskit_winit`. a11y focus follows the framework
//! focus (`FocusRegistry`, §1.5).
//!
//! `accesskit` stays an internal detail: the public surface is the kagari-owned [`Role`] plus the
//! `Div` builders (`role`/`a11y_label`/`a11y_value`) — no `accesskit` type leaks (same isolation as
//! taffy/wgpu). The tree is **sparse**: only annotated elements become accesskit nodes, parented to
//! the nearest annotated ancestor under a synthetic window root, so a `List → ListItem` hierarchy is
//! preserved when both are annotated while unannotated containers add no noise.

use std::collections::HashMap;

use kagari_base::{NodeId, Rect, SharedString};

use crate::arena::Arena;

/// The synthetic root node of the derived accesskit tree (a window). Arena [`NodeId`]s transport a
/// slotmap key whose packed value is always `≥ 2^32` (the first generation is 1, in the high 32 bits),
/// so `0` never collides with a real element node and is a safe reserved id for the root.
const A11Y_ROOT: accesskit::NodeId = accesskit::NodeId(0);

/// A kagari-owned accessibility role. Maps to an [`accesskit::Role`] internally; `accesskit` never
/// appears on the public surface. `#[non_exhaustive]` so more roles can be added without a break.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum Role {
    /// A clickable button.
    Button,
    /// A static text label.
    Label,
    /// A single-line text input.
    TextInput,
    /// A checkbox.
    CheckBox,
    /// A switch (on/off toggle) — distinct from [`CheckBox`](Role::CheckBox) so a screen reader
    /// announces "switch" rather than "checkbox".
    Switch,
    /// A radio button (one exclusive option within a [`RadioGroup`](Role::RadioGroup)).
    Radio,
    /// A group of mutually-exclusive [`Radio`](Role::Radio) buttons.
    RadioGroup,
    /// A list container (its items are [`Role::ListItem`]).
    List,
    /// An item within a [`Role::List`].
    ListItem,
    /// An image.
    Image,
    /// A hyperlink.
    Link,
    /// A generic grouping container with no more specific role.
    Group,
}

impl Role {
    /// Maps to the corresponding `accesskit` role (internal — keeps `accesskit` off the public API).
    fn to_accesskit(self) -> accesskit::Role {
        match self {
            Role::Button => accesskit::Role::Button,
            Role::Label => accesskit::Role::Label,
            Role::TextInput => accesskit::Role::TextInput,
            Role::CheckBox => accesskit::Role::CheckBox,
            Role::Switch => accesskit::Role::Switch,
            Role::Radio => accesskit::Role::RadioButton,
            Role::RadioGroup => accesskit::Role::RadioGroup,
            Role::List => accesskit::Role::List,
            Role::ListItem => accesskit::Role::ListItem,
            Role::Image => accesskit::Role::Image,
            Role::Link => accesskit::Role::Link,
            Role::Group => accesskit::Role::GenericContainer,
        }
    }
}

/// The semantic info an element carries (#67). Stored on a `Div` when any of `role`/`a11y_label`/
/// `a11y_value` is set; a label/value set without an explicit role defaults `role` to [`Role::Label`].
#[derive(Clone, Debug)]
pub(crate) struct A11y {
    pub(crate) role: Role,
    pub(crate) label: Option<SharedString>,
    pub(crate) value: Option<SharedString>,
    /// The toggle/selection state of a toggleable control (checkbox/switch/radio). `None` for elements
    /// that are not toggleable. Populated at paint from the reactive `a11y_checked` prop (not at build),
    /// so it mirrors the widget's current signal value.
    pub(crate) checked: Option<bool>,
}

impl Default for A11y {
    fn default() -> Self {
        Self {
            role: Role::Label,
            label: None,
            value: None,
            checked: None,
        }
    }
}

/// One annotated element recorded during paint: its arena id, semantic info, and absolute bounds.
struct A11yNode {
    id: NodeId,
    a11y: A11y,
    bounds: Rect,
}

/// A per-frame sink of annotated elements, recorded during the paint walk (like `HitTest`) so each
/// node's absolute laid-out `bounds` are captured. Cleared each frame by `render_tree` — it is a
/// per-frame record, not a build-once registry (RK-019). `build_update` turns the recorded set into
/// an `accesskit::TreeUpdate`.
///
/// The type is `pub` (it appears in `render_tree`'s signature, like `HitTest`), but all its methods
/// are `pub(crate)` so `accesskit` never reaches the public API — an external caller can hold one but
/// cannot record into it or read the derived tree.
#[derive(Default)]
pub struct A11yTree {
    nodes: Vec<A11yNode>,
}

impl A11yTree {
    /// Clears the recorded nodes (capacity retained), called at the start of each paint pass.
    pub(crate) fn clear(&mut self) {
        self.nodes.clear();
    }

    /// Records an annotated element at its absolute `bounds` (paint-time, RK-007). No-op-free: the
    /// caller only calls this for elements that carry semantic info.
    pub(crate) fn record(&mut self, id: NodeId, a11y: A11y, bounds: Rect) {
        self.nodes.push(A11yNode { id, a11y, bounds });
    }

    /// Builds a full `accesskit::TreeUpdate` snapshot from the recorded nodes.
    ///
    /// Each annotated node is parented to its nearest annotated arena ancestor, or to the synthetic
    /// window root when none. `focus` (the framework-focused arena node) maps to that accesskit node
    /// when it is annotated, else to the root — accesskit requires focus to name a node in the tree.
    /// A full snapshot is required on the `EventLoopProxy` path (every node, every update).
    ///
    /// Node bounds are recorded in **logical** px; accesskit expects physical px relative to the
    /// window origin, so the root carries a `scale` transform (the window's `scale_factor`) that maps
    /// every descendant's logical bounds to physical px (#67).
    pub(crate) fn build_update(
        &self,
        arena: &Arena,
        focus: Option<NodeId>,
        scale: f64,
    ) -> accesskit::TreeUpdate {
        // The set of annotated ids, to resolve nearest-annotated-ancestor parenting.
        let annotated: std::collections::HashSet<NodeId> =
            self.nodes.iter().map(|n| n.id).collect();

        // children-by-parent (accesskit ids); the root collects top-level annotated nodes.
        let mut children: HashMap<accesskit::NodeId, Vec<accesskit::NodeId>> = HashMap::new();
        let mut nodes: Vec<(accesskit::NodeId, accesskit::Node)> =
            Vec::with_capacity(self.nodes.len() + 1);

        for n in &self.nodes {
            let ak_id = to_ak_id(n.id);
            let parent = nearest_annotated_ancestor(arena, n.id, &annotated)
                .map(to_ak_id)
                .unwrap_or(A11Y_ROOT);
            children.entry(parent).or_default().push(ak_id);

            let mut node = accesskit::Node::new(n.a11y.role.to_accesskit());
            match n.a11y.role {
                // A `Role::Label` node's text content is carried in `value`, not `label` (accesskit
                // convention). A lone `a11y_label` defaults to this role, so route its text to value.
                Role::Label => {
                    if let Some(text) = n.a11y.label.as_ref().or(n.a11y.value.as_ref()) {
                        node.set_value(text.as_str());
                    }
                }
                _ => {
                    if let Some(label) = &n.a11y.label {
                        node.set_label(label.as_str());
                    }
                    if let Some(value) = &n.a11y.value {
                        node.set_value(value.as_str());
                    }
                }
            }
            // A toggleable control (checkbox/switch/radio) carries its state as accesskit `Toggled`
            // (ARIA checked semantics — radio buttons use checked, not list-style `selected`).
            if let Some(checked) = n.a11y.checked {
                node.set_toggled(accesskit::Toggled::from(checked));
            }
            node.set_bounds(to_ak_rect(n.bounds));
            nodes.push((ak_id, node));
        }

        // Attach each node's children (set after building so every child id is known).
        for (ak_id, node) in nodes.iter_mut() {
            if let Some(kids) = children.get(ak_id) {
                node.set_children(kids.clone());
            }
        }

        // The synthetic window root, holding the top-level annotated nodes. Its transform scales the
        // descendants' logical-px bounds to the physical px accesskit expects (window-origin relative).
        let mut root = accesskit::Node::new(accesskit::Role::Window);
        root.set_transform(accesskit::Affine::scale(scale));
        if let Some(kids) = children.get(&A11Y_ROOT) {
            root.set_children(kids.clone());
        }
        nodes.push((A11Y_ROOT, root));

        // Focus must name a node in the tree: the focused node if annotated, else the root.
        let focus = focus
            .filter(|id| annotated.contains(id))
            .map(to_ak_id)
            .unwrap_or(A11Y_ROOT);

        accesskit::TreeUpdate {
            nodes,
            tree: Some(accesskit::Tree::new(A11Y_ROOT)),
            tree_id: accesskit::TreeId::ROOT,
            focus,
        }
    }
}

/// Maps an arena [`NodeId`] to its accesskit id (both carry a `u64`).
fn to_ak_id(id: NodeId) -> accesskit::NodeId {
    accesskit::NodeId(id.raw())
}

/// Maps a kagari [`Rect`] (origin + size, logical px) to an accesskit `Rect` (min/max corners, f64).
fn to_ak_rect(rect: Rect) -> accesskit::Rect {
    accesskit::Rect {
        x0: rect.origin.x as f64,
        y0: rect.origin.y as f64,
        x1: (rect.origin.x + rect.size.w) as f64,
        y1: (rect.origin.y + rect.size.h) as f64,
    }
}

/// The nearest ancestor of `node` that is itself annotated, or `None` if none up to the root.
fn nearest_annotated_ancestor(
    arena: &Arena,
    node: NodeId,
    annotated: &std::collections::HashSet<NodeId>,
) -> Option<NodeId> {
    let mut cur = arena.get(node)?.parent;
    while let Some(id) = cur {
        if annotated.contains(&id) {
            return Some(id);
        }
        cur = arena.get(id).and_then(|n| n.parent);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::damage::DamageState;
    use crate::element::{IntoElement, div};
    use crate::paint::render_tree;
    use kagari_base::Size;
    use kagari_layout::LayoutTree;
    use kagari_render::Scene;
    use kagari_style::Theme;
    use kagari_text::{FontDb, TextSystem};
    use std::sync::Arc;

    /// Builds + paints `root` (GPU-free) into a fresh `A11yTree`, returning the tree + arena so the
    /// caller can `build_update`. Static divs only → no reactive effect, no owner needed (RK-005).
    fn derive(mut root: crate::element::AnyElement) -> (A11yTree, Arena) {
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let mut scene = Scene::new();
        let mut a11y = A11yTree::default();
        let damage = Arc::new(DamageState::default());
        let theme = Theme::default();
        render_tree(
            &mut root,
            &mut arena,
            &mut layout,
            &mut text,
            None,
            None,
            None,
            None,
            None,
            Some(&mut a11y),
            &mut scene,
            Size { w: 200.0, h: 200.0 },
            &damage,
            &theme,
        )
        .unwrap();
        (a11y, arena)
    }

    #[test]
    fn a11y_tree_should_derive_role_and_label() {
        let (a11y, arena) = derive(div().role(Role::Button).a11y_label("OK").into_element());
        let update = a11y.build_update(&arena, None, 1.0);

        // Root (window) + the one annotated button.
        assert_eq!(
            update.nodes.len(),
            2,
            "the window root plus one annotated node"
        );
        let (_, button) = update
            .nodes
            .iter()
            .find(|(_, n)| n.role() == accesskit::Role::Button)
            .expect("a Button node is derived");
        assert_eq!(
            button.label(),
            Some("OK"),
            "the a11y label is carried through"
        );
    }

    #[test]
    fn a11y_tree_should_nest_annotated_child_under_annotated_parent() {
        // A List with an annotated ListItem child; an unannotated wrapper div sits between them to
        // prove nearest-annotated-ancestor parenting skips unannotated nodes.
        let (a11y, arena) = derive(
            div()
                .role(Role::List)
                .child(div().child(div().role(Role::ListItem).a11y_label("row")))
                .into_element(),
        );
        let update = a11y.build_update(&arena, None, 1.0);

        let list_id = update
            .nodes
            .iter()
            .find(|(_, n)| n.role() == accesskit::Role::List)
            .map(|(id, _)| *id)
            .expect("a List node");
        let (item_id, list_node) = {
            let item = update
                .nodes
                .iter()
                .find(|(_, n)| n.role() == accesskit::Role::ListItem)
                .map(|(id, _)| *id)
                .expect("a ListItem node");
            let list = update
                .nodes
                .iter()
                .find(|(id, _)| *id == list_id)
                .map(|(_, n)| n)
                .unwrap();
            (item, list)
        };
        assert_eq!(
            list_node.children(),
            &[item_id],
            "the ListItem nests under the List despite the unannotated wrapper between them"
        );
    }

    #[test]
    fn a11y_focus_should_point_at_focused_node() {
        let (a11y, arena) = derive(div().role(Role::Button).a11y_label("OK").into_element());

        // Recover the annotated node's arena id from the derived tree: `to_ak_id` is a raw round-trip.
        let button_ak_id = a11y
            .build_update(&arena, None, 1.0)
            .nodes
            .iter()
            .find(|(_, n)| n.role() == accesskit::Role::Button)
            .map(|(id, _)| *id)
            .expect("a Button node");
        let button_arena_id = NodeId::from_raw(button_ak_id.0);

        let update = a11y.build_update(&arena, Some(button_arena_id), 1.0);
        assert_eq!(
            update.focus, button_ak_id,
            "focus resolves to the focused annotated node"
        );

        // No focus falls back to the root.
        let update_none = a11y.build_update(&arena, None, 1.0);
        assert_eq!(
            update_none.focus, A11Y_ROOT,
            "no focus falls back to the root"
        );
    }

    #[test]
    fn a11y_value_should_be_carried_through() {
        // A non-Label role carries `a11y_value` in the node's `value` property.
        let (a11y, arena) = derive(
            div()
                .role(Role::TextInput)
                .a11y_value("hello")
                .into_element(),
        );
        let update = a11y.build_update(&arena, None, 1.0);
        let (_, node) = update
            .nodes
            .iter()
            .find(|(_, n)| n.role() == accesskit::Role::TextInput)
            .expect("a TextInput node");
        assert_eq!(
            node.value(),
            Some("hello"),
            "the a11y value is carried through"
        );
    }

    #[test]
    fn a11y_tree_should_omit_unannotated_div() {
        // A tree with no annotated element derives only the synthetic window root (sparse tree).
        let (a11y, arena) = derive(div().child(div()).into_element());
        let update = a11y.build_update(&arena, None, 1.0);
        assert_eq!(
            update.nodes.len(),
            1,
            "only the synthetic window root; no annotated element"
        );
    }

    #[test]
    fn a11y_should_map_radio_group_and_switch_roles() {
        // The #257 roles map to their accesskit kinds (a RadioGroup > Radio, and a sibling Switch).
        let (a11y, arena) = derive(
            div()
                .child(
                    div()
                        .role(Role::RadioGroup)
                        .child(div().role(Role::Radio).a11y_label("opt")),
                )
                .child(div().role(Role::Switch).a11y_label("sw"))
                .into_element(),
        );
        let update = a11y.build_update(&arena, None, 1.0);
        let has = |r: accesskit::Role| update.nodes.iter().any(|(_, n)| n.role() == r);
        assert!(has(accesskit::Role::RadioGroup), "RadioGroup role maps");
        assert!(
            has(accesskit::Role::RadioButton),
            "Radio maps to RadioButton"
        );
        assert!(has(accesskit::Role::Switch), "Switch role maps");
    }

    #[test]
    fn a11y_should_carry_checked_state() {
        // A static `a11y_checked` propagates to the node's accesskit `Toggled` (checked semantics).
        let (on, arena_on) = derive(div().role(Role::CheckBox).a11y_checked(true).into_element());
        let up_on = on.build_update(&arena_on, None, 1.0);
        let (_, cb_on) = up_on
            .nodes
            .iter()
            .find(|(_, n)| n.role() == accesskit::Role::CheckBox)
            .expect("a CheckBox node");
        assert_eq!(
            cb_on.toggled(),
            Some(accesskit::Toggled::True),
            "checked → Toggled::True"
        );

        let (off, arena_off) = derive(
            div()
                .role(Role::CheckBox)
                .a11y_checked(false)
                .into_element(),
        );
        let up_off = off.build_update(&arena_off, None, 1.0);
        let (_, cb_off) = up_off
            .nodes
            .iter()
            .find(|(_, n)| n.role() == accesskit::Role::CheckBox)
            .expect("a CheckBox node");
        assert_eq!(
            cb_off.toggled(),
            Some(accesskit::Toggled::False),
            "unchecked → Toggled::False"
        );
    }

    #[test]
    fn a11y_checked_should_update_on_signal() {
        // A reactive `a11y_checked` re-resolves at paint, so a signal write flips the announced state
        // (this is the whole point of #257 — the a11y state mirrors the widget signal). RK-005: serial
        // + owner alive.
        use crate::reactive::prelude::*;
        use crate::reactive::{Owner, RwSignal, rx};

        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(false);
        let mut root = div()
            .role(Role::CheckBox)
            .a11y_checked(rx(move || value.get()))
            .into_element();
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let damage = Arc::new(DamageState::default());
        let theme = Theme::default();

        let mut toggled = || {
            let mut a11y = A11yTree::default();
            let mut scene = Scene::new();
            render_tree(
                &mut root,
                &mut arena,
                &mut layout,
                &mut text,
                None,
                None,
                None,
                None,
                None,
                Some(&mut a11y),
                &mut scene,
                Size { w: 50.0, h: 50.0 },
                &damage,
                &theme,
            )
            .unwrap();
            a11y.build_update(&arena, None, 1.0)
                .nodes
                .iter()
                .find(|(_, n)| n.role() == accesskit::Role::CheckBox)
                .and_then(|(_, n)| n.toggled())
        };

        assert_eq!(
            toggled(),
            Some(accesskit::Toggled::False),
            "frame 1: unchecked"
        );
        value.set(true);
        assert_eq!(
            toggled(),
            Some(accesskit::Toggled::True),
            "frame 2: checked after the signal write"
        );
        drop(owner);
    }

    #[test]
    fn a11y_label_without_role_should_default_to_label_value() {
        // A lone `a11y_label` (no explicit role) exposes a `Role::Label` node whose text is carried
        // in `value` (accesskit's convention for label-role nodes), not `label`.
        let (a11y, arena) = derive(div().a11y_label("name").into_element());
        let update = a11y.build_update(&arena, None, 1.0);
        let (_, node) = update
            .nodes
            .iter()
            .find(|(_, n)| n.role() == accesskit::Role::Label)
            .expect("a Label node exposed by a lone a11y_label");
        assert_eq!(
            node.value(),
            Some("name"),
            "a Label node's text is carried in value, not label"
        );
    }
}
