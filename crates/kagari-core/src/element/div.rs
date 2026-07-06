//! The [`Div`] element: a flex container with children and raw style props (#31).

use std::sync::{Arc, Mutex};

use kagari_base::{Color, Corners, Edges, NodeId, Point, Px, Rect, SharedString, Size};
use kagari_layout::{AlignItems, FlexDirection, JustifyContent, LayoutStyle};
use kagari_render::{Background, Border, Quad, RoundedRect, Shadow};
use kagari_style::{ColorRole, SpacingStep, Style, Styled, Theme};

use crate::a11y::{A11y, Role};

use super::reactive_prop::ReactiveProp;
use super::{
    AnchorHandle, AnyElement, DamageSink, Element, Event, EventCx, IntoElement, LayoutCx, PaintCx,
};
use crate::arena::Node;
use crate::event::{
    Action, ActionHandler, CursorIcon, DragHandler, DragPayload, DragSource, DropTarget,
    FocusHandle, FocusId, GestureEvent, GestureHandler, HitRegion, InteractFlags, KeyEvent,
    KeyHandler, KeyListenerKind, ListenerKind, MouseEvent, MouseHandler,
};
use crate::reactive::{Prop, create_effect};

/// A flex container element: children plus style props.
///
/// Styling is via the [`Styled`] token API (`gap_2`/`p_4`/`rounded_lg`/`bg(role)`, #41), recorded
/// into `style` and resolved against the theme at layout/paint (#42). The `background` escape hatch
/// keeps the raw/reactive [`Background`] path (#31/#35): a static value resolves once; a reactive
/// closure binds a synchronous effect that re-resolves it and flags damage (reactive token props
/// are #146).
pub struct Div {
    children: Vec<AnyElement>,
    layout: LayoutStyle,
    /// Recorded Tailwind-style tokens (resolved against the theme at layout/paint).
    style: Style,
    /// Background (raw color): a reactive prop resolved to a paint slot (#251). Read by `paint`.
    bg: ReactiveProp<Background>,
    /// Assigned on the first `request_layout`; the build-once rebuild lifecycle is #32.
    id: Option<NodeId>,
    /// Child node ids in `children` order, assigned at build; `paint` reads each child's bounds
    /// from the layout tree by id.
    child_ids: Vec<NodeId>,
    /// Semantic background **role** (static or reactive, #245) — the theme-reactive counterpart of
    /// `bg`: the effect resolves the *role* (not the color) and flags paint-dirty; `paint` resolves
    /// role → color via `cx.theme`, so it follows both a signal (state) and a theme swap. Takes
    /// precedence over the raw `bg` when set. Bound by [`Styled::bg`]-style `.bg(role)` / `.bg(rx(..))`.
    bg_role: ReactiveProp<ColorRole>,
    /// Semantic border-color **role**, *optional* so it can vanish (e.g. a focus ring shown only while
    /// focused): the resolved value is `Option<ColorRole>` — `None` = no border this frame. Paired with
    /// the static `border_width` token; resolved to a color at paint via `cx.theme`. Paint-dirty (#245).
    border_role: ReactiveProp<Option<ColorRole>>,
    /// Raw/reactive fixed size (#146). A reactive size's bind effect flags **layout-dirty** (vs `bg`'s
    /// paint-dirty), so a signal-driven size change triggers relayout.
    size: Option<Prop<Size>>,
    /// Resolved fixed size: written by the bound reactive effect (or once, for a static prop), read
    /// by `effective_layout`. Shared + `'static` so the effect can own a handle to it.
    resolved_size: Arc<Mutex<Option<Size>>>,
    /// The layout style last applied to taffy (resolved against the theme). Compared each build so
    /// `set_style` re-runs only when the resolved value changes — on first build, after a theme
    /// swap (#43), and after a reactive layout prop changes (#146), but not on idle frames.
    applied_layout: Option<LayoutStyle>,
    /// Mouse listeners (#48), tagged by kind, run during dispatch (capture+bubble / hover). These
    /// are imperative `FnMut` callbacks that write to signals (design.md §3) — not reactive effects
    /// (RK-005 N/A) — and drop with this `Div` when its node is removed (RK-006).
    mouse_handlers: Vec<(ListenerKind, MouseHandler)>,
    /// The focus identity bound by [`track_focus`](Self::track_focus) (#49), registered with the
    /// focus registry at build (FocusId → this node) so keyboard dispatch / focus-within can resolve
    /// the focused node. `None` for non-focusable elements.
    focus_id: Option<FocusId>,
    /// Keyboard listeners (#49), tagged by press/release, run as a key bubbles the focus chain.
    key_handlers: Vec<(KeyListenerKind, KeyHandler)>,
    /// Action listeners (#50), run as a resolved `Action` bubbles the focus chain. Each fires for any
    /// `Action`; the handler matches the one it cares about.
    action_handlers: Vec<ActionHandler>,
    /// Gesture listeners (#51), run as a recognized `GestureEvent` (pan/zoom) bubbles the hit path.
    gesture_handlers: Vec<GestureHandler>,
    /// Drag source (#52): produces a typed payload when a drag begins from this node.
    drag_source: Option<DragSource>,
    /// Drop target (#52): accepts a payload of a specific type and runs its handler.
    drop_target: Option<DropTarget>,
    /// Drag-over listeners (#178): run while an accepting drag hovers this drop target (hover feedback).
    drag_over_handlers: Vec<DragHandler>,
    /// Drag-leave listeners (#178): run when a hovering drag leaves this drop target.
    drag_leave_handlers: Vec<DragHandler>,
    /// The cursor declared by [`cursor`](Self::cursor) (#53), registered with the cursor registry at
    /// build (this node → icon) so pointer-move resolution can pick it. `None` declares nothing.
    cursor: Option<CursorIcon>,
    /// The anchor handle bound by [`anchor_ref`](Self::anchor_ref) (#175): this node records its
    /// `NodeId` into the handle at build so an overlay can anchor to it. `None` = not an anchor.
    anchor_handle: Option<AnchorHandle>,
    /// Optional accessibility info (#67): `role`/`a11y_label`/`a11y_value`. Recorded into the a11y
    /// sink at paint (with the node's absolute bounds) when set, so the derived accesskit tree carries
    /// this element. `None` = not exposed to accessibility.
    a11y: Option<A11y>,
    /// The reactive toggle/selection state (#257): a static or reactive `bool` resolved at paint into
    /// the recorded a11y state, so a control's a11y state mirrors its signal (checkbox/switch/radio).
    a11y_checked: ReactiveProp<bool>,
}

/// Creates an empty [`Div`].
pub fn div() -> Div {
    Div {
        children: Vec::new(),
        layout: LayoutStyle::default(),
        style: Style::default(),
        bg: ReactiveProp::default(),
        id: None,
        child_ids: Vec::new(),
        bg_role: ReactiveProp::default(),
        border_role: ReactiveProp::default(),
        size: None,
        resolved_size: Arc::new(Mutex::new(None)),
        applied_layout: None,
        mouse_handlers: Vec::new(),
        focus_id: None,
        key_handlers: Vec::new(),
        action_handlers: Vec::new(),
        gesture_handlers: Vec::new(),
        drag_source: None,
        drop_target: None,
        drag_over_handlers: Vec::new(),
        drag_leave_handlers: Vec::new(),
        cursor: None,
        anchor_handle: None,
        a11y: None,
        a11y_checked: ReactiveProp::default(),
    }
}

impl Styled for Div {
    fn style_mut(&mut self) -> &mut Style {
        &mut self.style
    }
}

impl Div {
    /// Appends a child element.
    pub fn child(mut self, child: impl IntoElement) -> Self {
        self.children.push(child.into_element());
        self
    }

    /// Appends several child elements.
    pub fn children<I>(mut self, children: I) -> Self
    where
        I: IntoIterator,
        I::Item: IntoElement,
    {
        self.children
            .extend(children.into_iter().map(IntoElement::into_element));
        self
    }

