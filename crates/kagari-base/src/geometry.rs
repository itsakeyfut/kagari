//! Bespoke geometry value types and their operations.
//!
//! All coordinates are `f32` logical pixels. These types carry no GPU/matrix
//! math (that lives in `kagari-render`); they are the shared vocabulary used
//! across the workspace for bounds, padding, and corner radii.

use std::ops::{Add, Mul, Sub};

/// A 2D point in logical pixels.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, Default)]
#[cfg_attr(feature = "bytemuck", derive(bytemuck::Pod, bytemuck::Zeroable))]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

/// A 2D size in logical pixels.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, Default)]
#[cfg_attr(feature = "bytemuck", derive(bytemuck::Pod, bytemuck::Zeroable))]
pub struct Size {
    pub w: f32,
    pub h: f32,
}

/// An axis-aligned rectangle described by its top-left origin and size.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, Default)]
#[cfg_attr(feature = "bytemuck", derive(bytemuck::Pod, bytemuck::Zeroable))]
pub struct Rect {
    pub origin: Point,
    pub size: Size,
}

/// Per-side lengths (padding, border, margin).
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, Default)]
#[cfg_attr(feature = "bytemuck", derive(bytemuck::Pod, bytemuck::Zeroable))]
pub struct Edges {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

/// Per-corner radii (top-left, top-right, bottom-right, bottom-left).
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, Default)]
#[cfg_attr(feature = "bytemuck", derive(bytemuck::Pod, bytemuck::Zeroable))]
pub struct Corners {
    pub tl: f32,
    pub tr: f32,
    pub br: f32,
    pub bl: f32,
}

impl Point {
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    /// Linear interpolation toward `other` by `t` (not clamped).
    pub fn lerp(self, other: Self, t: f32) -> Self {
        self + (other - self) * t
    }
}

impl Add for Point {
    type Output = Point;
    fn add(self, rhs: Point) -> Point {
        Point::new(self.x + rhs.x, self.y + rhs.y)
    }
}

impl Sub for Point {
    type Output = Point;
    fn sub(self, rhs: Point) -> Point {
        Point::new(self.x - rhs.x, self.y - rhs.y)
    }
}

impl Mul<f32> for Point {
    type Output = Point;
    fn mul(self, rhs: f32) -> Point {
        Point::new(self.x * rhs, self.y * rhs)
    }
}

impl Size {
    pub fn new(w: f32, h: f32) -> Self {
        Self { w, h }
    }

    /// Linear interpolation toward `other` by `t` (not clamped).
    pub fn lerp(self, other: Self, t: f32) -> Self {
        self + (other - self) * t
    }
}

impl Add for Size {
    type Output = Size;
    fn add(self, rhs: Size) -> Size {
        Size::new(self.w + rhs.w, self.h + rhs.h)
    }
}

impl Sub for Size {
    type Output = Size;
    fn sub(self, rhs: Size) -> Size {
        Size::new(self.w - rhs.w, self.h - rhs.h)
    }
}

impl Mul<f32> for Size {
    type Output = Size;
    fn mul(self, rhs: f32) -> Size {
        Size::new(self.w * rhs, self.h * rhs)
    }
}

impl Rect {
    pub fn new(origin: Point, size: Size) -> Self {
        Self { origin, size }
    }

    pub fn from_xywh(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self::new(Point::new(x, y), Size::new(w, h))
    }

    pub fn width(&self) -> f32 {
        self.size.w
    }

    pub fn height(&self) -> f32 {
        self.size.h
    }

    /// A rect with no area contributes nothing to hit-testing or damage.
    pub fn is_empty(&self) -> bool {
        self.size.w <= 0.0 || self.size.h <= 0.0
    }

    fn max_x(&self) -> f32 {
        self.origin.x + self.size.w
    }

    fn max_y(&self) -> f32 {
        self.origin.y + self.size.h
    }

