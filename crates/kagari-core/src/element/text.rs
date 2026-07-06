//! The [`Text`] leaf element (#34): shaped at build, rasterized at paint.

use std::sync::{Arc, Mutex};

use kagari_base::{Color, NodeId, Px, Rect, SharedString};
use kagari_layout::LayoutStyle;
use kagari_style::ColorRole;
use kagari_text::{ShapedText, TextStyle, fontdb};

use super::reactive_prop::ReactiveProp;
use super::{AnyElement, DamageSink, Element, Event, EventCx, IntoElement, LayoutCx, PaintCx};
use crate::arena::Node;
use crate::reactive::{Prop, create_effect};

/// A text leaf. Shapes its content once at `request_layout` (caching the result and reporting its
/// intrinsic size via a [`MeasureFn`](kagari_layout::MeasureFn)), and rasterizes the cached
/// shaping into glyph sprites at `paint`. The content is a static string or a reactive [`Prop`] (#274):
/// a signal-driven string re-shapes and relayouts live (the layout-dirty counterpart of the paint-dirty
/// `color_role`), mirroring [`Icon`](super::icon)'s reactive size.
pub struct Text {
    /// Content prop (static or reactive), taken at build by [`bind_content`](Text::bind_content).
    content: Option<Prop<SharedString>>,
    /// Resolved content: written once for a static prop, or by the bound reactive effect. Read by the
    /// measure/paint to shape. Shared + `'static` so both the effect and the measure own a handle.
    content_cell: Arc<Mutex<SharedString>>,
    /// Whether the content prop is reactive (set at build) — selects the re-shaping measure path.
    reactive_content: bool,
    style: TextStyle,
    color: Color,
    /// Semantic color **role** (static or reactive, #245) — the theme-reactive counterpart of `color`:
    /// the resolved role is stored, and `paint` resolves role → color via `cx.theme`, so it follows a
    /// theme swap (and, for a reactive `rx(..)`, a state signal). Takes precedence over `color` when set.
    color_role: ReactiveProp<ColorRole>,
    /// Whether to wrap to the available width (#260). When set, the measure re-shapes at the width taffy
    /// allots each `compute` (single line when the width is unconstrained); otherwise the text is shaped
    /// once at build and stays single-line.
    wrap: bool,
    id: Option<NodeId>,
    shaped: Option<ShapedText>,
    /// The width key `shaped` was last shaped at, for the re-shaping paint path (wrap #260 / reactive
    /// content #274) — `Some(w)` when wrapping, `None` otherwise — so `paint` re-shapes only when the key
    /// (or the content) changes, reusing the memoized shaping otherwise instead of cloning it every frame.
    /// Unused on the static non-wrap path (`shaped` is the build shaping).
    shaped_width: Option<f32>,
    /// The content `shaped` was last shaped from, for the reactive-content paint memo (#274) — so `paint`
    /// re-shapes when the content cell changes, not only when the width does.
    shaped_content: Option<SharedString>,
}

/// `text("literal")` / `text(String)` — the blanket `From<T> for Prop<T>` covers a `SharedString`; these
/// add the two other common static forms so the reactive-capable `text()` (#274) stays literal-friendly.
impl From<&'static str> for Prop<SharedString> {
    fn from(s: &'static str) -> Self {
        Prop::Static(s.into())
    }
}
impl From<String> for Prop<SharedString> {
    fn from(s: String) -> Self {
        Prop::Static(s.into())
    }
}

/// Creates a text leaf with the default style (bundled "Noto Sans", 16px) and a light color. The content
/// is static (`text("hi")`) or reactive (`text(rx(move || sig.get()))`, #274).
pub fn text(content: impl Into<Prop<SharedString>>) -> Text {
    let content = content.into();
    let reactive_content = matches!(content, Prop::Reactive(_));
    Text {
        content: Some(content),
        content_cell: Arc::new(Mutex::new(SharedString::from(""))),
        reactive_content,
        style: TextStyle {
            family: "Noto Sans".into(),
            size: Px(16.0),
            weight: fontdb::Weight::NORMAL,
            line_height: None,
        },
        color: Color::new(0.9, 0.9, 0.9, 1.0),
        color_role: ReactiveProp::default(),
        wrap: false,
        id: None,
        shaped: None,
        shaped_width: None,
        shaped_content: None,
    }
}