    /// Sets a raw/reactive [`Background`] (static value or reactive closure via [`Prop`]) — the
    /// escape hatch beside the semantic [`Styled::bg`](Styled::bg) token setter. Use this for a
    /// literal color/gradient or a signal-driven background; use `.bg(role)` for theme tokens.
    /// **Damage kind: paint-dirty** (appearance only, no relayout — contrast [`size`](Self::size)).
    pub fn background(mut self, bg: impl Into<Prop<Background>>) -> Self {
        self.bg.set(bg);
        self
    }

    /// Sets the semantic background **role** — a static token or a reactive [`Prop`] (#245). Shadows
    /// the static [`Styled::bg`] with a reactive-capable form: `.bg(ColorRole::Accent)` resolves once,
    /// `.bg(rx(move || if h.is_focused() { .. } else { .. }))` re-resolves on the signal. The role
    /// (not a raw color) is stored, so a theme swap still re-resolves it at paint. **Damage: paint-dirty.**
    pub fn bg(mut self, role: impl Into<Prop<ColorRole>>) -> Self {
        self.bg_role.set(role);
        self
    }

    /// Sets an *optional* semantic border-color **role** — a static token or a reactive [`Prop`] (#245),
    /// where `None` means no border this frame (e.g. a focus ring shown only while focused:
    /// `.border_color(rx(move || h.is_focused().then_some(ColorRole::FocusRing)))`). Pair with a
    /// `border_w_*` width token. The border is drawn inset (reusing the quad's per-edge border), so it
    /// costs no layout and shifts no content. Takes **precedence** over the always-on
    /// [`Styled::border`](kagari_style::Styled::border) when both are set. **Damage: paint-dirty.**
    pub fn border_color(mut self, role: impl Into<Prop<Option<ColorRole>>>) -> Self {
        self.border_role.set(role);
        self
    }

    /// Lays children out in a row (the flex default). Damage kind: static layout (build-time only).
    pub fn flex(mut self) -> Self {
        self.layout.flex_direction = FlexDirection::Row;
        self
    }

    /// Lays children out in a column. Damage kind: static layout (build-time only).
    pub fn flex_col(mut self) -> Self {
        self.layout.flex_direction = FlexDirection::Column;
        self
    }

    /// Sets the gap between children. Damage kind: static layout (a reactive `gap` is a future
    /// extension of the #146 reactive-size path).
    pub fn gap(mut self, gap: Px) -> Self {
        self.layout.gap = gap;
        self
    }

    /// Aligns children to the start of the cross axis (`align-items: flex-start`) — so children size to
    /// their content instead of stretching to fill the cross axis. Damage kind: static layout.
    pub fn items_start(mut self) -> Self {
        self.layout.align_items = Some(AlignItems::Start);
        self
    }

    /// Centers children on the cross axis (`align-items: center`). Damage kind: static layout.
    pub fn items_center(mut self) -> Self {
        self.layout.align_items = Some(AlignItems::Center);
        self
    }

    /// Centers children on the main axis (`justify-content: center`). Damage kind: static layout.
    pub fn justify_center(mut self) -> Self {
        self.layout.justify_content = Some(JustifyContent::Center);
        self
    }

    /// Sets a fixed size — a static value or a reactive [`Prop`] (#146). **Damage kind: layout-dirty**:
    /// a reactive size's bind effect re-applies the layout style and marks the node layout-dirty, so a
    /// signal-driven size change triggers relayout (contrast [`background`](Self::background), which is
    /// paint-dirty). A static `Size` resolves once.
    pub fn size(mut self, size: impl Into<Prop<Size>>) -> Self {
        self.size = Some(size.into());
        self
    }

    /// Sets the minimum width (logical px). Notably `min-width: 0` lets a flex child shrink **below** its
    /// content width — required for a nested wrapping text to break to its container (flexbox's default
    /// `min-width: auto` = min-content otherwise keeps the item at its content width). Per-axis: it does
    /// **not** affect `min-height`. #260.
    pub fn min_w(mut self, min_w: f32) -> Self {
        self.layout.min_width = Some(min_w);
        self
    }

    /// Registers a mouse-button-press handler (#48). Bubble-phase: it runs as the event bubbles
    /// from the target up through this node. The handler writes to signals (design.md §3); call
    /// [`EventCx::stop_propagation`] to halt bubbling or [`EventCx::capture_pointer`] to start a drag
    /// grab. Adding any handler makes this node hit-testable (recorded into the `HitTest` at paint).
    pub fn on_mouse_down(
        mut self,
        handler: impl FnMut(&MouseEvent, &mut EventCx) + 'static,
    ) -> Self {
        self.mouse_handlers
            .push((ListenerKind::Down, Box::new(handler)));
        self
    }

    /// Registers a mouse-button-release handler (#48); see [`on_mouse_down`](Self::on_mouse_down).
    pub fn on_mouse_up(mut self, handler: impl FnMut(&MouseEvent, &mut EventCx) + 'static) -> Self {
        self.mouse_handlers
            .push((ListenerKind::Up, Box::new(handler)));
        self
    }

    /// Registers a click handler — a press and release of the same button on this node (#48).
    pub fn on_click(mut self, handler: impl FnMut(&MouseEvent, &mut EventCx) + 'static) -> Self {
        self.mouse_handlers
            .push((ListenerKind::Click, Box::new(handler)));
        self
    }

    /// Registers a pointer-move handler (#48).
    pub fn on_mouse_move(
        mut self,
        handler: impl FnMut(&MouseEvent, &mut EventCx) + 'static,
    ) -> Self {
        self.mouse_handlers
            .push((ListenerKind::Move, Box::new(handler)));
        self
    }

    /// Registers a wheel/scroll handler (#48); the delta is in the event's `Wheel` kind.
    pub fn on_wheel(mut self, handler: impl FnMut(&MouseEvent, &mut EventCx) + 'static) -> Self {
        self.mouse_handlers
            .push((ListenerKind::Wheel, Box::new(handler)));
        self
    }

    /// Registers a pointer-enter handler — fired when the pointer enters this node's region as the
    /// hover path changes (#48, path-based hover).
    pub fn on_mouse_enter(
        mut self,
        handler: impl FnMut(&MouseEvent, &mut EventCx) + 'static,
    ) -> Self {
        self.mouse_handlers
            .push((ListenerKind::Enter, Box::new(handler)));
        self
    }

    /// Registers a pointer-leave handler — the counterpart of [`on_mouse_enter`](Self::on_mouse_enter).
    pub fn on_mouse_leave(
        mut self,
        handler: impl FnMut(&MouseEvent, &mut EventCx) + 'static,
    ) -> Self {
        self.mouse_handlers
            .push((ListenerKind::Leave, Box::new(handler)));
        self
    }

    /// Binds this element to a [`FocusHandle`](crate::event::FocusHandle) (#49): the node becomes a
    /// focus target (tab order, `focus()`, focus-within) and keyboard events route here while focused.
    /// The `FocusId → NodeId` association is recorded with the focus registry at build.
    pub fn track_focus(mut self, handle: &FocusHandle) -> Self {
        self.focus_id = Some(handle.id());
        self
    }

    /// Registers a key-press handler (#49). Bubble-phase: runs as the key bubbles from the focused
    /// node up its ancestor chain. The handler writes to signals (design.md §3); call
    /// [`EventCx::stop_propagation`] to halt bubbling.
    pub fn on_key_down(mut self, handler: impl FnMut(&KeyEvent, &mut EventCx) + 'static) -> Self {
        self.key_handlers
            .push((KeyListenerKind::Down, Box::new(handler)));
        self
    }

    /// Registers a key-release handler (#49); see [`on_key_down`](Self::on_key_down).
    pub fn on_key_up(mut self, handler: impl FnMut(&KeyEvent, &mut EventCx) + 'static) -> Self {
        self.key_handlers
            .push((KeyListenerKind::Up, Box::new(handler)));
        self
    }

