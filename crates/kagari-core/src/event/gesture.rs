//! Gesture recognition (#51, specs §1.9): a first-class recognizer turning raw scroll/touchpad and
//! modifier input into high-level [`GestureEvent`]s (pan / zoom), dispatched through the hit/bubble
//! path (#48) to `on_gesture` handlers.
//!
//! MVP recognizes **pan and zoom**; rotate and momentum/inertia (anim §1.10, not yet built) are
//! post-MVP — the `#[non_exhaustive]` enum lets them be added without a breaking change. The live
//! winit wiring (`MouseWheel`/`TouchpadMagnify` → recognizer → dispatch) and the plain-wheel→Pan vs
//! #48 `on_wheel` coexistence routing land with the deferred live mouse wiring; this module is the
//! recognizer + event they use.

use kagari_base::Point;

use crate::element::EventCx;
use crate::event::Modifiers;

/// Multiplicative zoom per unit of ctrl+wheel scroll: `factor = ZOOM_BASE^dy`. Always positive and
/// symmetric (dy>0 zooms in, dy<0 out). Tunable; a gentle 10% per notch.
const ZOOM_BASE: f32 = 1.1;

/// A recognized high-level gesture, dispatched up the hit/bubble path to `on_gesture` handlers.
#[derive(Clone, Copy, PartialEq, Debug)]
#[non_exhaustive]
pub enum GestureEvent {
    /// A pan (scroll) by a logical-pixel delta — from a plain wheel or two-finger scroll.
    Pan { dx: f32, dy: f32 },
    /// A zoom by a multiplicative `factor` (> 0) about `center` (the cursor) — from ctrl+wheel or a
    /// native touchpad magnify.
    Zoom { factor: f32, center: Point },
}

/// A per-window gesture recognizer: maps raw scroll/touchpad input into [`GestureEvent`]s. Holds the
/// cursor position (the zoom center), updated from `CursorMoved` by the live wiring. The cursor is the
/// only MVP state; post-MVP momentum/rotate state extends this.
#[derive(Default)]
pub struct GestureRecognizer {
    cursor: Point,
}

impl GestureRecognizer {
    /// Creates a recognizer with the cursor at the origin.
    pub fn new() -> Self {
        Self::default()
    }

    /// Updates the tracked cursor position (the zoom center).
    pub fn set_cursor(&mut self, cursor: Point) {
        self.cursor = cursor;
    }

    /// Recognizes a scroll: with `ctrl` held it is a zoom about the cursor (the cross-platform
    /// ctrl+wheel convention), otherwise a pan by the raw delta.
    pub fn recognize_scroll(&self, dx: f32, dy: f32, modifiers: Modifiers) -> GestureEvent {
        if modifiers.ctrl {
            GestureEvent::Zoom {
                factor: ZOOM_BASE.powf(dy),
                center: self.cursor,
            }
        } else {
            GestureEvent::Pan { dx, dy }
        }
    }

    /// Recognizes a native touchpad magnify (winit `TouchpadMagnify`): a zoom about the cursor.
    /// `delta` is winit's scale delta (e.g. `0.2` = +20%), so `factor = 1.0 + delta`.
    pub fn recognize_magnify(&self, delta: f32) -> GestureEvent {
        GestureEvent::Zoom {
            factor: 1.0 + delta,
            center: self.cursor,
        }
    }
}

/// A boxed gesture handler stored on an element (#51). Imperative (writes to signals — e.g. a timeline
/// zoom / scroll offset), not a reactive effect — runs as a gesture bubbles the hit path.
pub(crate) type GestureHandler = Box<dyn FnMut(&GestureEvent, &mut EventCx)>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctrl_wheel_should_recognize_zoom() {
        let mut r = GestureRecognizer::new();
        r.set_cursor(Point::new(30.0, 40.0));
        let g = r.recognize_scroll(
            0.0,
            1.0,
            Modifiers {
                ctrl: true,
                ..Modifiers::default()
            },
        );
        match g {
            GestureEvent::Zoom { factor, center } => {
                assert!(factor > 1.0, "scrolling up with ctrl zooms in (factor > 1)");
                assert_eq!(center, Point::new(30.0, 40.0), "zoom centers on the cursor");
            }
            other => panic!("expected Zoom, got {other:?}"),
        }
    }

    #[test]
    fn plain_wheel_should_recognize_pan() {
        let r = GestureRecognizer::new();
        assert_eq!(
            r.recognize_scroll(3.0, -5.0, Modifiers::default()),
            GestureEvent::Pan { dx: 3.0, dy: -5.0 },
            "a plain scroll pans by the raw delta"
        );
    }

    #[test]
    fn magnify_should_recognize_zoom() {
        let mut r = GestureRecognizer::new();
        r.set_cursor(Point::new(5.0, 6.0));
        assert_eq!(
            r.recognize_magnify(0.2),
            GestureEvent::Zoom {
                factor: 1.2,
                center: Point::new(5.0, 6.0)
            },
            "a native magnify of +0.2 is a 1.2x zoom about the cursor"
        );
    }
}