impl Text {
    /// Sets the glyph color from a raw [`Color`].
    pub fn color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }

    /// Sets the glyph color from a semantic **role** — a static token or a reactive [`Prop`] (#245).
    /// Resolved at paint via `cx.theme`, so it follows a theme swap; a reactive `rx(..)` also follows a
    /// state signal. Takes precedence over the raw [`color`](Self::color) when set. **Damage: paint-dirty.**
    pub fn color_role(mut self, role: impl Into<Prop<ColorRole>>) -> Self {
        self.color_role.set(role);
        self
    }

    /// Sets the font size.
    pub fn size(mut self, size: Px) -> Self {
        self.style.size = size;
        self
    }

    /// Wraps the text to its available width (#260): line-breaks to the width taffy allots (the container
    /// width), and stays single-line when the width is unconstrained. Off by default. **Damage: layout**
    /// (a width change re-measures via the shaper at `compute` time).
    pub fn wrap(mut self, wrap: bool) -> Self {
        self.wrap = wrap;
        self
    }

    /// Resolves the content prop into [`content_cell`](Text::content_cell) (#274), the **layout-dirty**
    /// counterpart of the paint-dirty [`color_role`](Text::color_role) binding — a content change needs
    /// relayout, not just a repaint (mirrors [`Icon::bind_size`](super::icon)). A static prop writes the
    /// cell once; a reactive prop registers a synchronous effect (ADR 0001) that re-reads the closure and
    /// — only when the value changed — writes the cell and flags **layout**-damage for `id`. The new
    /// content reaches the shaper on the next `compute` via the measure that reads the cell; the
    /// `mark_layout_dirty` call wakes the scheduler so that frame runs. Registered once at build, so the
    /// effect only re-fires on external signal writes, keeping the write serial with the frame loop (RK-009).
    fn bind_content(&mut self, damage: &Arc<dyn DamageSink>, id: NodeId) {
        match self.content.take() {
            Some(Prop::Static(s)) => {
                if let Ok(mut slot) = self.content_cell.lock() {
                    *slot = s;
                }
            }
            Some(Prop::Reactive(read)) => {
                let cell = Arc::clone(&self.content_cell);
                let damage = Arc::clone(damage);
                create_effect(move || {
                    let s = read();
                    if let Ok(mut slot) = cell.lock() {
                        if *slot != s {
                            *slot = s;
                            damage.mark_layout_dirty(id);
                        }
                    }
                });
            }
            None => {}
        }
    }
}

