//! A paint-space uniform transform: uniform `scale` + `offset` (translate), applied as `p*scale + offset`
//! to points and rects (#221). taffy lays out at base scale; this maps paint + hit-test geometry, so a
//! zoom re-paints (cheap) and never re-lays-out (big-canvas friendly, specs §4.3).
//!
//! MVP is uniform scale + translate; rotation/skew are post-MVP (adding them is a breaking change —
//! `scale: f32` cannot express rotation — which is acceptable pre-1.0).

use crate::geometry::{Point, Rect};

/// A uniform scale + translate applied in paint/hit space: `map(p) = p * scale + offset` (#221).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Transform {
    /// Uniform scale factor (`1.0` = no scaling). Kept `> 0` by the producing element.
    pub scale: f32,
    /// Translation applied *after* scaling (logical px, window space).
    pub offset: Point,
}

impl Transform {
    /// The identity transform (`scale 1`, no offset) — maps every point to itself.
    pub const IDENTITY: Transform = Transform {
        scale: 1.0,
        offset: Point { x: 0.0, y: 0.0 },
    };

    /// A transform with uniform `scale` and `offset`.
    pub fn new(scale: f32, offset: Point) -> Self {
        Self { scale, offset }
    }

    /// Maps a point into window space: `p * scale + offset`.
    pub fn map_point(&self, p: Point) -> Point {
        p * self.scale + self.offset
    }

    /// Maps a rect: its origin by [`map_point`](Self::map_point), its size by `scale`.
    pub fn map_rect(&self, r: Rect) -> Rect {
        Rect::new(self.map_point(r.origin), r.size * self.scale)
    }

    /// Composes `self` *after* `inner`: the result maps `p` as `self.map(inner.map(p))` — the window
    /// mapping of a nested transform (`self` the outer, `inner` the inner).
    pub fn compose(&self, inner: Transform) -> Transform {
        Transform {
            scale: self.scale * inner.scale,
            offset: self.offset + inner.offset * self.scale,
        }
    }

    /// The inverse transform, for window→content mapping. A degenerate `scale == 0` yields
    /// [`IDENTITY`](Self::IDENTITY) rather than dividing by zero (a zero-scale subtree paints nothing, so
    /// its inverse is never meaningfully used).
    pub fn inverse(&self) -> Transform {
        if self.scale == 0.0 {
            return Transform::IDENTITY;
        }
        let inv = 1.0 / self.scale;
        Transform {
            scale: inv,
            offset: self.offset * -inv,
        }
    }

    /// Maps a window-space point back to content space (`(p - offset) / scale`) — a convenience for a
    /// consumer doing window→content pointer mapping (e.g. a zoom-pan canvas, #237).
    pub fn inverse_point(&self, p: Point) -> Point {
        self.inverse().map_point(p)
    }
}

impl Default for Transform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transform_should_map_point_and_rect() {
        let t = Transform::new(2.0, Point::new(10.0, 5.0));
        assert_eq!(t.map_point(Point::new(3.0, 4.0)), Point::new(16.0, 13.0));
        let r = t.map_rect(Rect::from_xywh(1.0, 1.0, 4.0, 6.0));
        assert_eq!(r, Rect::from_xywh(12.0, 7.0, 8.0, 12.0));
    }

    #[test]
    fn transform_compose_should_nest_outer_after_inner() {
        // outer(scale 3) ∘ inner(scale 2) applied to p → outer(inner(p)).
        let inner = Transform::new(2.0, Point::new(1.0, 0.0));
        let outer = Transform::new(3.0, Point::new(0.0, 4.0));
        let composed = outer.compose(inner);
        let p = Point::new(5.0, 5.0);
        assert_eq!(composed.map_point(p), outer.map_point(inner.map_point(p)));
        assert_eq!(composed.scale, 6.0);
    }

    #[test]
    fn transform_inverse_should_round_trip() {
        let t = Transform::new(2.5, Point::new(7.0, -3.0));
        let p = Point::new(9.0, 12.0);
        let back = t.inverse_point(t.map_point(p));
        assert!((back.x - p.x).abs() < 1e-4 && (back.y - p.y).abs() < 1e-4);
        // Degenerate scale is safe (identity, no divide-by-zero).
        assert_eq!(
            Transform::new(0.0, Point::new(1.0, 1.0)).inverse(),
            Transform::IDENTITY
        );
    }
}