    /// Registers an action handler (#50): runs as a resolved [`Action`] bubbles from the focused node
    /// up this node's chain. The handler receives every dispatched `Action` and matches the ones it
    /// handles; call [`EventCx::stop_propagation`] to halt bubbling.
    pub fn on_action(mut self, handler: impl FnMut(&Action, &mut EventCx) + 'static) -> Self {
        self.action_handlers.push(Box::new(handler));
        self
    }

    /// Registers a gesture handler (#51): runs as a recognized [`GestureEvent`] (pan/zoom) bubbles
    /// from the hit node up this node's chain. The handler matches the gesture it cares about (e.g. a
    /// timeline scrolls on `Pan`, zooms on `Zoom`); call [`EventCx::stop_propagation`] to halt bubbling.
    pub fn on_gesture(
        mut self,
        handler: impl FnMut(&GestureEvent, &mut EventCx) + 'static,
    ) -> Self {
        self.gesture_handlers.push(Box::new(handler));
        self
    }

    /// Makes this node a drag source (#52): a press here that moves past the drag threshold begins a
    /// drag carrying the payload produced by `payload` (live flow #178). The node is hit-testable.
    pub fn drag_source<P: DragPayload>(mut self, payload: impl FnMut() -> P + 'static) -> Self {
        self.drag_source = Some(DragSource::new(payload));
        self
    }

    /// Attaches a drag-image to this node's drag source (#172): while a drag from here is active, the
    /// shell paints `image` as a cursor-tracked overlay above all content (#178). Call after
    /// [`drag_source`](Self::drag_source); a no-op if no drag source is set.
    pub fn drag_image<E: IntoElement>(mut self, image: impl FnMut() -> E + 'static) -> Self {
        if let Some(source) = self.drag_source.take() {
            self.drag_source = Some(source.with_drag_image(image));
        }
        self
    }

    /// Makes this node a drop target (#52) accepting a payload of type `T`: a matching drop runs
    /// `on_drop` with the payload; other types are rejected. While a compatible drag hovers, its
    /// [`on_drag_over`](Self::on_drag_over)/[`on_drag_leave`](Self::on_drag_leave) fire for hover
    /// feedback (live flow #178).
    pub fn drop_target<T: 'static>(
        mut self,
        on_drop: impl FnMut(T, &mut EventCx) + 'static,
    ) -> Self {
        self.drop_target = Some(DropTarget::new(on_drop));
        self
    }

    /// Runs `handler` while an **accepting** drag hovers this drop target (#178) — the live drag flow
    /// delivers drag-over only when the in-flight payload's type matches [`drop_target`](Self::drop_target).
    /// Use it to raise hover feedback (e.g. set a signal that highlights the target).
    pub fn on_drag_over(mut self, handler: impl FnMut(&mut EventCx) + 'static) -> Self {
        self.drag_over_handlers.push(Box::new(handler));
        self
    }

    /// Runs `handler` when a hovering drag leaves this drop target (#178) — the counterpart of
    /// [`on_drag_over`](Self::on_drag_over), for clearing hover feedback.
    pub fn on_drag_leave(mut self, handler: impl FnMut(&mut EventCx) + 'static) -> Self {
        self.drag_leave_handlers.push(Box::new(handler));
        self
    }

    /// Declares the cursor shown while the pointer is over this node (#53). On pointer move the topmost
    /// hit's nearest cursor-declaring node wins, so a declaration here covers this node and any
    /// descendant that declares none; outside any cursor region the cursor reverts to default. The
    /// `NodeId → CursorIcon` association is recorded with the cursor registry at build. Declaring a
    /// cursor makes this node hit-testable (recorded into the `HitTest` at paint).
    pub fn cursor(mut self, icon: CursorIcon) -> Self {
        self.cursor = Some(icon);
        self
    }

    /// Tags this element as an overlay anchor (#175): it records its `NodeId` into `handle` at build
    /// so an `overlay(...).anchor(&handle, placement)` can position relative to this element.
    pub fn anchor_ref(mut self, handle: &AnchorHandle) -> Self {
        self.anchor_handle = Some(handle.clone());
        self
    }

    /// The [`A11y`] to populate, inserting a default (role [`Role::Label`]) on first use so a lone
    /// `a11y_label`/`a11y_value` (no explicit role) still exposes the element.
    fn a11y_mut(&mut self) -> &mut A11y {
        self.a11y.get_or_insert_with(A11y::default)
    }

    /// Sets this element's accessibility [`Role`] (#67), exposing it in the derived accesskit tree.
    pub fn role(mut self, role: Role) -> Self {
        self.a11y_mut().role = role;
        self
    }

    /// Sets this element's accessibility label (#67) — the accessible name a screen reader announces.
    /// Also exposes the element (default role [`Role::Label`]) if no [`role`](Self::role) was set.
    pub fn a11y_label(mut self, label: impl Into<SharedString>) -> Self {
        self.a11y_mut().label = Some(label.into());
        self
    }

    /// Sets this element's accessibility value (#67) — e.g. a text input's contents or a control's
    /// current value. Also exposes the element (default role [`Role::Label`]) if no role was set.
    pub fn a11y_value(mut self, value: impl Into<SharedString>) -> Self {
        self.a11y_mut().value = Some(value.into());
        self
    }

    /// Sets this element's toggle/selection state (#257) — a static or reactive `bool` announced to a
    /// screen reader (checkbox/switch checked, radio selected). Reactive so it mirrors the control's
    /// signal: resolved at paint into the recorded a11y state, like the #245 role props. Also exposes
    /// the element (default role [`Role::Label`]) if no [`role`](Self::role) was set, so the recorded
    /// node carries the state.
    pub fn a11y_checked(mut self, checked: impl Into<Prop<bool>>) -> Self {
        self.a11y_mut();
        self.a11y_checked.set(checked);
        self
    }

    /// Resolves the size prop into `resolved_size` (#146), the **layout-dirty** counterpart of the
    /// paint-dirty [`ReactiveProp`](super::reactive_prop::ReactiveProp) bindings — kept separate here
    /// because a size change needs relayout, not just a repaint. A static prop writes the cell once; a reactive prop
    /// registers a synchronous effect (ADR 0001) that re-resolves the closure and — only when the
    /// value changed (design.md §9) — writes the cell and flags **layout**-damage for `id`. The new
    /// size reaches taffy on the next `request_layout` via the dedup `set_style` path (#43); the
    /// `mark_layout_dirty` call is what wakes the scheduler so that frame runs. Registered once at
    /// build, so the effect only re-fires on external signal writes (never inside `request_layout`),
    /// keeping the write serial with the frame loop (RK-009).
    fn bind_size(&mut self, damage: &Arc<dyn DamageSink>, id: NodeId) {
        match self.size.take() {
            Some(Prop::Static(size)) => self.store_size(size),
            Some(Prop::Reactive(read)) => {
                let cell = Arc::clone(&self.resolved_size);
                let damage = Arc::clone(damage);
                create_effect(move || {
                    let size = read();
                    if let Ok(mut slot) = cell.lock() {
                        if *slot != Some(size) {
                            *slot = Some(size);
                            damage.mark_layout_dirty(id);
                        }
                    }
                });
            }
            None => {}
        }
    }

    fn store_size(&self, size: Size) {
        if let Ok(mut slot) = self.resolved_size.lock() {
            *slot = Some(size);
        }
    }

    fn current_size(&self) -> Option<Size> {
        self.resolved_size.lock().ok().and_then(|slot| *slot)
    }

    /// The raw [`LayoutStyle`] overlaid with the resolved layout tokens (gap + padding) from the
    /// theme. size/margin tokens are not applied yet (LayoutStyle's `size` is a paired value and
    /// `margin` is not mapped to taffy — a follow-up).
    fn effective_layout(&self, theme: &Theme) -> LayoutStyle {
        let mut ls = self.layout.clone();
        // The reactive/static fixed size (#146) is resolved into a shared cell; overlay it so a
        // changed reactive size flows into the `LayoutStyle` the dedup `set_style` compares.
        if let Some(size) = self.current_size() {
            ls.size = Some(size);
        }
        let lt = &self.style.layout;
        if let Some(step) = lt.gap {
            ls.gap = theme.resolve_spacing(step);
        }
        // Padding: `p` sets all sides, then axis (`px`/`py`) and per-side tokens override it.
        let px = |step: Option<SpacingStep>| step.map(|s| theme.resolve_spacing(s).0);
        if let Some(v) = px(lt.p) {
            ls.padding = Edges {
                top: v,
                right: v,
                bottom: v,
                left: v,
            };
        }
        if let Some(v) = px(lt.px) {
            ls.padding.left = v;
            ls.padding.right = v;
        }
        if let Some(v) = px(lt.py) {
            ls.padding.top = v;
            ls.padding.bottom = v;
        }
        if let Some(v) = px(lt.pt) {
            ls.padding.top = v;
        }
        if let Some(v) = px(lt.pr) {
            ls.padding.right = v;
        }
        if let Some(v) = px(lt.pb) {
            ls.padding.bottom = v;
        }
        if let Some(v) = px(lt.pl) {
            ls.padding.left = v;
        }
        ls
    }
}

