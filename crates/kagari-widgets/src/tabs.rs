//! The [`Tabs`] widget (#85): a switchable tab strip + a panel that renders the selected tab's content.
//!
//! The strip mirrors [`Segmented`](crate::Segmented) — one focus stop, wrapping arrow-nav, reactive
//! selected state, Div-based a11y — but the active tab is marked by an **Accent underline** (a 2px bar)
//! rather than a fill. The **panel switches live**: a `dyn_list` keyed by the selected index rebuilds the
//! panel when `selected` changes (the old panel unmounts, the new builds fresh), driven by the frame-loop
//! reconcile (#278). Selection is controlled via a `RwSignal<usize>` (D2).

use kagari_base::SharedString;
use kagari_core::element::dyn_list;
use kagari_core::reactive::prelude::*;
use kagari_core::reactive::{RwSignal, rx};
use kagari_core::{AnyElement, IntoElement, KeyCode, Role, div, text, use_focus_handle};
use kagari_style::{ColorRole, Styled};

use crate::control::{ControlSize, label_px};

/// A tab's panel **content builder**: called to build the panel element when the tab becomes selected (a
/// closure, so the panel can be rebuilt each time it is switched to — a pre-built element could not).
type TabContent = Box<dyn Fn() -> AnyElement>;

/// A switchable tabbed view bound to a `usize` signal (#85). Build with [`tabs`]; add tabs with
/// `.tab(label, content)`, then chain `.size(..)`. The selected tab's panel renders; clicking a tab (or an
/// arrow key while the strip is focused) sets the bound `selected` index. The active tab shows an Accent
/// underline.
pub struct Tabs {
    selected: RwSignal<usize>,
    /// Each tab's label + its panel content builder ([`TabContent`]).
    tabs: Vec<(SharedString, TabContent)>,
    size: ControlSize,
}

/// Creates a tabbed view bound to `selected` (controlled: activating a tab sets `selected` to its index).
pub fn tabs(selected: RwSignal<usize>) -> Tabs {
    Tabs {
        selected,
        tabs: Vec::new(),
        size: ControlSize::Md,
    }
}

impl Tabs {
    /// Adds a tab: its `label` and a `content` builder — a closure that builds the panel element for this
    /// tab (invoked when the tab is selected, and again if it is switched away from and back to).
    pub fn tab<C: IntoElement>(
        mut self,
        label: impl Into<SharedString>,
        content: impl Fn() -> C + 'static,
    ) -> Self {
        self.tabs
            .push((label.into(), Box::new(move || content().into_element())));
        self
    }

    /// Sets the control size (D3) — drives the tab label font size.
    pub fn size(mut self, size: ControlSize) -> Self {
        self.size = size;
        self
    }
}

