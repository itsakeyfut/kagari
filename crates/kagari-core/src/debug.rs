//! Dev-build debug overlay (#69, specs §8.3): a diagnostic HUD injected by the window shell **after**
//! `render_tree`, as post-render `Scene` primitives above all content — FPS/frame-time, damage-rect
//! visualization, element bounds, and atlas utilization. It is not an element and threads no frame
//! internals into the tree; it only reads kagari-core/render state.
//!
//! The whole module (and its wiring in `app.rs`) is `#[cfg(debug_assertions)]`, so it is compiled out
//! of release builds entirely (zero cost). At runtime it is toggled by a keybinding (F12).
//!
//! The HUD is redrawn only on damage-driven frames (§1.4): while nothing is dirty the window does not
//! repaint, so the readouts (FPS especially) hold their last value until the next redraw — expected
//! for a damage-driven renderer, not a stall.

use std::collections::VecDeque;
use std::time::Instant;

use kagari_base::{Color, Corners, Edges, NodeId, Point, Px, Rect};
use kagari_layout::LayoutTree;
use kagari_render::{Atlas, AtlasUsage, Background, Border, Quad, RoundedRect, Scene};
use kagari_text::{TextStyle, TextSystem, fontdb};

use crate::arena::Arena;

/// How many recent frame timestamps to average FPS / frame-time over.
const FRAME_HISTORY: usize = 120;

/// Painter-order band for the overlay — near `u32::MAX`, so it composites above all content and any
/// overlay/drag-image band (which sit at `1 << 28`). Sub-bands keep panel < fills < outlines < text.
const DEBUG_ORDER: u32 = u32::MAX - 16;

/// A rolling window of recent frame timestamps, for FPS / frame-time (the scheduler has no timing).
pub(crate) struct FrameClock {
    times: VecDeque<Instant>,
}

impl FrameClock {
    pub(crate) fn new() -> Self {
        Self {
            times: VecDeque::with_capacity(FRAME_HISTORY),
        }
    }

    /// Records a frame timestamp, dropping the oldest beyond [`FRAME_HISTORY`].
    pub(crate) fn record(&mut self, now: Instant) {
        if self.times.len() == FRAME_HISTORY {
            self.times.pop_front();
        }
        self.times.push_back(now);
    }

    /// Frames per second over the recorded window (0 with fewer than two samples).
    pub(crate) fn fps(&self) -> f32 {
        let span = self.span_secs();
        if span > 0.0 {
            (self.times.len() - 1) as f32 / span
        } else {
            0.0
        }
    }

    /// Mean frame time in milliseconds over the window (0 with fewer than two samples).
    pub(crate) fn frame_ms(&self) -> f32 {
        let span = self.span_secs();
        if span > 0.0 {
            span * 1000.0 / (self.times.len() - 1) as f32
        } else {
            0.0
        }
    }

    /// Seconds spanned by the recorded window, or 0 with fewer than two samples.
    fn span_secs(&self) -> f32 {
        match (self.times.front(), self.times.back()) {
            (Some(first), Some(last)) if self.times.len() >= 2 => {
                last.duration_since(*first).as_secs_f32()
            }
            _ => 0.0,
        }
    }
}

/// The per-frame diagnostics the overlay renders (gathered by the shell, drawn by
/// [`paint_debug_overlay`]).
pub(crate) struct Diagnostics {
    pub fps: f32,
    pub frame_ms: f32,
    pub mono_atlas: AtlasUsage,
    pub rgba_atlas: AtlasUsage,
    pub damage_rects: Vec<Rect>,
    pub element_bounds: Vec<Rect>,
}

/// Every arena node's **absolute** bounds, reachable from `root` (RK-007: taffy layout coords are
/// parent-relative, so accumulate ancestor origins top-down). Zero-size nodes are skipped.
pub(crate) fn collect_element_bounds(
    arena: &Arena,
    layout: &LayoutTree,
    root: NodeId,
) -> Vec<Rect> {
    let mut out = Vec::new();
    collect(arena, layout, root, Point::new(0.0, 0.0), &mut out);
    out
}