impl Element for Div {
    fn request_layout(&mut self, cx: &mut LayoutCx) -> NodeId {
        // Build-once: allocate this element's node on the first build (the rebuild lifecycle is
        // #32). Subsequent calls reuse the id, so the bound effect is registered only once.
        let id = match self.id {
            Some(id) => id,
            None => {
                let id = cx.arena.insert(Node::default());
                self.id = Some(id);
                cx.layout.insert(id, None);
                self.bg.bind(&cx.damage, id);
                self.bg_role.bind(&cx.damage, id);
                self.border_role.bind(&cx.damage, id);
                self.a11y_checked.bind(&cx.damage, id);
                self.bind_size(&cx.damage, id);
                // Record this focus target's FocusId → NodeId so keyboard dispatch / focus-within
                // can resolve the focused node (#49). No-op when no registry is attached (the app
                // path until live keyboard wiring lands).
                if let (Some(fid), Some(reg)) = (self.focus_id, cx.focus.as_deref_mut()) {
                    reg.register(fid, id);
                }
                // Record this node's declared cursor (#53) so pointer-move resolution can pick it.
                // No-op when no registry is attached (the app path until live cursor wiring lands).
                if let (Some(icon), Some(reg)) = (self.cursor, cx.cursor.as_deref_mut()) {
                    reg.register(id, icon);
                }
                // Record this node's id into its anchor handle (#175) so an overlay can anchor to it.
                if let Some(handle) = &self.anchor_handle {
                    handle.set(id);
                }
                id
            }
        };

        // Resolve layout tokens (gap/padding) against the *current* theme every build, but re-apply
        // to taffy only when the result changed. First build applies it; a theme swap (#43) yields a
        // different `LayoutStyle`, so `set_style` re-runs and the node relays out — without this,
        // layout tokens would stay stale after a reskin (paint tokens already re-resolve per frame).
        // The dedup keeps idle frames from re-marking taffy dirty.
        let resolved = self.effective_layout(cx.theme);
        // `LayoutStyle` is no longer `Copy`; compare by ref so `resolved` can then be stored.
        if self.applied_layout.as_ref() != Some(&resolved) {
            cx.layout.set_style(id, &resolved);
            self.applied_layout = Some(resolved);
        }

        // Recurse children and link parent/children in both the arena and the layout tree.
        let mut child_ids = Vec::with_capacity(self.children.len());
        for child in &mut self.children {
            let child_id = child.request_layout(cx);
            if let Some(node) = cx.arena.get_mut(child_id) {
                node.parent = Some(id);
            }
            child_ids.push(child_id);
        }
        if let Some(node) = cx.arena.get_mut(id) {
            node.children = child_ids.clone();
        }
        cx.layout.set_children(id, &child_ids);
        self.child_ids = child_ids;
        id
    }

    fn paint(&mut self, bounds: Rect, cx: &mut PaintCx) {
        // Corner radius from the `rounded` token (all corners equal); 0 when unset. Shared by the
        // shadow and the background quad so they round identically.
        let radius = self
            .style
            .paint
            .rounded
            .map(|step| cx.theme.resolve_radius(step).0)
            .unwrap_or(0.0);
        let corner_radii = Corners {
            tl: radius,
            tr: radius,
            br: radius,
            bl: radius,
        };

        // Drop shadow (#155): emitted first so it takes a lower painter's order and draws *behind*
        // the box (Shadow batch precedes the Quad batch). The `shadow` token resolves to a
        // `ResolvedShadow` (offset/blur/spread + linear color); unset / `None` / `Inner` (post-MVP)
        // → no shadow. Its content-mask is a no-op (huge) rect — the blur extends beyond the box.
        if let Some(shadow) = self
            .style
            .paint
            .shadow
            .and_then(|step| cx.theme.resolve_shadow(step))
        {
            // The blur extends beyond the box, so the mask is unbounded — except under a scroll
            // ancestor, where it is clipped to the visible viewport (identity when unclipped).
            let shadow_mask = cx.clip_rect(Rect::from_xywh(0.0, 0.0, 1.0e4, 1.0e4));
            let order = cx.next_order();
            cx.scene.shadows.push(Shadow {
                bounds,
                corner_radii,
                offset: Point {
                    x: shadow.offset_x.0,
                    y: shadow.offset_y.0,
                },
                blur: shadow.blur.0,
                spread: shadow.spread.0,
                color: shadow.color,
                content_mask: RoundedRect {
                    rect: shadow_mask,
                    radii: Corners::default(),
                },
                order,
            });
        }

        // Background: the semantic **role** (`bg(role)`/`bg(rx(..))` → resolved theme color at paint,
        // so it follows a state signal *and* a theme swap) takes precedence; the raw/reactive
        // `.background()` value is the fallback. `.or(self.style.paint.bg)` is a compatibility path for a
        // generic `impl Styled` caller of `Styled::bg` (which writes `paint.bg`) — the inherent
        // `Div::bg` shadows it and writes `bg_role`, so direct Div callers never hit `paint.bg`.
        let bg = self
            .bg_role
            .current()
            .or(self.style.paint.bg)
            .map(|role| Background::Solid(cx.theme.resolve_color(role)))
            .or_else(|| self.bg.current());
        // Border (#245): the optional border-color role (reactive/static; `None` = no border this
        // frame) resolved to a color, plus the `border_w_*` width token. The border is drawn inset,
        // reusing the quad's per-edge border — so its width is applied only when a color is present
        // (an unfocused focus ring adds no inset), and it costs no layout / shifts no content.
        let border_color = self
            .border_role
            .current()
            .flatten()
            .or(self.style.paint.border_color)
            .map(|role| cx.theme.resolve_color(role));
        let border_width = self
            .style
            .paint
            .border_width
            .map(|step| cx.theme.resolve_border_width(step).0)
            .unwrap_or(0.0);
        let has_border = border_width > 0.0 && border_color.is_some();
        // Emit a quad when there is a fill *or* a visible border; a border-only element paints over a
        // transparent fill (the shader draws the border band, leaving the interior transparent).
        if bg.is_some() || has_border {
            // Clip the fill to any scroll ancestor (identity when unclipped). A quad the clip fully
            // culls (empty intersection) is skipped rather than emitted with a zero-area mask —
            // but only when a clip is active, so an unclipped zero-size div still emits as before.
            let mask = cx.clip_rect(bounds);
            if cx.clip().is_none() || !mask.is_empty() {
                let order = cx.next_order();
                cx.scene.quads.push(Quad {
                    bounds,
                    corner_radii,
                    bg: bg.unwrap_or(Background::Solid(Color::TRANSPARENT)),
                    border: Border {
                        widths: if has_border {
                            Edges {
                                top: border_width,
                                right: border_width,
                                bottom: border_width,
                                left: border_width,
                            }
                        } else {
                            Edges::default()
                        },
                        color: border_color.unwrap_or(Color::TRANSPARENT),
                    },
                    content_mask: RoundedRect {
                        rect: mask,
                        radii: corner_radii,
                    },
                    // Painter's order from traversal order: the parent's quad takes a lower order
                    // than its children's primitives, so children draw on top.
                    order,
                });
            }
        }
        // Record an interactive hit region (#48) before painting children, so children take larger
        // painter orders and sit in front (a child is picked over its parent). A node is recorded when
        // it opts into any pointer interaction — a mouse handler (#48), a drag source / drop target
        // (#52), or a declared cursor (#53). `bounds` is this node's absolute rect (RK-007); the clip is
        // axis-aligned (rounded-corner hit precision is post-MVP). No-op when no hit-test is attached.
        let flags = InteractFlags {
            mouse: !self.mouse_handlers.is_empty(),
            drag_source: self.drag_source.is_some(),
            drop_target: self.drop_target.is_some(),
            cursor: self.cursor.is_some(),
            ..InteractFlags::default()
        };
        if flags != InteractFlags::default() {
            if let Some(id) = self.id {
                // Clip the hit region to any scroll ancestor so a scrolled-out node is not picked
                // (empty clip → `contains` always false). Identity (`= bounds`) when unclipped.
                let clip = cx.clip_rect(bounds);
                let order = cx.next_order();
                cx.record_hit(HitRegion {
                    node: id,
                    bounds,
                    clip,
                    order,
                    flags,
                });
            }
        }

        // Record this element's accessibility node (#67) at its absolute bounds, so the derived
        // accesskit tree carries it with laid-out geometry. No-op when this div has no a11y info or
        // no a11y sink is attached (GPU-free tests / the paths before live wiring).
        if let (Some(a11y), Some(id)) = (&self.a11y, self.id) {
            // Inject the reactive toggle state (#257) resolved this frame — `A11y::checked` is not set
            // at build, only here at paint, so it mirrors the control's current signal.
            let mut a11y = a11y.clone();
            a11y.checked = self.a11y_checked.current();
            cx.record_a11y(id, a11y, bounds);
        }

        // Paint each child at its absolute bounds. `LayoutTree::layout` returns parent-relative
        // coordinates (taffy), so offset by this node's origin to get the child's absolute rect.
        for (child, &child_id) in self.children.iter_mut().zip(&self.child_ids) {
            let local = cx.layout.layout(child_id);
            let child_bounds = Rect {
                origin: Point {
                    x: bounds.origin.x + local.origin.x,
                    y: bounds.origin.y + local.origin.y,
                },
                size: local.size,
            };
            child.paint(child_bounds, cx);
        }
    }