impl IntoElement for Tabs {
    fn into_element(self) -> AnyElement {
        let Tabs {
            selected,
            tabs,
            size,
        } = self;
        let n = tabs.len();
        let (labels, builders): (Vec<SharedString>, Vec<TabContent>) = tabs.into_iter().unzip();

        // One focus handle for the whole strip (the tab-navigation scope).
        let handle = use_focus_handle();

        // The tab strip: a row of tabs. Arrow keys move the selection (wrapping); the strip is the
        // focused node. Mirrors `segmented.rs`.
        let mut strip = div()
            .flex()
            .role(Role::RadioGroup)
            .track_focus(&handle)
            .on_key_down(move |kev, _cx| {
                if kev.repeat || n == 0 {
                    return;
                }
                let down = matches!(kev.code, KeyCode::ArrowDown | KeyCode::ArrowRight);
                let up = matches!(kev.code, KeyCode::ArrowUp | KeyCode::ArrowLeft);
                if !down && !up {
                    return;
                }
                let cur = selected.get_untracked().min(n - 1);
                let next = if down {
                    (cur + 1) % n
                } else {
                    (cur + n - 1) % n
                };
                selected.set(next);
            });

        for (i, label) in labels.into_iter().enumerate() {
            let a11y = label.clone();
            // A centered label row; active = Text, inactive = TextMuted (the Accent underline is the
            // primary active cue).
            let label_row = div().flex().justify_center().px_3().py_2().child(
                text(label)
                    .color_role(rx(move || {
                        if selected.get() == i {
                            ColorRole::Text
                        } else {
                            ColorRole::TextMuted
                        }
                    }))
                    .size(label_px(size)),
            );
            // The 2px underline bar: Accent under the active tab (FocusRing while the strip is
            // focus-visible), Border otherwise — the inactive bars form a continuous baseline across
            // the whole strip, so the widget reads as a tab bar with the active tab highlighted.
            // Full-width via flex stretch.
            let hb = handle.clone();
            let underline = div().h_0p5().bg(rx(move || {
                if selected.get() == i {
                    if hb.is_focus_visible() {
                        ColorRole::FocusRing
                    } else {
                        ColorRole::Accent
                    }
                } else {
                    ColorRole::Border
                }
            }));

            let hc = handle.clone();
            let tab = div()
                .flex_col()
                .role(Role::Radio)
                .a11y_label(a11y)
                .a11y_checked(rx(move || selected.get() == i))
                .on_click(move |_ev, _cx| {
                    selected.set(i);
                    hc.focus();
                })
                .child(label_row)
                .child(underline);
            strip = strip.child(tab);
        }

        // The panel: a 1-item `dyn_list` keyed by the selected index. Switching `selected` removes the old
        // key and adds the new, so the old panel unmounts and the new one builds fresh (rebuild-on-switch)
        // — driven live by the frame-loop reconcile (#278). Out-of-range clamps to the last tab; empty →
        // no panel.
        let panel = dyn_list(
            move || {
                if n == 0 {
                    Vec::new()
                } else {
                    vec![selected.get().min(n - 1)]
                }
            },
            |k: &usize| *k,
            move |idx: &usize| (builders[*idx])(),
        );

        div()
            .flex_col()
            .gap_2()
            .child(strip)
            .child(panel)
            .into_element()
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
    use kagari_core::reactive::Owner;
    use kagari_core::{
        DispatchState, HitTest, KeyEvent, Modifiers, MouseButton, MouseEvent, MouseKind,
        dispatch_key, dispatch_mouse,
    };
    use kagari_layout::LayoutTree;
    use kagari_render::{Background, Scene};
    use kagari_style::{ColorRole, Theme};
    use kagari_text::{FontDb, TextSystem};

    const VIEWPORT: BSize = BSize { w: 300.0, h: 200.0 };

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

    fn press_strip(b: &mut Built, code: KeyCode) {
        // The key handler lives on the strip (the focus scope), which is the root's first child.
        let strip = b.arena.get(b.root_id).unwrap().children[0];
        let kev = KeyEvent::new(code, Modifiers::default(), true, false);
        dispatch_key(&mut b.root, &b.arena, Some(strip), &kev);
    }

    fn click_at(b: &mut Built, pos: Point) {
        let mut dispatch = DispatchState::default();
        for kind in [
            MouseKind::Down(MouseButton::Left),
            MouseKind::Up(MouseButton::Left),
        ] {
            let ev = MouseEvent::new(kind, pos, Modifiers::default());
            dispatch_mouse(&mut b.root, &b.arena, &b.hit, &ev, &mut dispatch);
        }
    }

    fn tabs_3(selected: RwSignal<usize>) -> Tabs {
        tabs(selected).tab("A", div).tab("B", div).tab("C", div)
    }

    #[test]
    fn tabs_should_switch_on_click() {
        let owner = Owner::new();
        owner.set();
        let selected = RwSignal::new(0usize);
        let mut b = build(tabs_3(selected), &Theme::light());
        // The second tab (index 1) sits to the right in the strip; click its center.
        let strip = b.arena.get(b.root_id).unwrap().children[0];
        let tab1 = b.arena.get(strip).unwrap().children[1];
        let tb = b.layout.absolute_layout(tab1);
        click_at(
            &mut b,
            Point::new(tb.origin.x + tb.size.w / 2.0, tb.origin.y + tb.size.h / 2.0),
        );
        assert_eq!(
            selected.get_untracked(),
            1,
            "clicking the second tab selects it"
        );
        drop(owner);
    }

    #[test]
    fn tabs_arrow_should_move_selection() {
        let owner = Owner::new();
        owner.set();
        let selected = RwSignal::new(0usize);
        let mut b = build(tabs_3(selected), &Theme::light());
        press_strip(&mut b, KeyCode::ArrowRight);
        assert_eq!(
            selected.get_untracked(),
            1,
            "ArrowRight selects the next tab"
        );
        press_strip(&mut b, KeyCode::ArrowRight);
        assert_eq!(selected.get_untracked(), 2);
        press_strip(&mut b, KeyCode::ArrowRight);
        assert_eq!(
            selected.get_untracked(),
            0,
            "ArrowRight wraps from the last to the first"
        );
        press_strip(&mut b, KeyCode::ArrowLeft);
        assert_eq!(
            selected.get_untracked(),
            2,
            "ArrowLeft wraps from the first to the last"
        );
        drop(owner);
    }

    #[test]
    fn tabs_active_tab_should_underline() {
        // Exactly one Accent underline bar (the active tab's); the other bars are Border.
        let owner = Owner::new();
        owner.set();
        let theme = Theme::light();
        let b = build(tabs_3(RwSignal::new(1)), &theme);
        let accent = Background::Solid(theme.resolve_color(ColorRole::Accent));
        let bars: Vec<_> = b.scene.quads.iter().filter(|q| q.bg == accent).collect();
        assert_eq!(
            bars.len(),
            1,
            "exactly one tab shows an Accent underline (the active one)"
        );
        // The bar is a real 2px-tall (h_0p5, S0p5) full-width stripe: `h_0p5` fixes the height while the
        // width stretches to the tab (per-axis size tokens, #280 — before which the bar was 0px tall).
        assert_eq!(
            bars[0].bounds.size.h, 2.0,
            "the underline is 2px tall (h_0p5 resolves to S0p5=2)"
        );
        assert!(
            bars[0].bounds.size.w > 0.0,
            "the underline stretches to a non-zero width"
        );
        // The inactive bars form the baseline with `Border`, NOT `Surface` — on the light theme `Surface`
        // is white and would be invisible (RK-030), so guard against a regression to it.
        let border = Background::Solid(theme.resolve_color(ColorRole::Border));
        let surface = Background::Solid(theme.resolve_color(ColorRole::Surface));
        assert_ne!(
            border, surface,
            "Border must differ from Surface for the baseline to be visible on light"
        );
        assert_eq!(
            b.scene.quads.iter().filter(|q| q.bg == border).count(),
            2,
            "the two inactive tabs paint a visible Border baseline (not invisible Surface)"
        );
        drop(owner);
    }

    #[test]
    fn tabs_should_render_selected_panel() {
        // #278 check (render_tree-driven, no manual reconcile): the selected tab's panel renders, and
        // switching `selected` swaps it live — the old panel's quad disappears, the new one's appears.
        let owner = Owner::new();
        owner.set();
        let theme = Theme::light();
        let red = Background::Solid(kagari_base::Color::new(1.0, 0.0, 0.0, 1.0));
        let blue = Background::Solid(kagari_base::Color::new(0.0, 0.0, 1.0, 1.0));
        let selected = RwSignal::new(0usize);
        let mut root = tabs(selected)
            .tab("A", move || {
                div().background(red).size(BSize { w: 20.0, h: 20.0 })
            })
            .tab("B", move || {
                div().background(blue).size(BSize { w: 20.0, h: 20.0 })
            })
            .into_element();

        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let damage = Arc::new(DamageState::default());
        let frame = |root: &mut AnyElement,
                     arena: &mut Arena,
                     layout: &mut LayoutTree,
                     text: &mut TextSystem|
         -> Scene {
            let mut scene = Scene::new();
            render_tree(
                root, arena, layout, text, None, None, None, None, None, None, &mut scene,
                VIEWPORT, &damage, &theme,
            )
            .unwrap();
            scene
        };
        let has = |s: &Scene, bg: Background| s.quads.iter().any(|q| q.bg == bg);

        let s0 = frame(&mut root, &mut arena, &mut layout, &mut text);
        assert!(has(&s0, red), "tab A's panel renders when selected");
        assert!(!has(&s0, blue), "tab B's panel is not mounted");

        selected.set(1);
        let s1 = frame(&mut root, &mut arena, &mut layout, &mut text);
        assert!(
            has(&s1, blue),
            "switching selects tab B's panel live (#278)"
        );
        assert!(
            !has(&s1, red),
            "tab A's panel unmounts on switch (rebuild-on-switch)"
        );
        drop(owner);
    }
}