fn collect(
    arena: &Arena,
    layout: &LayoutTree,
    id: NodeId,
    parent_origin: Point,
    out: &mut Vec<Rect>,
) {
    let local = layout.layout(id);
    let abs = Rect {
        origin: Point::new(
            parent_origin.x + local.origin.x,
            parent_origin.y + local.origin.y,
        ),
        size: local.size,
    };
    if !abs.is_empty() {
        out.push(abs);
    }
    if let Some(node) = arena.get(id) {
        for &child in &node.children {
            collect(arena, layout, child, abs.origin, out);
        }
    }
}

/// A clip far larger than any primitive — the overlay is unclipped.
fn no_clip() -> RoundedRect {
    RoundedRect {
        rect: Rect::from_xywh(0.0, 0.0, 1.0e4, 1.0e4),
        radii: Corners::default(),
    }
}

/// A solid-fill quad (no border) at `order`.
fn fill_quad(bounds: Rect, color: Color, order: u32) -> Quad {
    Quad {
        bounds,
        corner_radii: Corners::default(),
        bg: Background::Solid(color),
        border: Border {
            widths: Edges::default(),
            color: Color::TRANSPARENT,
        },
        content_mask: no_clip(),
        order,
    }
}

/// A 1px-outlined quad (transparent fill) at `order` — an element-bounds box.
fn outline_quad(bounds: Rect, color: Color, order: u32) -> Quad {
    Quad {
        bounds,
        corner_radii: Corners::default(),
        bg: Background::Solid(Color::TRANSPARENT),
        border: Border {
            widths: Edges {
                top: 1.0,
                right: 1.0,
                bottom: 1.0,
                left: 1.0,
            },
            color,
        },
        content_mask: no_clip(),
        order,
    }
}

