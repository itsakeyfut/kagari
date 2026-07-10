//! The [`Dropdown`] and [`Combobox`] widgets (#79): selection controls bound to a `RwSignal<T>`.
//!
//! Both are **self-contained** — they own their internal `open` signal and [`AnchorHandle`], so the caller
//! writes just `dropdown(value, options)` / `combobox(value, options)`. `into_element` returns a container
//! holding the **trigger** (tagged `.anchor_ref`) and an anchored **overlay** popup (Popover's shape, #81),
//! reusing the Menu #77 overlay stack (dismiss-on-outside, open-state focus save/restore).
//!
//! - [`Dropdown`]: a focusable trigger button showing the selected option's label + a chevron. Opening
//!   focuses the popup panel, which owns Arrow/Enter/Esc navigation (a `highlight` signal, initialized to the
//!   selected row); a row's click / Enter sets `value` and closes.
//! - [`Combobox`]: the trigger is a [`text_input`] filter. Focus stays in the field so
//!   typing works; the popup is passive and its rows narrow as you type (via [`dyn_list`]). Arrow/Enter/Esc
//!   **bubble** out of the editor leaf (#312) to the wrapping container, which drives the same nav/select.
//!   The filter is transient (D2): `value` changes only on an explicit pick, and the closed field shows the
//!   selected label.
//!
//! MVP scope: no option virtualization (long lists render all filtered rows — VirtualizedList #86 is
//! separate), and `Role::ComboBox`/`ListBox` a11y is a follow-up.

use std::sync::Arc;

use kagari_base::{Px, SharedString};
use kagari_core::element::{Div, dyn_if, dyn_list};
use kagari_core::reactive::prelude::*;
use kagari_core::reactive::{RwSignal, create_effect, rx};
use kagari_core::{
    AnchorHandle, AnyElement, IntoElement, KeyCode, Placement, Role, div, overlay, text,
    use_focus_handle,
};
use kagari_style::{ColorRole, Styled};

use crate::control::{ControlSize, field_frame, label_px};
use crate::text_input;

/// The option list shared into the reactive closures (`Arc` so it crosses the `Send + Sync` `rx`/effect
/// boundary and clones cheaply into the popup builders).
type Options<T> = Arc<Vec<(T, SharedString)>>;

/// The index of the option equal to `value` (by `PartialEq`), if any.
fn selected_index<T: PartialEq>(opts: &[(T, SharedString)], value: &T) -> Option<usize> {
    opts.iter().position(|(v, _)| v == value)
}

/// The label of the option equal to `value`, or empty when none matches (the placeholder shows instead).
fn selected_label<T: PartialEq>(opts: &[(T, SharedString)], value: &T) -> SharedString {
    opts.iter()
        .find(|(v, _)| v == value)
        .map(|(_, l)| l.clone())
        .unwrap_or_else(|| SharedString::from(""))
}

/// The option indices whose label contains `query` (case-insensitive substring); all indices for an empty
/// query. Used both to build the filtered popup rows and to bound keyboard navigation.
fn filter_visible<T>(opts: &[(T, SharedString)], query: &str) -> Vec<usize> {
    if query.is_empty() {
        return (0..opts.len()).collect();
    }
    let q = query.to_lowercase();
    opts.iter()
        .enumerate()
        .filter(|(_, (_, label))| label.to_lowercase().contains(&q))
        .map(|(i, _)| i)
        .collect()
}

/// The next highlighted option index stepping through the `visible` set from `current` (clamped at the
/// ends). If `current` is not visible (e.g. just filtered out), jumps to the first visible option.
fn step_visible(visible: &[usize], current: usize, down: bool) -> usize {
    if visible.is_empty() {
        return current;
    }
    match visible.iter().position(|&i| i == current) {
        Some(p) => {
            let np = if down {
                (p + 1).min(visible.len() - 1)
            } else {
                p.saturating_sub(1)
            };
            visible[np]
        }
        None => visible[0],
    }
}

/// The shared popup panel chrome (Menu's raised, bordered, shadowed column).
fn panel_frame() -> Div {
    div()
        .flex_col()
        .min_w_40()
        .py_1()
        .rounded_md()
        .bg(ColorRole::SurfaceRaised)
        .border_w_1()
        .border_color(Some(ColorRole::Border))
        .shadow_lg()
}