    fn handle_event(&mut self, ev: &Event, cx: &mut EventCx) {
        let Some(id) = self.id else {
            return;
        };
        match ev {
            Event::Mouse(me) => {
                // Descend toward the target along the dispatch path (capture phase: no MVP handlers).
                if let Some(next) = cx.next_child_on_path(id) {
                    if let Some(i) = self.child_ids.iter().position(|&c| c == next) {
                        self.children[i].handle_event(ev, cx);
                    }
                }
                // A descendant may have stopped propagation — then this node does not bubble.
                if cx.is_stopped() {
                    return;
                }
                // Bubble (or filtered enter/leave) phase: run this node's matching listeners.
                if cx.should_fire(id) {
                    cx.set_current(id);
                    let kind = me.kind;
                    for (lk, handler) in self.mouse_handlers.iter_mut() {
                        if lk.matches(kind) {
                            handler(me, cx);
                        }
                    }
                }
            }
            Event::Keyboard(ke) => {
                // Keyboard bubbles the focused node's ancestor chain (#49), same descend-then-fire
                // as the mouse path; key listeners match on the press/release state.
                if let Some(next) = cx.next_child_on_path(id) {
                    if let Some(i) = self.child_ids.iter().position(|&c| c == next) {
                        self.children[i].handle_event(ev, cx);
                    }
                }
                if cx.is_stopped() {
                    return;
                }
                if cx.should_fire(id) {
                    cx.set_current(id);
                    let pressed = ke.pressed;
                    for (lk, handler) in self.key_handlers.iter_mut() {
                        if lk.matches(pressed) {
                            handler(ke, cx);
                        }
                    }
                }
            }
            Event::Action(action) => {
                // A resolved action bubbles the focus chain (#50), same descend-then-fire as keyboard;
                // every action handler fires (it matches the action it cares about).
                if let Some(next) = cx.next_child_on_path(id) {
                    if let Some(i) = self.child_ids.iter().position(|&c| c == next) {
                        self.children[i].handle_event(ev, cx);
                    }
                }
                if cx.is_stopped() {
                    return;
                }
                if cx.should_fire(id) {
                    cx.set_current(id);
                    for handler in self.action_handlers.iter_mut() {
                        handler(action, cx);
                    }
                }
            }
            Event::Gesture(gesture) => {
                // A recognized gesture bubbles the hit path (#51), same descend-then-fire as action;
                // every gesture handler fires (it matches the pan/zoom it cares about).
                if let Some(next) = cx.next_child_on_path(id) {
                    if let Some(i) = self.child_ids.iter().position(|&c| c == next) {
                        self.children[i].handle_event(ev, cx);
                    }
                }
                if cx.is_stopped() {
                    return;
                }
                if cx.should_fire(id) {
                    cx.set_current(id);
                    for handler in self.gesture_handlers.iter_mut() {
                        handler(gesture, cx);
                    }
                }
            }
            Event::DragStart => {
                // Filtered to the source node (#178): descend to it, then it produces its payload +
                // drag-image into the cx for the driver to take.
                if let Some(next) = cx.next_child_on_path(id) {
                    if let Some(i) = self.child_ids.iter().position(|&c| c == next) {
                        self.children[i].handle_event(ev, cx);
                    }
                }
                if cx.should_fire(id) {
                    if let Some(source) = self.drag_source.as_mut() {
                        cx.produce_drag(source.payload(), source.drag_image());
                    }
                }
            }
            Event::DragOver { payload } => {
                // Filtered to the hovered node (#178): if this is a drop target accepting the payload
                // type, mark acceptance (hover feedback) and run the drag-over listeners.
                if let Some(next) = cx.next_child_on_path(id) {
                    if let Some(i) = self.child_ids.iter().position(|&c| c == next) {
                        self.children[i].handle_event(ev, cx);
                    }
                }
                if cx.should_fire(id)
                    && self
                        .drop_target
                        .as_ref()
                        .is_some_and(|t| t.accepts_type(*payload))
                {
                    cx.accept_drag();
                    cx.set_current(id);
                    for handler in self.drag_over_handlers.iter_mut() {
                        handler(cx);
                    }
                }
            }
            Event::DragLeave => {
                if let Some(next) = cx.next_child_on_path(id) {
                    if let Some(i) = self.child_ids.iter().position(|&c| c == next) {
                        self.children[i].handle_event(ev, cx);
                    }
                }
                if cx.should_fire(id) {
                    cx.set_current(id);
                    for handler in self.drag_leave_handlers.iter_mut() {
                        handler(cx);
                    }
                }
            }
            Event::Drop => {
                // Filtered to the drop node (#178): take the owned payload from the cx and deliver it.
                if let Some(next) = cx.next_child_on_path(id) {
                    if let Some(i) = self.child_ids.iter().position(|&c| c == next) {
                        self.children[i].handle_event(ev, cx);
                    }
                }
                if cx.should_fire(id) {
                    if let Some(target) = self.drop_target.as_mut() {
                        if let Some(payload) = cx.take_drop_payload() {
                            cx.set_current(id);
                            target.try_drop(payload, cx);
                        }
                    }
                }
            }
        }
    }
}

