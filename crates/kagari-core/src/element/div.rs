//! The [`Div`] element: a flex container with children and raw style props (#31).

use std::sync::{Arc, Mutex};

use kagari_base::{Color, Corners, Edges, NodeId, Point, Px, Rect, Size};
use kagari_layout::{FlexDirection, LayoutStyle};
use kagari_render::{Background, Border, Quad, RoundedRect, Shadow};
use kagari_style::{SpacingStep, Style, Styled, Theme};

use super::{AnyElement, DamageSink, Element, Event, EventCx, IntoElement, LayoutCx, PaintCx};
use crate::arena::Node;
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
    bg: Option<Prop<Background>>,
    /// Assigned on the first `request_layout`; the build-once rebuild lifecycle is #32.
    id: Option<NodeId>,
    /// Child node ids in `children` order, assigned at build; `paint` reads each child's bounds
    /// from the layout tree by id.
    child_ids: Vec<NodeId>,
    /// Resolved background: written by the bound reactive effect (or once, for a static prop),
    /// read by `paint`. Shared + `'static` so the effect can own a handle to it.
    resolved_bg: Arc<Mutex<Option<Background>>>,
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
}

/// Creates an empty [`Div`].
pub fn div() -> Div {
    Div {
        children: Vec::new(),
        layout: LayoutStyle::default(),
        style: Style::default(),
        bg: None,
        id: None,
        child_ids: Vec::new(),
        resolved_bg: Arc::new(Mutex::new(None)),
        size: None,
        resolved_size: Arc::new(Mutex::new(None)),
        applied_layout: None,
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
        self.bg = Some(bg.into());
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

    /// Sets a fixed size — a static value or a reactive [`Prop`] (#146). **Damage kind: layout-dirty**:
    /// a reactive size's bind effect re-applies the layout style and marks the node layout-dirty, so a
    /// signal-driven size change triggers relayout (contrast [`background`](Self::background), which is
    /// paint-dirty). A static `Size` resolves once.
    pub fn size(mut self, size: impl Into<Prop<Size>>) -> Self {
        self.size = Some(size.into());
        self
    }

    /// Resolves the background prop into `resolved_bg`. A static prop writes its value once; a
    /// reactive prop registers a synchronous effect (ADR 0001) that re-resolves the closure,
    /// writes the cell, and flags paint-damage for `id` — the #35 hook.
    ///
    /// The effect is registered **once** at build (the prop closure is moved into it) and stays
    /// alive via its signal subscription; it is owner-scoped to the *current* ambient owner (the
    /// App root in #36). Per-node owners — so a removed node disposes its effect — and the real
    /// damage tracking arrive with #32/#35; see RK-005.
    fn bind_background(&mut self, damage: &Arc<dyn DamageSink>, id: NodeId) {
        match self.bg.take() {
            Some(Prop::Static(bg)) => self.store_bg(bg),
            Some(Prop::Reactive(read)) => {
                let cell = Arc::clone(&self.resolved_bg);
                let damage = Arc::clone(damage);
                create_effect(move || {
                    let bg = read();
                    if let Ok(mut slot) = cell.lock() {
                        // Only flag damage when the resolved value actually changed: a plain
                        // signal re-runs subscribers even on an equal-value write, and idle
                        // re-paints violate the damage discipline (design.md §9).
                        if *slot != Some(bg) {
                            *slot = Some(bg);
                            damage.mark_paint_dirty(id);
                        }
                    }
                });
            }
            None => {}
        }
    }

    fn store_bg(&self, bg: Background) {
        if let Ok(mut slot) = self.resolved_bg.lock() {
            *slot = Some(bg);
        }
    }

    fn current_bg(&self) -> Option<Background> {
        self.resolved_bg.lock().ok().and_then(|slot| *slot)
    }

    /// Resolves the size prop into `resolved_size` (#146), the **layout-dirty** counterpart of
    /// [`bind_background`](Self::bind_background). A static prop writes the cell once; a reactive prop
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
        let mut ls = self.layout;
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
                self.bind_background(&cx.damage, id);
                self.bind_size(&cx.damage, id);
                id
            }
        };

        // Resolve layout tokens (gap/padding) against the *current* theme every build, but re-apply
        // to taffy only when the result changed. First build applies it; a theme swap (#43) yields a
        // different `LayoutStyle`, so `set_style` re-runs and the node relays out — without this,
        // layout tokens would stay stale after a reskin (paint tokens already re-resolve per frame).
        // The dedup keeps idle frames from re-marking taffy dirty.
        let resolved = self.effective_layout(cx.theme);
        if self.applied_layout != Some(resolved) {
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
                    rect: Rect::from_xywh(0.0, 0.0, 1.0e4, 1.0e4),
                    radii: Corners::default(),
                },
                order,
            });
        }

        // Background: the semantic token (`bg(role)` → resolved theme color) takes precedence; the
        // raw/reactive `.background()` value is the fallback. border emit is deferred (the theme has
        // no border-width resolution yet — a follow-up).
        let bg = self
            .style
            .paint
            .bg
            .map(|role| Background::Solid(cx.theme.resolve_color(role)))
            .or_else(|| self.current_bg());
        if let Some(bg) = bg {
            let order = cx.next_order();
            cx.scene.quads.push(Quad {
                bounds,
                corner_radii,
                bg,
                border: Border {
                    widths: Edges::default(),
                    color: Color::TRANSPARENT,
                },
                content_mask: RoundedRect {
                    rect: bounds,
                    radii: corner_radii,
                },
                // Painter's order from traversal order: the parent's quad takes a lower order than
                // its children's primitives, so children draw on top.
                order,
            });
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

    fn handle_event(&mut self, _ev: &Event, _cx: &mut EventCx) {}
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
    use kagari_style::ColorRole;
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
        });

        let mut scene = Scene::new();
        d.paint(
            Rect::default(),
            &mut PaintCx::new(&mut scene, &layout, &mut text, None, &theme),
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
        });

        let paint = |d: &mut Div, scene: &mut Scene, text: &mut TextSystem| {
            d.paint(
                Rect::default(),
                &mut PaintCx::new(scene, &layout, text, None, &theme),
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
}