/// One option row: `Accent`/`AccentFg` when highlighted (by `highlight == index`), hover sets the highlight,
/// and a click runs `on_select`.
fn option_row(
    label: SharedString,
    highlight: RwSignal<usize>,
    index: usize,
    font: Px,
    on_select: impl Fn() + 'static,
) -> Div {
    div()
        .px_3()
        .py_1p5()
        .bg(rx(move || {
            if highlight.get() == index {
                ColorRole::Accent
            } else {
                ColorRole::SurfaceRaised
            }
        }))
        .on_mouse_enter(move |_, _| highlight.set(index))
        .on_click(move |_, _| on_select())
        .child(
            text(label)
                .color_role(rx(move || {
                    if highlight.get() == index {
                        ColorRole::AccentFg
                    } else {
                        ColorRole::Text
                    }
                }))
                .size(font),
        )
}

/// A muted, static trigger surface for a disabled dropdown/combobox: the selected label (or placeholder) + a
/// chevron. No editor, popup, or focus (RK-027).
fn disabled_field<T: Clone + PartialEq + Send + Sync + 'static>(
    value: RwSignal<T>,
    opts: Options<T>,
    placeholder: SharedString,
    size: ControlSize,
) -> AnyElement {
    let font = label_px(size);
    field_frame(size)
        .items_center()
        .border_color(Some(ColorRole::Border))
        .role(Role::TextInput)
        .child(
            text(rx(move || {
                let s = selected_label(&opts, &value.get());
                if s.is_empty() { placeholder.clone() } else { s }
            }))
            .color_role(ColorRole::TextMuted)
            .size(font),
        )
        .child(text("▾").color_role(ColorRole::TextMuted).size(font))
        .into_element()
}

/// A selection control: a trigger showing the current option, opening a list to pick another (#79). Build
/// with [`dropdown`]; chain `.placeholder(..)` / `.size(..)` / `.disabled(..)`. Selection is **controlled**
/// via `RwSignal<T>` (D2). Returns `impl IntoElement`; `Styled` is **not** on its surface.
pub struct Dropdown<T> {
    value: RwSignal<T>,
    options: Options<T>,
    placeholder: SharedString,
    size: ControlSize,
    disabled: bool,
}

/// Creates a medium, enabled dropdown bound to `value`, listing `options` as `(value, label)` pairs (#79).
pub fn dropdown<T: Clone + PartialEq + Send + Sync + 'static>(
    value: RwSignal<T>,
    options: Vec<(T, SharedString)>,
) -> Dropdown<T> {
    Dropdown {
        value,
        options: Arc::new(options),
        placeholder: SharedString::from(""),
        size: ControlSize::Md,
        disabled: false,
    }
}

impl<T> Dropdown<T> {
    /// Sets the text shown (muted) when `value` matches no option (e.g. an initial "nothing selected").
    pub fn placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    /// Sets the control size (D3) — drives the trigger padding and font size.
    pub fn size(mut self, size: ControlSize) -> Self {
        self.size = size;
        self
    }

    /// Disables the control: muted, display-only (no popup), inert, and not focusable.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

/// Builds the dropdown popup panel for one open cycle: a self-focused, keyboard-navigable list of option
/// rows. A fresh focus handle + highlight (initialized to the selected option) each mount.
fn build_dropdown_panel<T: Clone + PartialEq + Send + Sync + 'static>(
    opts: Options<T>,
    value: RwSignal<T>,
    open: RwSignal<bool>,
    font: Px,
) -> impl IntoElement {
    let handle = use_focus_handle();
    handle.focus();
    let highlight = RwSignal::new(selected_index(&opts, &value.get_untracked()).unwrap_or(0));
    let all: Vec<usize> = (0..opts.len()).collect();

    let nav_opts = opts.clone();
    let mut panel = panel_frame()
        .track_focus(&handle)
        .on_key_down(move |kev, _cx| {
            if kev.repeat {
                return;
            }
            match kev.code {
                KeyCode::ArrowDown => {
                    highlight.set(step_visible(&all, highlight.get_untracked(), true));
                }
                KeyCode::ArrowUp => {
                    highlight.set(step_visible(&all, highlight.get_untracked(), false));
                }
                KeyCode::Enter | KeyCode::NumpadEnter => {
                    if let Some((v, _)) = nav_opts.get(highlight.get_untracked()) {
                        value.set(v.clone());
                        open.set(false);
                    }
                }
                KeyCode::Escape => open.set(false),
                _ => {}
            }
        });

    for (i, (v, label)) in opts.iter().enumerate() {
        let v = v.clone();
        panel = panel.child(option_row(label.clone(), highlight, i, font, move || {
            value.set(v.clone());
            open.set(false);
        }));
    }
    panel
}