impl IntoElement for Div {
    fn into_element(self) -> AnyElement {
        Box::new(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arena::Arena;
    use crate::damage::DamageState;
    use crate::paint::render_tree;
    use crate::reactive::rx;
    use kagari_layout::LayoutTree;
    use kagari_render::Scene;
    use kagari_style::{BorderWidthStep, ColorRole};
    use kagari_text::{FontDb, TextSystem};
    use reactive_graph::owner::Owner;
    use reactive_graph::prelude::*;
    use reactive_graph::signal::signal;

    /// A small populated theme for the token-resolution tests: `Accent → #3b82f6`, `S4 → 16`,
    /// radius `Lg → 8`.
    const TEST_THEME_RON: &str = r##"(
        primitives: (
            colors: { "blue-500": "#3b82f6" },
            spacing: { S4: 16.0 },
            radius: { Lg: 8.0 },
        ),
        roles: ( colors: { Accent: "blue-500" } ),
    )"##;

    struct NoopDamage;
    impl DamageSink for NoopDamage {
        fn mark_paint_dirty(&self, _id: NodeId) {}
    }

    #[derive(Default)]
    struct RecordingDamage {
        dirtied: Mutex<Vec<NodeId>>,
    }
    impl DamageSink for RecordingDamage {
        fn mark_paint_dirty(&self, id: NodeId) {
            if let Ok(mut v) = self.dirtied.lock() {
                v.push(id);
            }
        }
    }

    #[test]
    fn div_drag_image_should_attach_to_source() {
        // `.drag_source().drag_image()` yields a source that produces the drag-image element (#172);
        // `div` (the fn) is itself an `FnMut() -> Div` factory.
        let mut d = div().drag_source(|| 7u32).drag_image(div);
        assert!(
            d.drag_source
                .as_mut()
                .and_then(|s| s.drag_image())
                .is_some(),
            "drag_image attaches to the node's drag source"
        );
        // Without a drag source first, drag_image is a documented no-op (no source, no panic).
        let no_source = div().drag_image(div);
        assert!(
            no_source.drag_source.is_none(),
            "drag_image without a drag_source is a no-op"
        );
    }

    #[test]
    fn div_child_should_build_tree() {
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let damage: Arc<dyn DamageSink> = Arc::new(NoopDamage);
        let theme = Theme::default();
        let mut cx = LayoutCx {
            arena: &mut arena,
            layout: &mut layout,
            text: &mut text,
            damage,
            theme: &theme,
            focus: None,
            cursor: None,
        };

        let mut root = div().child(div()).child(div());
        let root_id = root.request_layout(&mut cx);

        let root_node = cx.arena.get(root_id).expect("root node present");
        assert_eq!(root_node.children.len(), 2);
        for child_id in root_node.children.clone() {
            assert_eq!(
                cx.arena.get(child_id).and_then(|n| n.parent),
                Some(root_id),
                "each child's parent points back at the root"
            );
        }
    }

    #[test]
    fn into_element_should_round_trip() {
        let any: AnyElement = div().into_element();
        // Re-wrapping an AnyElement is the identity (no double box).
        let _again: AnyElement = any.into_element();
    }

    #[test]
    fn div_static_bg_should_paint_quad() {
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let damage: Arc<dyn DamageSink> = Arc::new(NoopDamage);

        let theme = Theme::default();
        let green = Background::Solid(Color::new(0.0, 1.0, 0.0, 1.0));
        let mut d = div().background(green);
        d.request_layout(&mut LayoutCx {
            arena: &mut arena,
            layout: &mut layout,
            text: &mut text,
            damage,
            theme: &theme,
            focus: None,
            cursor: None,
        });

        let mut scene = Scene::new();
        d.paint(
            Rect::default(),
            &mut PaintCx::new(&mut scene, &layout, &mut text, None, None, &theme),
        );

        assert_eq!(scene.quads.len(), 1);
        assert_eq!(scene.quads[0].bg, green);
    }

    #[test]
    fn div_reactive_bg_should_repaint_and_flag_damage() {
        // create_effect needs a current owner; keep `owner` alive for the whole test (RK-003).
        // The effect is synchronous (ImmediateEffect), so this is hang-free (RK-005).
        let owner = Owner::new();
        owner.set();

        let red = Background::Solid(Color::new(1.0, 0.0, 0.0, 1.0));
        let blue = Background::Solid(Color::new(0.0, 0.0, 1.0, 1.0));
        let (read, write) = signal(red);

        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let damage = Arc::new(RecordingDamage::default());
        let theme = Theme::default();

        let mut d = div().background(rx(move || read.get()));
        let id = d.request_layout(&mut LayoutCx {
            arena: &mut arena,
            layout: &mut layout,
            text: &mut text,
            damage: damage.clone(),
            theme: &theme,
            focus: None,
            cursor: None,
        });

        let paint = |d: &mut Div, scene: &mut Scene, text: &mut TextSystem| {
            d.paint(
                Rect::default(),
                &mut PaintCx::new(scene, &layout, text, None, None, &theme),
            );
        };

        // The effect runs once on creation: resolved bg = red, damage flagged for `id`.
        let mut scene = Scene::new();
        paint(&mut d, &mut scene, &mut text);
        assert_eq!(scene.quads[0].bg, red);
        assert!(damage.dirtied.lock().unwrap().contains(&id));
        let flags_after_build = damage.dirtied.lock().unwrap().len();

        // A signal write re-runs the effect synchronously: resolved bg = blue, re-flagged.
        write.set(blue);
        let mut scene = Scene::new();
        paint(&mut d, &mut scene, &mut text);
        assert_eq!(scene.quads[0].bg, blue);
        let dirtied = damage.dirtied.lock().unwrap();
        assert!(
            dirtied.len() > flags_after_build,
            "the write re-flags damage"
        );
        assert!(
            dirtied[flags_after_build..].contains(&id),
            "re-flag targets this node's id"
        );
        drop(dirtied);

        drop(owner);
    }

    /// Renders `root` against `theme` into a fresh `Scene` (GPU-free: no atlas).
    fn render(mut root: AnyElement, theme: &Theme) -> Scene {
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let mut scene = Scene::new();
        let damage = Arc::new(DamageState::default());
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
            None,
            &mut scene,
            Size { w: 200.0, h: 200.0 },
            &damage,
            theme,
        )
        .unwrap();
        scene
    }

    #[test]
    fn div_styled_bg_token_should_resolve_to_color() {
        let theme = Theme::from_ron(TEST_THEME_RON).unwrap();
        let scene = render(div().bg(ColorRole::Accent).into_element(), &theme);
        // `bg(Accent)` resolves through the theme (Accent → blue-500 → linear) into the quad.
        assert_eq!(scene.quads.len(), 1);
        assert_eq!(
            scene.quads[0].bg,
            Background::Solid(theme.resolve_color(ColorRole::Accent))
        );
    }

    #[test]
    fn div_styled_padding_should_offset_child() {
        let theme = Theme::from_ron(TEST_THEME_RON).unwrap();
        // Parent has padding p_4 (→16px) and no bg; the green child is inset by the padding.
        let green = Background::Solid(Color::new(0.0, 1.0, 0.0, 1.0));
        let scene = render(
            div().p_4().child(div().background(green)).into_element(),
            &theme,
        );
        assert_eq!(scene.quads.len(), 1, "only the child has a bg");
        assert_eq!(scene.quads[0].bounds.origin.x, 16.0);
        assert_eq!(scene.quads[0].bounds.origin.y, 16.0);
    }

    #[test]
    fn div_styled_rounded_should_set_corner_radii() {
        let theme = Theme::from_ron(TEST_THEME_RON).unwrap();
        let scene = render(
            div().rounded_lg().bg(ColorRole::Accent).into_element(),
            &theme,
        );
        // radius Lg → 8px, applied to all four corners.
        assert_eq!(scene.quads[0].corner_radii.tl, 8.0);
        assert_eq!(scene.quads[0].corner_radii.br, 8.0);
    }

