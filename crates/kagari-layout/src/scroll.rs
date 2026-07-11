//! Scroll geometry (#60): pure offset / clip / overlay-thumb math plus the fixed-size
//! visible-range calc (#61). Shared by the core `Scroll` and `VirtualizedList` elements.
//! Element/GPU-free — just value types, so it is unit-testable in isolation.

use std::ops::Range;

use kagari_base::{Point, Size};

/// Minimum overlay-scrollbar thumb length (logical px) so a tiny thumb stays grabbable.
const MIN_THUMB: f32 = 16.0;

/// Time constant (seconds) for the inertial fling projection: a fling of `velocity` px/sec is
/// projected `velocity · FLING_TAU` px ahead before it settles. Tunable — the inertial-scroll physics
/// tuning is post-MVP (specs §4.3); this pairs with a near-critically-damped scroll spring.
const FLING_TAU: f32 = 0.1;

/// A scroll viewport's state: the current `offset` and the laid-out `content` extent. The container
/// (viewport) size is passed per call since it comes fresh from layout each frame.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct ScrollState {
    pub offset: Point,
    pub content: Size,
}

impl ScrollState {
    /// The maximum scroll offset per axis: `max(0, content - viewport)`. Zero where content fits.
    pub fn max_offset(&self, viewport: Size) -> Point {
        Point::new(
            (self.content.w - viewport.w).max(0.0),
            (self.content.h - viewport.h).max(0.0),
        )
    }

    /// `offset` clamped to `[0, max_offset]` per axis, so over-scroll never reveals blank space.
    pub fn clamp(&self, viewport: Size) -> Point {
        let max = self.max_offset(viewport);
        Point::new(
            self.offset.x.clamp(0.0, max.x),
            self.offset.y.clamp(0.0, max.y),
        )
    }
}

/// Overlay-scrollbar thumb geometry along one axis: `(pos, len)` within a `track`-long bar, or
/// `None` when the content fits (no scrollbar needed). `len` is the content/viewport ratio (clamped
/// to a grabbable minimum); `pos` maps the clamped `offset` across the free track. `offset` need not
/// be pre-clamped — it is clamped here.
pub fn thumb(viewport: f32, content: f32, offset: f32, track: f32) -> Option<(f32, f32)> {
    if content <= viewport || content <= 0.0 || track <= 0.0 {
        return None;
    }
    // `MIN_THUMB.min(track)` keeps the clamp's lower bound ≤ upper bound (track) — a track shorter
    // than the minimum yields a full-track thumb rather than panicking.
    let len = (track * viewport / content).clamp(MIN_THUMB.min(track), track);
    let max_off = (content - viewport).max(0.0);
    let pos = if max_off > 0.0 {
        (offset.clamp(0.0, max_off) / max_off) * (track - len)
    } else {
        0.0
    };
    Some((pos, len))
}

/// The offset a scrollbar-thumb drag lands at: the inverse of [`thumb`]'s `pos` map, using the SAME
/// `MIN_THUMB`-clamped thumb length so the grabbed point tracks the cursor 1:1 even for tall content
/// (where the naive `delta · content / track` factor drifts). `delta` is the pointer's move along the
/// axis since the grab; the result is clamped to `[0, content - viewport]`. Returns `start_offset`
/// unchanged for a non-overflowing or degenerate axis (`content ≤ viewport`, `track ≤ 0`, or a thumb
/// that fills the track) — so a divide by a zero free-track never produces `NaN`/`Inf`.
pub fn thumb_drag_to_offset(
    start_offset: f32,
    delta: f32,
    viewport: f32,
    content: f32,
    track: f32,
) -> f32 {
    if content <= viewport || track <= 0.0 {
        return start_offset;
    }
    let len = (track * viewport / content).clamp(MIN_THUMB.min(track), track);
    let free = track - len;
    if free <= 0.0 {
        return start_offset;
    }
    let max_off = content - viewport;
    (start_offset + delta * max_off / free).clamp(0.0, max_off)
}