impl<T: Clone + PartialEq + Send + Sync + 'static> IntoElement for Dropdown<T> {
    fn into_element(self) -> AnyElement {
        let Dropdown {
            value,
            options,
            placeholder,
            size,
            disabled,
        } = self;
        let font = label_px(size);

        if disabled {
            return disabled_field(value, options, placeholder, size);
        }

        let open = RwSignal::new(false);
        let anchor = AnchorHandle::new();
        let handle = use_focus_handle();

        // Trigger: a focusable field showing the selected label (or placeholder) + a chevron.
        let ring = handle.clone();
        let label_opts = options.clone();
        let ph = placeholder;
        let color_opts = options.clone();
        let trigger = field_frame(size)
            .items_center()
            .border_color(rx(move || {
                Some(if ring.is_focus_visible() {
                    ColorRole::FocusRing
                } else {
                    ColorRole::Border
                })
            }))
            .role(Role::TextInput)
            .track_focus(&handle)
            .anchor_ref(&anchor)
            .on_click(move |_, _| open.set(true))
            .on_key_down(move |kev, _cx| {
                if !kev.repeat
                    && matches!(
                        kev.code,
                        KeyCode::ArrowDown | KeyCode::Enter | KeyCode::NumpadEnter | KeyCode::Space
                    )
                {
                    open.set(true);
                }
            })
            .child(
                text(rx(move || {
                    let s = selected_label(&label_opts, &value.get());
                    if s.is_empty() { ph.clone() } else { s }
                }))
                .color_role(rx(move || {
                    if selected_label(&color_opts, &value.get()).is_empty() {
                        ColorRole::TextMuted
                    } else {
                        ColorRole::Text
                    }
                }))
                .size(font),
            )
            // Chevron flips ▾→▴ while the list is open (the conventional dropdown affordance).
            .child(
                text(rx(move || {
                    SharedString::from(if open.get() { "▴" } else { "▾" })
                }))
                .color_role(ColorRole::TextMuted)
                .size(font),
            );

        // Popup: the option list, focused on open, dismissed on outside/Esc (#219).
        let popup = overlay(dyn_if(
            move || open.get(),
            move || build_dropdown_panel(options.clone(), value, open, font),
        ))
        .anchor(&anchor, Placement::Below)
        .open(open)
        .dismiss_on_outside(true);

        div().child(trigger).child(popup).into_element()
    }
}

/// A filter-as-you-type selection control (#79): the trigger is a text field that narrows the option list as
/// you type; a click / Enter on a row picks it. Build with [`combobox`]; chain `.placeholder(..)` /
/// `.size(..)` / `.disabled(..)`. Selection is **controlled** via `RwSignal<T>` (D2); the filter is
/// transient. Returns `impl IntoElement`; `Styled` is **not** on its surface.
pub struct Combobox<T> {
    value: RwSignal<T>,
    options: Options<T>,
    placeholder: SharedString,
    size: ControlSize,
    disabled: bool,
}

/// Creates a medium, enabled combobox bound to `value`, listing `options` as `(value, label)` pairs (#79).
pub fn combobox<T: Clone + PartialEq + Send + Sync + 'static>(
    value: RwSignal<T>,
    options: Vec<(T, SharedString)>,
) -> Combobox<T> {
    Combobox {
        value,
        options: Arc::new(options),
        placeholder: SharedString::from(""),
        size: ControlSize::Md,
        disabled: false,
    }
}

impl<T> Combobox<T> {
    /// Sets the placeholder shown while filtering (empty field) or when `value` matches no option.
    pub fn placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    /// Sets the control size (D3).
    pub fn size(mut self, size: ControlSize) -> Self {
        self.size = size;
        self
    }