impl Element for Text {
    fn request_layout(&mut self, cx: &mut LayoutCx) -> NodeId {
        if let Some(id) = self.id {
            return id;
        }
        let id = cx.arena.insert(Node::default());
        self.id = Some(id);
        cx.layout.insert(id, None);
        self.color_role.bind(&cx.damage, id);
        self.bind_content(&cx.damage, id);

        // A reactive content (#274) or a wrapping text (#260) re-shapes each `compute`; a static non-wrap
        // text keeps the shape-once fast path.
        if self.reactive_content || self.wrap {
            // Re-shaping measure: shapes the **current** content (read from the cell) each `compute` via a
            // cloned `Shaper` handle — at the allotted width when wrapping (`avail.width == None` → single
            // line, `Some(w)` → wrap at `w`), single-line otherwise. The `Shaper` cache dedupes an unchanged
            // (content, width); a reactive content write (bound above) marks layout-dirty so the next
            // `compute` re-measures. `paint` re-shapes at the laid-out width (cache-deduped).
            let shaper = cx.text.shaper();
            let cell = Arc::clone(&self.content_cell);
            let style = self.style.clone();
            let wrap = self.wrap;
            cx.layout.set_measure(
                id,
                Box::new(move |avail| {
                    let content = cell.lock().map(|c| c.clone()).unwrap_or_else(|_| "".into());
                    let width = if wrap { avail.width } else { None };
                    shaper.shape(&content, &style, width).size
                }),
            );
        } else {
            // Shape once at build: cache the result, report its intrinsic size as a constant measure (so
            // `compute` needs no TextSystem), and reuse the cached shaping at paint.
            let content = self
                .content_cell
                .lock()
                .map(|c| c.clone())
                .unwrap_or_else(|_| "".into());
            let shaped = cx.text.shape(&content, &self.style, None);
            let size = shaped.size;
            self.shaped = Some(shaped);
            cx.layout.set_measure(id, Box::new(move |_avail| size));
        }
        // A wrapping text needs `min-width: 0` (flexbox's default `min-width: auto` = min-content keeps a
        // flex item at its content width, so it would never shrink to wrap). Per-axis, so `min-height`
        // stays `auto` (no vertical clipping in a height-constrained flex column). #260.
        let style = if self.wrap {
            LayoutStyle {
                min_width: Some(0.0),
                ..LayoutStyle::default()
            }
        } else {
            LayoutStyle::default()
        };
        cx.layout.set_style(id, &style);
        id
    }

    fn paint(&mut self, bounds: Rect, cx: &mut PaintCx) {
        // Resolve the glyph color: a semantic role (resolved via `cx.theme`, so it follows a theme
        // swap / state signal) takes precedence over the raw color. Computed before the atlas/shaping
        // borrows so `cx.theme` and `cx.atlas`/`cx.scene` stay disjoint.
        let color = self
            .color_role
            .current()
            .map(|role| cx.theme.resolve_color(role))
            .unwrap_or(self.color);
        // Without an atlas (GPU-free Scene-structure tests) text emits no glyphs.
        let Some(atlas) = cx.atlas.as_deref_mut() else {
            return;
        };
        // The re-shaping path (wrap #260 / reactive content #274) re-shapes at the laid-out width, but
        // **memoized**: only when the content **or** the width key changed since the last paint (otherwise
        // the build/prev `shaped` is reused by borrow — no per-frame `ShapedText` clone). The width key is
        // `Some(w)` only when wrapping; a reactive non-wrap text keys on content alone (single-line). A
        // static non-wrap text reuses the build-time shaping unchanged.
        if self.reactive_content || self.wrap {
            let content = self
                .content_cell
                .lock()
                .map(|c| c.clone())
                .unwrap_or_else(|_| "".into());
            let width = self.wrap.then_some(bounds.size.w);
            if self.shaped_content.as_ref() != Some(&content) || self.shaped_width != width {
                self.shaped = Some(cx.text.shape(&content, &self.style, width));
                self.shaped_width = width;
                self.shaped_content = Some(content);
            }
        }
        let Some(shaped) = self.shaped.as_ref() else {
            return;
        };
        // Rasterize directly into the scene's glyph buffer (reused across frames) — no per-frame
        // temp Vec — then fix up only the newly-appended sprites.
        let start = cx.scene.glyphs.len();
        cx.text
            .rasterize_into(shaped, color, atlas, &mut cx.scene.glyphs);
        // Glyph sprites are emitted in text-local coordinates at order 0; offset by the laid-out
        // origin and stamp the traversal painter's order so the text draws above its background.
        // Under a scroll ancestor, mask each glyph to the visible viewport (`rasterize_into` left
        // them unclipped); unclipped text keeps that no-op mask.
        let clip = cx.clip();
        let order = cx.next_order();
        for sprite in &mut cx.scene.glyphs[start..] {
            sprite.bounds.origin.x += bounds.origin.x;
            sprite.bounds.origin.y += bounds.origin.y;
            sprite.order = order;
            if let Some(clip) = clip {
                sprite.content_mask.rect = clip;
            }
        }
    }