    /// Renders the *same retained* `root` against `theme` into a fresh `Scene`, reusing the given
    /// arena/layout/text so a second call exercises the build-once + re-resolve path (a theme swap).
    fn render_retained(
        root: &mut AnyElement,
        arena: &mut Arena,
        layout: &mut LayoutTree,
        text: &mut TextSystem,
        theme: &Theme,
    ) -> Scene {
        let mut scene = Scene::new();
        let damage = Arc::new(DamageState::default());
        render_tree(
            root,
            arena,
            layout,
            text,
            None,
            None,
            None,
            None,
            None,
            None,
            &mut scene,
            Size { w: 200.0, h: 200.0 },
            &damage,
            theme,
        )
        .unwrap();
        scene
    }

    #[test]
    fn theme_swap_should_reresolve_layout_token() {
        // A column with a `gap_2` token between two 10px-tall children. The gap resolves from the
        // theme, so swapping the theme must move the second child — the layout-token reskin (#43).
        // Without re-applying the resolved style, the gap would stay baked from the first build.
        let green = Background::Solid(Color::new(0.0, 1.0, 0.0, 1.0));
        let mut root = div()
            .flex_col()
            .gap_2()
            .child(div().size(Size { w: 10.0, h: 10.0 }).background(green))
            .child(div().size(Size { w: 10.0, h: 10.0 }).background(green))
            .into_element();

        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());

        let theme_a = Theme::from_ron(
            r##"( primitives: ( colors: {}, spacing: { S2: 8.0 } ), roles: ( colors: {} ) )"##,
        )
        .unwrap();
        let theme_b = Theme::from_ron(
            r##"( primitives: ( colors: {}, spacing: { S2: 16.0 } ), roles: ( colors: {} ) )"##,
        )
        .unwrap();

        // quads[0] = first child (y0), quads[1] = second child (y = 10 + gap).
        let scene = render_retained(&mut root, &mut arena, &mut layout, &mut text, &theme_a);
        assert_eq!(
            scene.quads[1].bounds.origin.y, 18.0,
            "second child sits below the first (10) plus the resolved gap S2=8"
        );