    /// Disables the control: muted, display-only (no filter/popup), inert, and not focusable.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

impl<T: Clone + PartialEq + Send + Sync + 'static> IntoElement for Combobox<T> {
    fn into_element(self) -> AnyElement {
        let Combobox {
            value,
            options,
            placeholder,
            size,
            disabled,
        } = self;
        let font = label_px(size);

        if disabled {
            return disabled_field(value, options, placeholder, size);
        }

        let open = RwSignal::new(false);
        let anchor = AnchorHandle::new();
        let filter = RwSignal::new(String::new());
        // `highlight` is an option index (into `options`), kept within the visible set (mirrors Dropdown).
        let highlight = RwSignal::new(0usize);

        // Close/show-label: while closed (or on a value change while closed) the field shows the selected
        // label; on open it clears for filtering. Subscribes to `open` + `value` only (NOT `filter`), so
        // typing never re-triggers it — no open↔filter loop. Guarded equal-write.
        {
            let opts = options.clone();
            create_effect(move || {
                let s = if open.get() {
                    String::new()
                } else {
                    selected_label(&opts, &value.get()).as_str().to_string()
                };
                if filter.with_untracked(|f| *f != s) {
                    filter.set(s);
                }
            });
        }
        // A filter change resets the highlight to the first visible option.
        {
            let opts = options.clone();
            create_effect(move || {
                let visible = filter_visible(&opts, &filter.get());
                highlight.set(visible.first().copied().unwrap_or(0));
            });
        }

        // The trigger: a text-input filter wrapped in a container that catches the bubbled Arrow/Enter/Esc
        // (#312) while focus stays in the field, and opens the popup on click.
        let nav_opts = options.clone();
        let container = div()
            .anchor_ref(&anchor)
            .on_click(move |_, _| open.set(true))
            .on_key_down(move |kev, _cx| {
                if kev.repeat {
                    return;
                }
                let visible = filter_visible(&nav_opts, &filter.get_untracked());
                match kev.code {
                    KeyCode::ArrowDown => {
                        // Idempotent open: re-setting `open` while already open would re-fire the
                        // close/show-label effect (it subscribes to `open`) and wipe the typed filter —
                        // an equal `set` still notifies subscribers. Only open on the closed→open edge.
                        if !open.get_untracked() {
                            open.set(true);
                        }
                        highlight.set(step_visible(&visible, highlight.get_untracked(), true));
                    }
                    KeyCode::ArrowUp => {
                        highlight.set(step_visible(&visible, highlight.get_untracked(), false));
                    }
                    KeyCode::Enter | KeyCode::NumpadEnter => {
                        let hl = highlight.get_untracked();
                        if visible.contains(&hl) {
                            if let Some((v, _)) = nav_opts.get(hl) {
                                value.set(v.clone());
                                open.set(false);
                            }
                        }
                    }
                    KeyCode::Escape => open.set(false),
                    _ => {}
                }
            })
            .child(text_input(filter).placeholder(placeholder).size(size));

        // Popup: passive filtered rows (nav is driven by the container). A single-item `dyn_list` keyed on
        // the filter string rebuilds the whole visible **column** whenever the filter changes (#278) — the
        // rows are direct children of a `flex_col`, since `dyn_list` itself lays its own children out in the
        // default (row) direction.
        let row_opts = options.clone();
        let popup = overlay(dyn_if(
            move || open.get(),
            move || {
                let opts = row_opts.clone();
                panel_frame().child(dyn_list(
                    move || vec![filter.get()],
                    |q: &String| q.clone(),
                    move |_q| {
                        let opts = opts.clone();
                        let visible = filter_visible(&opts, &filter.get_untracked());
                        // Floor the column to the panel's min width so each row (and its highlight fill) spans
                        // the full popup width — matching Dropdown/Menu. `dyn_list` lays this single child out in
                        // the default (row) direction, so the column would otherwise collapse to content width.
                        let mut col = div().flex_col().min_w_40();
                        for i in visible {
                            let (v, label) = opts[i].clone();
                            col = col.child(option_row(label, highlight, i, font, move || {
                                value.set(v.clone());
                                open.set(false);
                            }));
                        }
                        col
                    },
                ))
            },
        ))
        .anchor(&anchor, Placement::Below)
        .open(open)
        .dismiss_on_outside(true);

        div().child(container).child(popup).into_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc as StdArc;

    use kagari_base::{NodeId, Point, Size as BSize};
    use kagari_core::arena::Arena;
    use kagari_core::damage::DamageState;
    use kagari_core::event::FocusRegistry;
    use kagari_core::overlay::{OverlayLayers, OverlayRegistry};
    use kagari_core::paint::render_tree;
    use kagari_core::reactive::{Owner, provide_context};
    use kagari_core::{
        DispatchState, HitTest, KeyEvent, Modifiers, MouseButton, MouseEvent, MouseKind,
        dispatch_key, dispatch_mouse,
    };
    use kagari_layout::LayoutTree;
    use kagari_render::Scene;
    use kagari_style::Theme;
    use kagari_text::{FontDb, TextSystem};

    const VIEWPORT: BSize = BSize { w: 300.0, h: 300.0 };

    struct Harness {
        root: AnyElement,
        arena: Arena,
        layout: LayoutTree,
        text: TextSystem,
        hit: HitTest,
        reg: OverlayRegistry,
        focus: FocusRegistry,
        layers: OverlayLayers,
        damage: StdArc<DamageState>,
        theme: Theme,
        root_id: NodeId,
    }

    /// Builds the widget under the overlay + focus contexts (so the inner editor/trigger mint into them) and
    /// renders one frame. The contexts must exist *before* `build` runs (the editor's focus handle is minted
    /// at construction), so the widget is built by a closure, not passed pre-built.
    fn harness(build: impl FnOnce() -> AnyElement) -> Harness {
        let layers = OverlayLayers::new();
        provide_context(layers.clone());
        let focus = FocusRegistry::new();
        provide_context(focus.clone());
        let root = build();
        let mut h = Harness {
            root,
            arena: Arena::new(),
            layout: LayoutTree::new(),
            text: TextSystem::new(FontDb::new()),
            hit: HitTest::new(),
            reg: OverlayRegistry::new(),
            focus,
            layers,
            damage: StdArc::new(DamageState::default()),
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
            Some(&mut h.focus),
            None,
            None,
            &mut scene,
            VIEWPORT,
            &h.damage,
            &h.theme,
        )
        .unwrap()
    }

    fn opts() -> Vec<(u32, SharedString)> {
        vec![
            (1, SharedString::from("Apple")),
            (2, SharedString::from("Banana")),
            (3, SharedString::from("Cherry")),
        ]
    }

    /// The count of visible combobox rows (outer → overlay → dyn_if → panel → dyn_list → column → rows).
    fn combobox_row_count(h: &Harness) -> usize {
        let overlay = h.arena.get(h.root_id).unwrap().children[1];
        let dyn_if = h.arena.get(overlay).unwrap().children[0];
        let panel = h.arena.get(dyn_if).unwrap().children[0];
        let list = h.arena.get(panel).unwrap().children[0];
        let column = h.arena.get(list).unwrap().children[0];
        h.arena.get(column).unwrap().children.len()
    }

    /// Whether the popup is mounted (outer → overlay → dyn_if has a child). Works for dropdown + combobox.
    fn popup_mounted(h: &Harness) -> bool {
        let overlay = h.arena.get(h.root_id).unwrap().children[1];
        let dyn_if = h.arena.get(overlay).unwrap().children[0];
        !h.arena.get(dyn_if).unwrap().children.is_empty()
    }

    /// The dropdown popup panel node (outer → child[1] overlay → dyn_if → panel), once mounted.
    fn dropdown_panel(h: &Harness) -> NodeId {
        let overlay = h.arena.get(h.root_id).unwrap().children[1];
        let dyn_if = h.arena.get(overlay).unwrap().children[0];
        h.arena.get(dyn_if).unwrap().children[0]
    }

    /// Dispatches a key to the focused node (dropdown: the panel; combobox: the editor leaf).
    fn press_focused(h: &mut Harness, code: KeyCode) {
        let kev = KeyEvent::new(code, Modifiers::default(), true, false);
        dispatch_key(&mut h.root, &h.arena, h.focus.focused_node(), &kev);
    }

    /// Types one character into the focused editor (combobox filter).
    fn type_char(h: &mut Harness, code: KeyCode, ch: &str) {
        let mut kev = KeyEvent::new(code, Modifiers::default(), true, false);
        kev.text = Some(SharedString::from(ch.to_string()));
        dispatch_key(&mut h.root, &h.arena, h.focus.focused_node(), &kev);
    }

    fn click_at(h: &mut Harness, c: Point) {
        let mut dispatch = DispatchState::default();
        for kind in [
            MouseKind::Down(MouseButton::Left),
            MouseKind::Up(MouseButton::Left),
        ] {
            let ev = MouseEvent::new(kind, c, Modifiers::default());
            dispatch_mouse(&mut h.root, &h.arena, &h.hit, &ev, &mut dispatch);
        }
    }

    fn click_center(h: &mut Harness, node: NodeId) {
        let b = h.layout.absolute_layout(node);
        click_at(
            h,
            Point::new(b.origin.x + b.size.w / 2.0, b.origin.y + b.size.h / 2.0),
        );
    }

    /// Clicks a row inside an anchored overlay panel. The overlay lays its content out at the origin but
    /// **paints** it below the anchor (`Placement::Below`), so the painted (= hit-test) position is the row's
    /// panel-relative layout offset translated by the anchor's bottom-left.
    fn click_overlay_row(h: &mut Harness, anchor: NodeId, panel: NodeId, row: NodeId) {
        let a = h.layout.absolute_layout(anchor);
        let p = h.layout.absolute_layout(panel);
        let r = h.layout.absolute_layout(row);
        let c = Point::new(
            a.origin.x + (r.origin.x - p.origin.x) + r.size.w / 2.0,
            (a.origin.y + a.size.h) + (r.origin.y - p.origin.y) + r.size.h / 2.0,
        );
        click_at(h, c);
    }

    #[test]
    fn dropdown_should_select_option() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(1u32);
        let mut h = harness(|| dropdown(value, opts()).into_element());

        // Open by clicking the trigger; the panel mounts + focuses itself, then ArrowDown → Banana, Enter.
        let trigger = h.arena.get(h.root_id).unwrap().children[0];
        click_center(&mut h, trigger);
        frame(&mut h); // realize the dyn_if mount (panel focuses itself)
        press_focused(&mut h, KeyCode::ArrowDown); // panel: highlight → Banana
        press_focused(&mut h, KeyCode::Enter);
        assert_eq!(value.get_untracked(), 2, "ArrowDown+Enter selects Banana");
        drop(owner);
    }