    fn handle_event(&mut self, _ev: &Event, _cx: &mut EventCx) {}
}

impl IntoElement for Text {
    fn into_element(self) -> AnyElement {
        Box::new(self)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::arena::Arena;
    use crate::damage::DamageState;
    use crate::element::DamageSink;
    use crate::paint::render_tree;
    use crate::reactive::rx;
    use kagari_base::Size as BSize;
    use kagari_layout::LayoutTree;
    use kagari_render::Scene;
    use kagari_style::{ColorRole, Theme};
    use kagari_text::FontDb;
    use kagari_text::TextSystem;
    use reactive_graph::owner::Owner;
    use reactive_graph::prelude::*;
    use reactive_graph::signal::signal;

    struct NoopDamage;
    impl DamageSink for NoopDamage {
        fn mark_paint_dirty(&self, _id: NodeId) {}
    }

    fn build(t: &mut Text) {
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut ts = TextSystem::new(FontDb::new());
        let damage: Arc<dyn DamageSink> = Arc::new(NoopDamage);
        let theme = Theme::default();
        t.request_layout(&mut LayoutCx {
            arena: &mut arena,
            layout: &mut layout,
            text: &mut ts,
            damage,
            theme: &theme,
            focus: None,
            cursor: None,
        });
    }

    #[test]
    fn text_color_role_should_take_precedence_over_raw_color() {
        // A static `.color_role(role)` stores the role at build and wins over a raw `.color(..)` (paint
        // does `color_role.current().map(resolve).unwrap_or(self.color)`); `paint` resolves the role via
        // `cx.theme` (theme-reactive) — a role is enough for a GPU-free assertion (glyph color needs an atlas).
        let mut t = text("hi")
            .color(Color::new(1.0, 0.0, 0.0, 1.0))
            .color_role(ColorRole::Accent);
        build(&mut t);
        assert_eq!(
            t.color_role.current(),
            Some(ColorRole::Accent),
            "color_role is set and takes precedence over the raw color at paint"
        );
    }

    #[test]
    fn text_reactive_color_role_should_update_on_signal() {
        // A reactive `.color_role(rx(..))` re-resolves on a signal write and flags **paint-dirty**
        // (RK-003/005: synchronous effect, keep `owner` alive; RK-009: paint damage, not layout).
        let owner = Owner::new();
        owner.set();
        let (read, write) = signal(false);
        let mut t = text("hi").color_role(rx(move || {
            if read.get() {
                ColorRole::Accent
            } else {
                ColorRole::Text
            }
        }));

        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut ts = TextSystem::new(FontDb::new());
        let damage = Arc::new(DamageState::default());
        let theme = Theme::default();
        let sink: Arc<dyn DamageSink> = damage.clone();
        t.request_layout(&mut LayoutCx {
            arena: &mut arena,
            layout: &mut layout,
            text: &mut ts,
            damage: sink,
            theme: &theme,
            focus: None,
            cursor: None,
        });

        assert_eq!(t.color_role.current(), Some(ColorRole::Text));
        write.set(true);
        assert_eq!(
            t.color_role.current(),
            Some(ColorRole::Accent),
            "the signal write re-resolves the color role"
        );
        assert!(
            damage.is_dirty(),
            "the reactive color-role write flags paint damage"
        );
        drop(owner);
    }

    /// Lays out `t` inside a fixed-width (`parent_w` × tall) flex parent whose cross-axis is not stretched
    /// (`items_start`), so the text sizes to its wrapped content, and returns the text's computed size. The
    /// `render_tree` pass exercises the wrap **measure** (at `compute`); the paint re-shape is not hit here
    /// (paint early-returns without an atlas, before the re-shape). This is the realistic case: a flex-item
    /// text needs `min-width: 0` to shrink and wrap to its container (#260).
    fn text_in_width(t: Text, parent_w: f32) -> BSize {
        let mut root = crate::element::div()
            .size(BSize {
                w: parent_w,
                h: 400.0,
            })
            .items_start()
            .child(t)
            .into_element();
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut ts = TextSystem::new(FontDb::new());
        let mut scene = Scene::new();
        let damage = Arc::new(DamageState::default());
        let root_id = render_tree(
            &mut root,
            &mut arena,
            &mut layout,
            &mut ts,
            None,
            None,
            None,
            None,
            None,
            None,
            &mut scene,
            BSize { w: 400.0, h: 800.0 },
            &damage,
            &Theme::default(),
        )
        .unwrap();
        let text_id = arena
            .get(root_id)
            .and_then(|n| n.children.first().copied())
            .expect("the parent has the text as its child");
        layout.layout(text_id).size
    }

    #[test]
    fn text_wrap_should_break_to_available_width() {
        // In a fixed-width parent, `.wrap(true)` line-breaks the (long) text — laying out taller
        // (multi-line) and within the parent width — while `.wrap(false)` stays single-line, wider than
        // the parent (proving `min-width: 0` let the flex item shrink and wrap). #260.
        let long = "The quick brown fox jumps over the lazy dog";
        let wrapped = text_in_width(text(long).wrap(true), 80.0);
        let single = text_in_width(text(long).wrap(false), 80.0);
        assert!(
            wrapped.h > single.h,
            "wrapped text is taller (multi-line) than single-line: wrapped={wrapped:?} single={single:?}"
        );
        assert!(
            wrapped.w <= 80.5 && single.w > 80.5,
            "wrapped text fits the parent width while single-line overflows: wrapped={wrapped:?} single={single:?}"
        );
    }

    #[test]
    fn text_wrap_should_not_clip_height_in_short_flex_col() {
        // #260 regression: `min-width: 0` for wrapping must be **per-axis** (min-height stays `auto`), or a
        // wrapping text in a height-constrained overflowing flex column gets vertically shrunk/clipped. The
        // wrapped text keeps its full (multi-line) height, overflowing the short column.
        let long = "The quick brown fox jumps over the lazy dog wraps to several lines here";
        let short_h = 24.0; // ~ one line — the wrapped text is much taller
        let mut root = crate::element::div()
            .flex_col()
            .size(BSize {
                w: 90.0,
                h: short_h,
            })
            .child(text(long).wrap(true))
            .into_element();
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut ts = TextSystem::new(FontDb::new());
        let mut scene = Scene::new();
        let damage = Arc::new(DamageState::default());
        let root_id = render_tree(
            &mut root,
            &mut arena,
            &mut layout,
            &mut ts,
            None,
            None,
            None,
            None,
            None,
            None,
            &mut scene,
            BSize { w: 400.0, h: 400.0 },
            &damage,
            &Theme::default(),
        )
        .unwrap();
        let text_id = arena
            .get(root_id)
            .and_then(|n| n.children.first().copied())
            .expect("the column has the text as its child");
        let text_h = layout.layout(text_id).size.h;
        assert!(
            text_h > short_h,
            "the wrapped text keeps its full multi-line height (min-height stays auto), not clipped to the \
             short column: text_h={text_h} col_h={short_h}"
        );
    }

    #[test]
    fn text_reactive_content_should_reshape_on_signal() {
        // #274: a reactive content string re-shapes and relayouts live — a signal write flags layout-dirty
        // and the next frame measures the new (wider) content. Mirrors `icon_reactive_size_should_relayout`.
        // The content signal is written *between* frames (serial with `render_tree`), never during (RK-009).
        let owner = Owner::new();
        owner.set();
        let (read, write) = signal(SharedString::from("Hi"));
        let mut root = crate::element::div()
            .size(BSize { w: 400.0, h: 200.0 })
            .items_start()
            .child(text(rx(move || read.get())))
            .into_element();

        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut ts = TextSystem::new(FontDb::new());
        let damage = Arc::new(DamageState::default());
        let frame = |root: &mut AnyElement,
                     arena: &mut Arena,
                     layout: &mut LayoutTree,
                     ts: &mut TextSystem|
         -> f32 {
            let mut scene = Scene::new();
            let root_id = render_tree(
                root,
                arena,
                layout,
                ts,
                None,
                None,
                None,
                None,
                None,
                None,
                &mut scene,
                BSize { w: 400.0, h: 200.0 },
                &damage,
                &Theme::default(),
            )
            .unwrap();
            let text_id = arena
                .get(root_id)
                .and_then(|n| n.children.first().copied())
                .expect("the parent has the text as its child");
            layout.layout(text_id).size.w
        };

        let w0 = frame(&mut root, &mut arena, &mut layout, &mut ts);
        write.set("A much longer string of text".into());
        assert!(
            damage.is_dirty(),
            "the reactive content write flags damage (layout-dirty)"
        );
        let w1 = frame(&mut root, &mut arena, &mut layout, &mut ts);
        assert!(
            w1 > w0,
            "the signal write re-shapes the text to the new (longer) content: w0={w0} w1={w1}"
        );
        drop(owner);
    }

    #[test]
    fn text_static_content_should_measure_to_content() {
        // Regression: the `Prop` unification (#274) keeps the static shape-once path — longer static
        // content still lays out wider. (No reactive owner needed; a static text registers no effect.)
        let measure = |content: &'static str| -> f32 {
            let mut root = crate::element::div()
                .size(BSize { w: 400.0, h: 200.0 })
                .items_start()
                .child(text(content))
                .into_element();
            let mut arena = Arena::new();
            let mut layout = LayoutTree::new();
            let mut ts = TextSystem::new(FontDb::new());
            let mut scene = Scene::new();
            let damage = Arc::new(DamageState::default());
            let root_id = render_tree(
                &mut root,
                &mut arena,
                &mut layout,
                &mut ts,
                None,
                None,
                None,
                None,
                None,
                None,
                &mut scene,
                BSize { w: 400.0, h: 200.0 },
                &damage,
                &Theme::default(),
            )
            .unwrap();
            let text_id = arena
                .get(root_id)
                .and_then(|n| n.children.first().copied())
                .expect("the parent has the text as its child");
            layout.layout(text_id).size.w
        };
        let short = measure("Hi");
        let long = measure("Hi there, a much longer static string");
        assert!(
            long > short,
            "static content shapes to its size (shape-once path intact): short={short} long={long}"
        );
    }