        // Swap the theme and re-render the SAME retained tree: the gap token re-resolves to 16.
        let scene = render_retained(&mut root, &mut arena, &mut layout, &mut text, &theme_b);
        assert_eq!(
            scene.quads[1].bounds.origin.y, 26.0,
            "after the swap the gap re-resolves to S2=16 → reskin without a rebuild"
        );
    }

    #[test]
    fn theme_swap_should_reresolve_paint_token() {
        // `bg(Accent)` resolves per frame, so a retained div reskins its background on a swap (#43).
        let mut root = div().bg(ColorRole::Accent).into_element();
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());

        let theme_a = Theme::from_ron(
            r##"( primitives: ( colors: { "blue-500": "#3b82f6" } ), roles: ( colors: { Accent: "blue-500" } ) )"##,
        )
        .unwrap();
        let theme_b = Theme::from_ron(
            r##"( primitives: ( colors: { "red-500": "#ef4444" } ), roles: ( colors: { Accent: "red-500" } ) )"##,
        )
        .unwrap();

        let scene = render_retained(&mut root, &mut arena, &mut layout, &mut text, &theme_a);
        assert_eq!(
            scene.quads[0].bg,
            Background::Solid(theme_a.resolve_color(ColorRole::Accent))
        );

        let scene = render_retained(&mut root, &mut arena, &mut layout, &mut text, &theme_b);
        assert_eq!(
            scene.quads[0].bg,
            Background::Solid(theme_b.resolve_color(ColorRole::Accent)),
            "the background re-resolves to the swapped theme's Accent"
        );
        assert_ne!(
            theme_a.resolve_color(ColorRole::Accent),
            theme_b.resolve_color(ColorRole::Accent),
            "the two themes' Accent differ, so the swap is observable"
        );
    }

    #[test]
    fn div_reactive_size_should_relayout_and_flag_layout_dirty() {
        // The element-API counterpart of #35's `layout_dirty_should_trigger_relayout` (#146): a
        // reactive `.size` write must mark the node *layout*-dirty and relayout on the next frame.
        // Synchronous `ImmediateEffect` → hang-free; keep `owner` alive (RK-003/005). The size signal
        // is written *between* frames (serial with `render_tree`), never during it (RK-009).
        let owner = Owner::new();
        owner.set();

        let (read, write) = signal(Size { w: 40.0, h: 20.0 });
        let green = Background::Solid(Color::new(0.0, 1.0, 0.0, 1.0));
        let mut root = div()
            .size(rx(move || read.get()))
            .background(green)
            .into_element();

        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let theme = Theme::default();
        // One shared damage across both frames: the bound effect captured it at build (frame 1).
        let damage = Arc::new(DamageState::default());
        let viewport = Size { w: 200.0, h: 200.0 };

        // Frame 1: lays out at the initial size A.
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
            None,
            &mut scene,
            viewport,
            &damage,
            &theme,
        )
        .unwrap();
        assert_eq!(scene.quads.len(), 1, "the div paints one bg quad");
        assert_eq!(
            scene.quads[0].bounds.size.w, 40.0,
            "frame 1 width is size A"
        );
        assert_eq!(
            scene.quads[0].bounds.size.h, 20.0,
            "frame 1 height is size A"
        );

        // Write a new size between frames: the effect re-runs and flags layout-dirty (not paint).
        write.set(Size { w: 80.0, h: 50.0 });
        assert!(
            damage.is_dirty(),
            "the reactive size write flags damage (layout-dirty)"
        );

        // Frame 2: the dedup `set_style` re-applies the new size → relayout to size B.
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
            None,
            &mut scene,
            viewport,
            &damage,
            &theme,
        )
        .unwrap();
        assert_eq!(
            scene.quads[0].bounds.size.w, 80.0,
            "frame 2 relays out to size B width"
        );
        assert_eq!(
            scene.quads[0].bounds.size.h, 50.0,
            "frame 2 relays out to size B height"
        );

        drop(owner);
    }

    #[test]
    fn div_shadow_should_emit_behind_quad() {
        // `div().shadow_md().bg(role)` emits a shadow primitive *behind* the bg quad (#155). The
        // built-in light theme (#45) carries the shadow scale, so `shadow_md` resolves.
        let theme = Theme::light();
        let scene = render(
            div().shadow_md().bg(ColorRole::Surface).into_element(),
            &theme,
        );
        assert_eq!(scene.shadows.len(), 1, "the div emits one shadow");
        assert_eq!(scene.quads.len(), 1, "and one background quad");
        assert!(
            scene.shadows[0].order < scene.quads[0].order,
            "shadow (order {}) draws behind the quad (order {})",
            scene.shadows[0].order,
            scene.quads[0].order
        );
        // Resolved from the Md spec (blur 6, offset_y 4, spread -1).
        assert_eq!(scene.shadows[0].blur, 6.0);
        assert_eq!(scene.shadows[0].offset.y, 4.0);
        assert_eq!(scene.shadows[0].spread, -1.0);
    }

    #[test]
    fn paint_should_record_hit_region_for_interactive_div() {
        // A div with a mouse handler records a HitRegion (#48); a handler-less sibling records none.
        // `on_click` makes the node interactive; the recorded region picks at its painted bounds.
        use crate::event::HitTest;

        let theme = Theme::default();
        let green = Background::Solid(Color::new(0.0, 1.0, 0.0, 1.0));
        let mut root = div()
            .flex_col()
            .child(div().size(Size { w: 20.0, h: 20.0 }).on_click(|_e, _c| {}))
            .child(div().size(Size { w: 20.0, h: 20.0 }).background(green))
            .into_element();

        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let mut scene = Scene::new();
        let mut hit = HitTest::new();
        let damage = Arc::new(DamageState::default());
        render_tree(
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
            Size { w: 200.0, h: 200.0 },
            &damage,
            &theme,
        )
        .unwrap();

        // The interactive child is the first 20x20 row → a point inside it hits its node; the
        // non-interactive sibling (below it) records nothing, so a point there misses.
        assert!(
            hit.pick(Point::new(10.0, 10.0)).is_some(),
            "the interactive div records a pickable hit region"
        );
        assert!(
            hit.pick(Point::new(10.0, 30.0)).is_none(),
            "the handler-less sibling records no hit region"
        );
    }

    #[test]
    fn div_cursor_should_register_and_record_hit() {
        // A div declaring a cursor registers `NodeId → CursorIcon` at build and records a hit region at
        // paint (it is interactive via its cursor), so pointer-move resolution picks it: the declared
        // Pointer over the node, the default outside it.
        use crate::event::{CursorIcon, CursorRegistry, HitTest};

        let theme = Theme::default();
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let damage: Arc<dyn DamageSink> = Arc::new(NoopDamage);
        let mut reg = CursorRegistry::new();

        let mut d = div()
            .size(Size { w: 20.0, h: 20.0 })
            .cursor(CursorIcon::Pointer);
        let id = d.request_layout(&mut LayoutCx {
            arena: &mut arena,
            layout: &mut layout,
            text: &mut text,
            damage,
            theme: &theme,
            focus: None,
            cursor: Some(&mut reg),
        });
        layout.compute(id, Size { w: 200.0, h: 200.0 }).unwrap();

        let mut scene = Scene::new();
        let mut hit = HitTest::new();
        let bounds = layout.layout(id);
        d.paint(
            bounds,
            &mut PaintCx::new(&mut scene, &layout, &mut text, None, Some(&mut hit), &theme),
        );

        assert_eq!(
            reg.resolve(&hit, &arena, Point::new(10.0, 10.0)),
            CursorIcon::Pointer,
            "the declared cursor is resolved while the pointer is over the node"
        );
        assert_eq!(
            reg.resolve(&hit, &arena, Point::new(50.0, 50.0)),
            CursorIcon::Default,
            "outside the cursor region the cursor reverts to default"
        );
    }

    #[test]
    fn div_should_emit_border_from_role_and_width_token() {
        // A `.border(role).border_w_2()` resolves the width token + color role at paint into the quad's
        // per-edge border (#245): the shader draws the inset border band from these.
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let damage: Arc<dyn DamageSink> = Arc::new(NoopDamage);
        let theme = Theme::light();
        let bw = theme.resolve_border_width(BorderWidthStep::B2).0;
        assert!(bw > 0.0, "the light theme defines a non-zero B2 width");

        let mut d = div()
            .bg(ColorRole::Surface)
            .border(ColorRole::Border)
            .border_w_2();
        d.request_layout(&mut LayoutCx {
            arena: &mut arena,
            layout: &mut layout,
            text: &mut text,
            damage,
            theme: &theme,
            focus: None,
            cursor: None,
        });

        let mut scene = Scene::new();
        d.paint(
            Rect::default(),
            &mut PaintCx::new(&mut scene, &layout, &mut text, None, None, &theme),
        );

        assert_eq!(scene.quads.len(), 1);
        let q = &scene.quads[0];
        assert_eq!(q.border.widths.top, bw);
        assert_eq!(q.border.widths.left, bw);
        assert_eq!(q.border.color, theme.resolve_color(ColorRole::Border));
    }

    #[test]
    fn div_border_only_should_emit_quad() {
        // A border with no background still emits a quad — the interior is transparent, the shader
        // draws only the border band (#245).
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let damage: Arc<dyn DamageSink> = Arc::new(NoopDamage);
        let theme = Theme::light();

        let mut d = div().border(ColorRole::Border).border_w_2();
        d.request_layout(&mut LayoutCx {
            arena: &mut arena,
            layout: &mut layout,
            text: &mut text,
            damage,
            theme: &theme,
            focus: None,
            cursor: None,
        });

        let mut scene = Scene::new();
        d.paint(
            Rect::default(),
            &mut PaintCx::new(&mut scene, &layout, &mut text, None, None, &theme),
        );

        assert_eq!(scene.quads.len(), 1, "a border-only div still emits a quad");
        assert_eq!(scene.quads[0].bg, Background::Solid(Color::TRANSPARENT));
        assert_eq!(
            scene.quads[0].border.color,
            theme.resolve_color(ColorRole::Border)
        );
    }

    #[test]
    fn div_reactive_bg_role_should_repaint_on_signal() {
        // A reactive `.bg(rx(..))` re-resolves the *role* on a signal write, flags paint-dirty, and the
        // next paint resolves the new role → color via the theme (#245). Synchronous effect (RK-003/005);
        // keep `owner` alive; the write is between frames (RK-009).
        let owner = Owner::new();
        owner.set();

        let (read, write) = signal(false);
        let mut root = div()
            .bg(rx(move || {
                if read.get() {
                    ColorRole::Accent
                } else {
                    ColorRole::Surface
                }
            }))
            .size(Size { w: 10.0, h: 10.0 })
            .into_element();

        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let theme = Theme::default();
        let damage = Arc::new(DamageState::default());
        let viewport = Size { w: 50.0, h: 50.0 };
        let render = |root: &mut AnyElement,
                      arena: &mut Arena,
                      layout: &mut LayoutTree,
                      text: &mut TextSystem,
                      damage: &Arc<DamageState>,
                      theme: &Theme|
         -> Scene {
            let mut scene = Scene::new();
            render_tree(
                root, arena, layout, text, None, None, None, None, None, None, &mut scene,
                viewport, damage, theme,
            )
            .unwrap();
            scene
        };

        let scene = render(
            &mut root,
            &mut arena,
            &mut layout,
            &mut text,
            &damage,
            &theme,
        );
        assert_eq!(
            scene.quads[0].bg,
            Background::Solid(theme.resolve_color(ColorRole::Surface)),
            "frame 1 (signal=false) paints the Surface role"
        );

        write.set(true);
        assert!(
            damage.is_dirty(),
            "the reactive bg-role write flags paint damage"
        );

        let scene = render(
            &mut root,
            &mut arena,
            &mut layout,
            &mut text,
            &damage,
            &theme,
        );
        assert_eq!(
            scene.quads[0].bg,
            Background::Solid(theme.resolve_color(ColorRole::Accent)),
            "frame 2 (signal=true) paints the Accent role"
        );
        drop(owner);
    }

    #[test]
    fn div_reactive_border_role_should_toggle_on_signal() {
        // The focus-ring pattern (#245): an optional reactive border color — `None` while the signal is
        // false (no quad, since there is no bg either), a FocusRing border when true.
        let owner = Owner::new();
        owner.set();

        let (read, write) = signal(false);
        let mut root = div()
            .border_w_2()
            .border_color(rx(move || read.get().then_some(ColorRole::FocusRing)))
            .size(Size { w: 10.0, h: 10.0 })
            .into_element();

        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let theme = Theme::light();
        let damage = Arc::new(DamageState::default());
        let viewport = Size { w: 50.0, h: 50.0 };
        let render = |root: &mut AnyElement,
                      arena: &mut Arena,
                      layout: &mut LayoutTree,
                      text: &mut TextSystem,
                      damage: &Arc<DamageState>,
                      theme: &Theme|
         -> Scene {
            let mut scene = Scene::new();
            render_tree(
                root, arena, layout, text, None, None, None, None, None, None, &mut scene,
                viewport, damage, theme,
            )
            .unwrap();
            scene
        };

        let scene = render(
            &mut root,
            &mut arena,
            &mut layout,
            &mut text,
            &damage,
            &theme,
        );
        assert!(
            scene.quads.is_empty(),
            "unfocused (signal=false) emits no border and no quad"
        );

        write.set(true);
        let scene = render(
            &mut root,
            &mut arena,
            &mut layout,
            &mut text,
            &damage,
            &theme,
        );
        assert_eq!(
            scene.quads.len(),
            1,
            "focused (signal=true) emits the ring quad"
        );
        assert_eq!(
            scene.quads[0].border.color,
            theme.resolve_color(ColorRole::FocusRing)
        );
        assert!(scene.quads[0].border.widths.top > 0.0);
        drop(owner);
    }
}