/// The projected rest offset of an inertial fling (#176): `offset + velocity · FLING_TAU`, clamped per
/// axis to `[0, max_offset]` so the fling never rests past the content. The scroll spring eases into
/// this target seeded with `velocity`, so the motion carries forward before settling. `velocity` is in
/// logical px/sec. Pure geometry (Element/GPU-free), shared by `ScrollHandle::fling`.
pub fn fling_target(offset: Point, velocity: Point, content: Size, viewport: Size) -> Point {
    let projected = Point::new(
        offset.x + velocity.x * FLING_TAU,
        offset.y + velocity.y * FLING_TAU,
    );
    ScrollState {
        offset: projected,
        content,
    }
    .clamp(viewport)
}

/// The half-open index range `[start, end)` of fixed-height rows intersecting the viewport (#61),
/// widened by `buffer` rows each side and clamped to `0..=count`. `start = floor(offset_y / h)`,
/// `end = ceil((offset_y + viewport_h) / h)`. An over-scrolled or negative offset yields an empty or
/// tail range rather than an out-of-bounds one; a zero item height or empty list yields `0..0`.
pub fn visible_range(
    offset_y: f32,
    viewport_h: f32,
    item_height: f32,
    count: usize,
    buffer: usize,
) -> Range<usize> {
    if item_height <= 0.0 || count == 0 {
        return 0..0;
    }
    let first = (offset_y / item_height).floor();
    let last = ((offset_y + viewport_h.max(0.0)) / item_height).ceil();
    // `f32 as usize` saturates (negative → 0, huge → usize::MAX) in Rust, so the clamps below are
    // safe against extreme offsets.
    let first = if first > 0.0 { first as usize } else { 0 };
    let last = if last > 0.0 { last as usize } else { 0 };
    let start = first.saturating_sub(buffer).min(count);
    let end = last.saturating_add(buffer).min(count);
    start..end.max(start)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scroll_state_should_clamp_offset_to_content() {
        // content 300 tall in a 100 viewport → max offset 200; an over-scrolled 500 clamps to 200.
        // The x axis has no overflow (content 100 == viewport 100), so its offset clamps to 0.
        let s = ScrollState {
            offset: Point::new(50.0, 500.0),
            content: Size::new(100.0, 300.0),
        };
        let viewport = Size::new(100.0, 100.0);
        assert_eq!(s.max_offset(viewport), Point::new(0.0, 200.0));
        assert_eq!(s.clamp(viewport), Point::new(0.0, 200.0));
    }

    #[test]
    fn thumb_should_reflect_viewport_content_ratio() {
        // A 100 viewport over 400 content → the thumb is 1/4 of the 100-long track = 25px.
        let (pos, len) = thumb(100.0, 400.0, 0.0, 100.0).expect("overflow → a thumb");
        assert_eq!(
            len, 25.0,
            "thumb length is the viewport/content ratio of the track"
        );
        assert_eq!(pos, 0.0, "at offset 0 the thumb sits at the track start");
        // Scrolled halfway (offset = max/2 = 150) → pos = 0.5 * (100 - 25) = 37.5.
        let (pos_mid, _) = thumb(100.0, 400.0, 150.0, 100.0).unwrap();
        assert_eq!(
            pos_mid, 37.5,
            "the thumb maps the offset across the free track"
        );
    }

    #[test]
    fn thumb_should_be_none_when_content_fits() {
        assert!(
            thumb(100.0, 80.0, 0.0, 100.0).is_none(),
            "content smaller than the viewport shows no thumb"
        );
        assert!(
            thumb(100.0, 100.0, 0.0, 100.0).is_none(),
            "content equal to the viewport shows no thumb"
        );
    }

    #[test]
    fn thumb_drag_to_offset_should_invert_thumb_pos() {
        // 100 vp / 400 content / 100 track → thumb len 25, free 75, max_off 300 (factor 4).
        // Dragging the thumb the full free track (75) from offset 0 lands exactly at max_off (300).
        assert_eq!(thumb_drag_to_offset(0.0, 75.0, 100.0, 400.0, 100.0), 300.0);
        // A half-free-track drag (37.5) from 0 → 150, the exact inverse of thumb()'s pos=37.5 at offset 150.
        assert_eq!(thumb_drag_to_offset(0.0, 37.5, 100.0, 400.0, 100.0), 150.0);
        // Clamped to [0, max_off]: dragging past the top pins at 0, past the bottom pins at max_off.
        assert_eq!(
            thumb_drag_to_offset(150.0, -1000.0, 100.0, 400.0, 100.0),
            0.0
        );
        assert_eq!(
            thumb_drag_to_offset(150.0, 1000.0, 100.0, 400.0, 100.0),
            300.0
        );
    }

    #[test]
    fn thumb_drag_to_offset_should_use_min_thumb_length_for_tall_content() {
        // Tall content clamps the thumb to MIN_THUMB (16): the naive delta·content/track factor would
        // under-move and leave an end-of-track dead zone. vp 100 / content 10000 / track 100 → len 16,
        // free 84, max_off 9900 → dragging the full free track (84) reaches max_off exactly.
        assert_eq!(
            thumb_drag_to_offset(0.0, 84.0, 100.0, 10000.0, 100.0),
            9900.0
        );
    }

    #[test]
    fn thumb_drag_to_offset_should_noop_on_degenerate() {
        // Content fits → no thumb → the start offset is returned unchanged.
        assert_eq!(thumb_drag_to_offset(5.0, 50.0, 100.0, 80.0, 100.0), 5.0);
        // A zero track must not divide-by-zero into NaN/Inf — returns the (finite) start offset.
        let r = thumb_drag_to_offset(5.0, 50.0, 100.0, 400.0, 0.0);
        assert_eq!(r, 5.0);
        assert!(r.is_finite());
    }

    #[test]
    fn fling_target_should_project_and_clamp() {
        // content 300 in a 100 viewport → max offset 200. A downward fling of 500 px/sec projects
        // 0 + 500·0.1 = 50 px ahead (within bounds); a hard 5000 projects 500 → clamps to 200.
        let content = Size::new(100.0, 300.0);
        let viewport = Size::new(100.0, 100.0);
        assert_eq!(
            fling_target(
                Point::new(0.0, 0.0),
                Point::new(0.0, 500.0),
                content,
                viewport
            ),
            Point::new(0.0, 50.0),
            "a gentle fling projects offset + velocity·tau, within bounds"
        );
        assert_eq!(
            fling_target(
                Point::new(0.0, 0.0),
                Point::new(0.0, 5000.0),
                content,
                viewport
            ),
            Point::new(0.0, 200.0),
            "a hard fling clamps to the max offset (no rest past the content)"
        );
        assert_eq!(
            fling_target(
                Point::new(0.0, 20.0),
                Point::new(0.0, -1000.0),
                content,
                viewport
            ),
            Point::new(0.0, 0.0),
            "an upward fling past the top clamps to 0"
        );
    }

    #[test]
    fn visible_range_should_cover_viewport_with_buffer() {
        // A 100 viewport of 20px rows shows 5 rows (0..5); the ±2 buffer widens it, clamped at the
        // top to 0. offset 0 → floor 0, ceil(100/20)=5 → 0.saturating_sub(2)=0 .. 5+2=7.
        assert_eq!(visible_range(0.0, 100.0, 20.0, 1000, 2), 0..7);
    }

    #[test]
    fn visible_range_should_offset_the_window() {
        // offset 200 → first = floor(200/20)=10, last = ceil(300/20)=15; ±2 buffer → 8..17.
        assert_eq!(visible_range(200.0, 100.0, 20.0, 1000, 2), 8..17);
    }

    #[test]
    fn visible_range_should_clamp_to_count() {
        // Near the end of a 1000-row list, `end` clamps to `count` (no out-of-bounds index).
        // offset 19980 → first=999, last=ceil(20080/20)=1004; start=997, end=1004→1000.
        assert_eq!(visible_range(19980.0, 100.0, 20.0, 1000, 2), 997..1000);
    }

    #[test]
    fn visible_range_should_be_empty_for_zero_items() {
        assert_eq!(visible_range(0.0, 100.0, 20.0, 0, 2), 0..0);
        // A zero item height is degenerate (no O(1) math) → empty, not a divide-by-zero.
        assert_eq!(visible_range(0.0, 100.0, 0.0, 1000, 2), 0..0);
    }
}