/// Injects the debug overlay into `scene` at the top painter-order band: a translucent fill per
/// damage rect, a thin outline per element bound, and (when an `atlas` is available) a dark HUD panel
/// with FPS / frame-time / atlas-utilization text. `atlas` is `None` in GPU-free tests, which then
/// assert only the quad output.
pub(crate) fn paint_debug_overlay(
    scene: &mut Scene,
    text: &mut TextSystem,
    atlas: Option<&mut Atlas>,
    diag: &Diagnostics,
) {
    // Damage rects: translucent orange fill.
    let damage_color = Color::new(1.0, 0.35, 0.0, 0.18);
    for &r in &diag.damage_rects {
        scene
            .quads
            .push(fill_quad(r, damage_color, DEBUG_ORDER + 1));
    }
    // Element bounds: thin translucent cyan outline.
    let bounds_color = Color::new(0.0, 0.9, 0.9, 0.5);
    for &r in &diag.element_bounds {
        scene
            .quads
            .push(outline_quad(r, bounds_color, DEBUG_ORDER + 2));
    }

    // The text HUD needs the glyph atlas; GPU-free tests pass `None` and skip it.
    let Some(atlas) = atlas else {
        return;
    };
    let lines = [
        format!("FPS {:.0}  ({:.1} ms)", diag.fps, diag.frame_ms),
        format!(
            "atlas glyph {:.0}%  image {:.0}%",
            diag.mono_atlas.fraction() * 100.0,
            diag.rgba_atlas.fraction() * 100.0
        ),
        format!(
            "damage {} rects  nodes {}",
            diag.damage_rects.len(),
            diag.element_bounds.len()
        ),
    ];

    // A dark panel behind the text (drawn under it so the text stays legible).
    let pad = 6.0;
    let line_h = 16.0;
    let (x0, y0) = (8.0, 8.0);
    let panel = Rect::from_xywh(
        x0 - pad,
        y0 - pad,
        260.0,
        pad * 2.0 + line_h * lines.len() as f32,
    );
    scene.quads.push(fill_quad(
        panel,
        Color::new(0.0, 0.0, 0.0, 0.6),
        DEBUG_ORDER,
    ));

    let style = TextStyle {
        family: "Noto Sans".into(),
        size: Px(13.0),
        weight: fontdb::Weight::NORMAL,
        line_height: None,
    };
    let text_color = Color::new(0.6, 1.0, 0.45, 1.0);
    for (i, line) in lines.iter().enumerate() {
        let shaped = text.shape(line, &style, None);
        let start = scene.glyphs.len();
        text.rasterize_into(&shaped, text_color, atlas, &mut scene.glyphs);
        // Offset the newly-appended glyphs to this line's position + stamp the overlay order (mirrors
        // `element/text.rs`; `rasterize_into` emits at text-local coords, order 0).
        let y = y0 + line_h * i as f32;
        for sprite in &mut scene.glyphs[start..] {
            sprite.bounds.origin.x += x0;
            sprite.bounds.origin.y += y;
            sprite.order = DEBUG_ORDER + 3;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arena::Node;
    use kagari_base::Size;
    use kagari_layout::{LayoutStyle, LayoutTree};
    use kagari_text::{FontDb, TextSystem};

    fn diag(damage: Vec<Rect>, bounds: Vec<Rect>) -> Diagnostics {
        Diagnostics {
            fps: 60.0,
            frame_ms: 16.6,
            mono_atlas: AtlasUsage::default(),
            rgba_atlas: AtlasUsage::default(),
            damage_rects: damage,
            element_bounds: bounds,
        }
    }

    #[test]
    fn debug_overlay_should_emit_quads_for_damage_rects() {
        // For a known frame with N damage rects (and no element bounds, no atlas), the pass injects
        // exactly N quads and no HUD glyphs.
        let mut scene = Scene::new();
        let mut text = TextSystem::new(FontDb::new());
        let rects = vec![
            Rect::from_xywh(0.0, 0.0, 10.0, 10.0),
            Rect::from_xywh(20.0, 20.0, 5.0, 5.0),
            Rect::from_xywh(1.0, 1.0, 2.0, 2.0),
        ];
        paint_debug_overlay(
            &mut scene,
            &mut text,
            None,
            &diag(rects.clone(), Vec::new()),
        );
        assert_eq!(scene.quads.len(), rects.len(), "one quad per damage rect");
        assert!(scene.glyphs.is_empty(), "no HUD glyphs without an atlas");
    }

    #[test]
    fn debug_overlay_should_outline_element_bounds() {
        let mut scene = Scene::new();
        let mut text = TextSystem::new(FontDb::new());
        let bounds = vec![
            Rect::from_xywh(0.0, 0.0, 40.0, 20.0),
            Rect::from_xywh(5.0, 5.0, 10.0, 10.0),
        ];
        paint_debug_overlay(
            &mut scene,
            &mut text,
            None,
            &diag(Vec::new(), bounds.clone()),
        );
        assert_eq!(
            scene.quads.len(),
            bounds.len(),
            "one outline quad per element bound"
        );
        assert!(
            scene.quads.iter().all(|q| q.border.widths.top > 0.0),
            "element-bound quads are drawn as outlines (non-zero border)"
        );
    }

    #[test]
    fn collect_element_bounds_should_return_a_rect_per_node() {
        // root (100x100 column) → [a (100x20), b (100x30)]; three sized nodes → three rects.
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let root = arena.insert(Node::default());
        let a = arena.insert(Node::default());
        let b = arena.insert(Node::default());
        arena.get_mut(a).unwrap().parent = Some(root);
        arena.get_mut(b).unwrap().parent = Some(root);
        arena.get_mut(root).unwrap().children = vec![a, b];

        layout.insert(root, None);
        layout.insert(a, Some(root));
        layout.insert(b, Some(root));
        layout.set_style(
            root,
            &LayoutStyle {
                flex_direction: kagari_layout::FlexDirection::Column,
                size: Some(Size { w: 100.0, h: 100.0 }),
                ..LayoutStyle::default()
            },
        );
        layout.set_style(
            a,
            &LayoutStyle {
                size: Some(Size { w: 100.0, h: 20.0 }),
                ..LayoutStyle::default()
            },
        );
        layout.set_style(
            b,
            &LayoutStyle {
                size: Some(Size { w: 100.0, h: 30.0 }),
                ..LayoutStyle::default()
            },
        );
        layout.set_children(root, &[a, b]);
        layout.compute(root, Size { w: 100.0, h: 100.0 }).unwrap();

        let bounds = collect_element_bounds(&arena, &layout, root);
        assert_eq!(bounds.len(), 3, "root + two children");
        // `b` sits below `a` (20px tall), so some node's absolute y is >= 20 (ancestor accumulation).
        assert!(
            bounds.iter().any(|r| r.origin.y >= 20.0),
            "the second child's absolute y accumulates the first child's height"
        );
    }
}