    #[test]
    fn text_reactive_content_should_compose_with_reactive_color_role() {
        // #274 + #245: reactive content and reactive `color_role` are independent bindings on one `Text` —
        // a content write updates the shaped content (via the cell, read by the measure), a role write
        // re-resolves the tint. Built via `request_layout` so both bindings can be inspected directly.
        let owner = Owner::new();
        owner.set();
        let (rc, wc) = signal(SharedString::from("Hi"));
        let (rr, wr) = signal(false);
        let mut t = text(rx(move || rc.get())).color_role(rx(move || {
            if rr.get() {
                ColorRole::Danger
            } else {
                ColorRole::Text
            }
        }));

        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut ts = TextSystem::new(FontDb::new());
        let damage = Arc::new(DamageState::default());
        let sink: Arc<dyn DamageSink> = damage.clone();
        t.request_layout(&mut LayoutCx {
            arena: &mut arena,
            layout: &mut layout,
            text: &mut ts,
            damage: sink,
            theme: &Theme::default(),
            focus: None,
            cursor: None,
        });

        // Initial: both props resolved.
        assert_eq!(t.color_role.current(), Some(ColorRole::Text));
        assert_eq!(*t.content_cell.lock().unwrap(), SharedString::from("Hi"));

        // A role write re-resolves the tint (independent of content).
        wr.set(true);
        assert_eq!(t.color_role.current(), Some(ColorRole::Danger));

        // A content write updates the cell (the measure reads it on the next `compute`).
        wc.set("Hello, world".into());
        assert_eq!(
            *t.content_cell.lock().unwrap(),
            SharedString::from("Hello, world")
        );
        drop(owner);
    }
}