    /// Half-open containment: the top-left edge is inclusive, the far edge is
    /// exclusive, so adjacent rects never both claim a shared boundary point.
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.origin.x && p.y >= self.origin.y && p.x < self.max_x() && p.y < self.max_y()
    }

    /// The overlapping region, or `None` when the rects are disjoint or merely
    /// touch (an empty overlap is not a rect).
    pub fn intersect(&self, other: Rect) -> Option<Rect> {
        let x0 = self.origin.x.max(other.origin.x);
        let y0 = self.origin.y.max(other.origin.y);
        let x1 = self.max_x().min(other.max_x());
        let y1 = self.max_y().min(other.max_y());
        if x1 > x0 && y1 > y0 {
            Some(Rect::from_xywh(x0, y0, x1 - x0, y1 - y0))
        } else {
            None
        }
    }

    /// The bounding rect of both. Used by core to merge damage regions, so an
    /// empty operand is treated as the identity (it must not enlarge the union).
    pub fn union(&self, other: Rect) -> Rect {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return *self;
        }
        let x0 = self.origin.x.min(other.origin.x);
        let y0 = self.origin.y.min(other.origin.y);
        let x1 = self.max_x().max(other.max_x());
        let y1 = self.max_y().max(other.max_y());
        Rect::from_xywh(x0, y0, x1 - x0, y1 - y0)
    }

    /// Shrink inward by per-side edges (negative edges grow the rect). When the
    /// edges exceed the size the result has a non-positive dimension and reads as
    /// empty via [`Rect::is_empty`]; it is not clamped.
    pub fn inset(&self, e: Edges) -> Rect {
        Rect::from_xywh(
            self.origin.x + e.left,
            self.origin.y + e.top,
            self.size.w - e.left - e.right,
            self.size.h - e.top - e.bottom,
        )
    }

    /// Move the origin by `d`, keeping the size.
    pub fn translate(&self, d: Point) -> Rect {
        Rect::new(self.origin + d, self.size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_should_include_origin_and_exclude_far_edge() {
        let r = Rect::from_xywh(0.0, 0.0, 10.0, 10.0);
        assert!(r.contains(Point::new(0.0, 0.0)));
        assert!(r.contains(Point::new(9.9, 9.9)));
        assert!(!r.contains(Point::new(10.0, 5.0)));
        assert!(!r.contains(Point::new(-0.1, 5.0)));
    }

    #[test]
    fn intersect_should_return_overlap_when_present() {
        let a = Rect::from_xywh(0.0, 0.0, 10.0, 10.0);
        let b = Rect::from_xywh(5.0, 5.0, 10.0, 10.0);
        assert_eq!(a.intersect(b), Some(Rect::from_xywh(5.0, 5.0, 5.0, 5.0)));
    }

    #[test]
    fn intersect_should_return_none_when_disjoint() {
        let a = Rect::from_xywh(0.0, 0.0, 10.0, 10.0);
        let b = Rect::from_xywh(20.0, 20.0, 5.0, 5.0);
        assert_eq!(a.intersect(b), None);
    }

    #[test]
    fn intersect_should_return_none_when_touching() {
        let a = Rect::from_xywh(0.0, 0.0, 10.0, 10.0);
        let b = Rect::from_xywh(10.0, 0.0, 10.0, 10.0);
        assert_eq!(a.intersect(b), None);
    }

    #[test]
    fn union_should_bound_both_rects() {
        let a = Rect::from_xywh(0.0, 0.0, 10.0, 10.0);
        let b = Rect::from_xywh(20.0, 20.0, 10.0, 10.0);
        assert_eq!(a.union(b), Rect::from_xywh(0.0, 0.0, 30.0, 30.0));
    }

    #[test]
    fn union_should_ignore_empty_operand() {
        let a = Rect::from_xywh(100.0, 100.0, 10.0, 10.0);
        let empty = Rect::default();
        assert_eq!(a.union(empty), a);
        assert_eq!(empty.union(a), a);
    }

    #[test]
    fn inset_should_shrink_by_edges() {
        let r = Rect::from_xywh(0.0, 0.0, 20.0, 20.0);
        let e = Edges {
            top: 1.0,
            right: 2.0,
            bottom: 3.0,
            left: 4.0,
        };
        assert_eq!(r.inset(e), Rect::from_xywh(4.0, 1.0, 14.0, 16.0));
    }

    #[test]
    fn translate_should_move_origin_only() {
        let r = Rect::from_xywh(1.0, 2.0, 10.0, 10.0);
        assert_eq!(
            r.translate(Point::new(3.0, 4.0)),
            Rect::from_xywh(4.0, 6.0, 10.0, 10.0)
        );
    }

    #[test]
    fn point_lerp_should_interpolate_linearly() {
        let a = Point::new(0.0, 0.0);
        let b = Point::new(10.0, 20.0);
        assert_eq!(a.lerp(b, 0.5), Point::new(5.0, 10.0));
    }

    #[test]
    fn point_ops_should_add_sub_and_scale() {
        let a = Point::new(1.0, 2.0);
        let b = Point::new(3.0, 4.0);
        assert_eq!(a + b, Point::new(4.0, 6.0));
        assert_eq!(b - a, Point::new(2.0, 2.0));
        assert_eq!(a * 2.0, Point::new(2.0, 4.0));
    }

    #[cfg(feature = "bytemuck")]
    #[test]
    fn point_should_be_tightly_packed_pod() {
        // No padding: the byte view is exactly two f32s (layout the GPU relies on).
        assert_eq!(bytemuck::bytes_of(&Point::new(1.0, 2.0)).len(), 8);
    }
}