    #[test]
    fn dropdown_click_should_select_option() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(1u32);
        let mut h = harness(|| dropdown(value, opts()).into_element());

        let trigger = h.arena.get(h.root_id).unwrap().children[0];
        click_center(&mut h, trigger); // open
        frame(&mut h);
        let panel = dropdown_panel(&h);
        // The panel's children are the option rows [Apple, Banana, Cherry] — Cherry is index 2.
        let cherry = h.arena.get(panel).unwrap().children[2];
        click_overlay_row(&mut h, trigger, panel, cherry);
        assert_eq!(
            value.get_untracked(),
            3,
            "clicking the Cherry row selects it"
        );
        drop(owner);
    }

    #[test]
    fn dropdown_esc_should_dismiss() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(1u32);
        let mut h = harness(|| dropdown(value, opts()).into_element());

        let trigger = h.arena.get(h.root_id).unwrap().children[0];
        click_center(&mut h, trigger);
        frame(&mut h);
        press_focused(&mut h, KeyCode::Escape);
        // After dismiss the panel unmounts on the next frame.
        frame(&mut h);
        let overlay = h.arena.get(h.root_id).unwrap().children[1];
        let dyn_if = h.arena.get(overlay).unwrap().children[0];
        assert!(
            h.arena.get(dyn_if).unwrap().children.is_empty(),
            "Esc dismisses the dropdown (panel unmounts)"
        );
        drop(owner);
    }

    #[test]
    fn dropdown_disabled_should_not_focus() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(1u32);
        let mut h = harness(|| dropdown(value, opts()).disabled(true).into_element());

        // A disabled dropdown is a static field (no editor/trigger), so a click focuses nothing.
        let root = h.root_id;
        click_center(&mut h, root);
        assert_eq!(
            h.focus.focused_node(),
            None,
            "a disabled dropdown is inert and not focusable"
        );
        drop(owner);
    }

    #[test]
    fn combobox_filter_should_narrow_options() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(1u32);
        let filter_probe = RwSignal::new(String::new());
        let _ = filter_probe;
        let mut h = harness(|| combobox(value, opts()).into_element());

        // Open the popup (click the field), then type a filter by driving the internal editor.
        let container = h.arena.get(h.root_id).unwrap().children[0];
        click_center(&mut h, container); // opens; clears filter
        frame(&mut h);
        // Type "an" into the focused editor → matches Banana only.
        type_char(&mut h, KeyCode::KeyA, "a");
        type_char(&mut h, KeyCode::KeyN, "n");
        frame(&mut h); // realize the dyn_list filter
        // overlay → dyn_if → panel → dyn_list → column → rows
        let rows = combobox_row_count(&h);
        assert_eq!(rows, 1, "typing 'an' narrows the list to Banana");
        drop(owner);
    }

    #[test]
    fn combobox_should_select_filtered_option() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(1u32);
        let mut h = harness(|| combobox(value, opts()).into_element());

        let container = h.arena.get(h.root_id).unwrap().children[0];
        click_center(&mut h, container);
        frame(&mut h);
        type_char(&mut h, KeyCode::KeyC, "c");
        frame(&mut h);
        // Enter selects the single visible (Cherry). Arrow/Enter bubble from the editor to the container.
        press_focused(&mut h, KeyCode::Enter);
        assert_eq!(
            value.get_untracked(),
            3,
            "filtering to 'c' then Enter selects Cherry"
        );
        drop(owner);
    }

    #[test]
    fn combobox_open_should_show_all_options() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(2u32); // Banana selected
        let mut h = harness(|| combobox(value, opts()).into_element());

        // While closed the field holds the selected label ("Banana") as its text; opening must clear that
        // to the empty filter (the close-effect), so all options show — not just the pre-selected one.
        let container = h.arena.get(h.root_id).unwrap().children[0];
        click_center(&mut h, container);
        frame(&mut h);
        assert_eq!(
            combobox_row_count(&h),
            3,
            "opening clears any prior filter (the closed field's label text), showing all options"
        );
        drop(owner);
    }

    #[test]
    fn dropdown_trigger_key_should_open() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(1u32);
        let mut h = harness(|| dropdown(value, opts()).into_element());

        // ArrowDown delivered to the trigger itself (its own on_key_down open path) opens the popup.
        let trigger = h.arena.get(h.root_id).unwrap().children[0];
        let kev = KeyEvent::new(KeyCode::ArrowDown, Modifiers::default(), true, false);
        dispatch_key(&mut h.root, &h.arena, Some(trigger), &kev);
        frame(&mut h);
        assert!(
            popup_mounted(&h),
            "ArrowDown on the trigger opens the dropdown"
        );
        drop(owner);
    }

    #[test]
    fn dropdown_outside_press_should_dismiss() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(1u32);
        let mut h = harness(|| dropdown(value, opts()).into_element());

        let trigger = h.arena.get(h.root_id).unwrap().children[0];
        click_center(&mut h, trigger); // open
        frame(&mut h);
        assert!(
            !h.layers.is_empty(),
            "an open dropdown registers a dismiss-on-outside layer"
        );
        assert!(
            h.layers.dismiss_topmost(),
            "the dismissible layer is dismissed by an outside press (#219)"
        );
        frame(&mut h);
        assert!(
            !popup_mounted(&h),
            "an outside press dismisses the dropdown (panel unmounts)"
        );
        drop(owner);
    }

    #[test]
    fn combobox_arrow_should_keep_filter_and_navigate() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(1u32);
        let mut h = harness(|| combobox(value, opts()).into_element());

        let container = h.arena.get(h.root_id).unwrap().children[0];
        click_center(&mut h, container); // open (filter cleared)
        frame(&mut h);
        type_char(&mut h, KeyCode::KeyA, "a"); // "a" matches Apple + Banana (not Cherry)
        frame(&mut h);
        assert_eq!(
            combobox_row_count(&h),
            2,
            "filter 'a' narrows to Apple + Banana"
        );

        // ArrowDown moves the highlight WITHIN the filtered set — it must NOT re-open/clear the filter.
        press_focused(&mut h, KeyCode::ArrowDown);
        frame(&mut h);
        assert_eq!(
            combobox_row_count(&h),
            2,
            "ArrowDown keeps the typed filter (an idempotent open must not wipe it)"
        );
        press_focused(&mut h, KeyCode::Enter);
        assert_eq!(
            value.get_untracked(),
            2,
            "Enter selects the highlighted second match (Banana)"
        );
        drop(owner);
    }
}
