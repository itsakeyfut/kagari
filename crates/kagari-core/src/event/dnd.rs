//! Drag and drop (#52, specs §1.9): a typed internal DnD model — a type-erased [`DragPayload`], a
//! [`DragSource`] that produces a payload, and a [`DropTarget`] that accepts a payload of a concrete
//! type (downcast) and rejects others — plus the [`FileDrop`] payload for OS file drop.
//!
//! This is the headless typed model. The mouse-driven drag flow (down → move-threshold → hover →
//! drop), the tree drop *delivery*, the OS file-drop winit wiring, and the per-target reactive
//! hover-feedback signal land with the deferred live mouse wiring (deferred since #48). The floating
//! drag-image is relocated to Phase 6 (overlay §4.2). hover feedback is decided by
//! [`DropTarget::accepts`]; the signal-toggle that highlights the target is the live part.

use std::any::{Any, TypeId};
use std::path::PathBuf;

use crate::element::EventCx;

/// A type-erased drag payload — any `'static` value can be one. `as_any` gives a borrow for a
/// non-consuming type check; `into_any` moves the value out of its box for a consuming downcast to
/// the concrete type.
pub trait DragPayload: Any {
    fn as_any(&self) -> &dyn Any;
    fn into_any(self: Box<Self>) -> Box<dyn Any>;
}

impl<T: Any> DragPayload for T {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

/// The payload an OS file drop carries (winit `DroppedFile`): the dropped paths. Uses [`PathBuf`] (no
/// OS-specific literals — rust.md). A file-accepting element uses `drop_target::<FileDrop>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDrop(pub Vec<PathBuf>);

/// A drag source: produces a fresh boxed [`DragPayload`] when a drag begins.
pub struct DragSource {
    produce: Box<dyn FnMut() -> Box<dyn DragPayload>>,
}

impl DragSource {
    /// Builds a drag source from a payload producer.
    pub fn new<P: DragPayload>(mut produce: impl FnMut() -> P + 'static) -> Self {
        Self {
            produce: Box::new(move || Box::new(produce())),
        }
    }

    /// Produces a payload for a new drag.
    pub fn payload(&mut self) -> Box<dyn DragPayload> {
        (self.produce)()
    }
}

/// A type-erased drop handler: receives the (type-matched) payload as `Box<dyn Any>` and downcasts
/// it to the target's concrete type before running the user handler.
type ErasedDrop = Box<dyn FnMut(Box<dyn Any>, &mut EventCx)>;

/// A drop target: accepts a payload of one concrete type `T` and runs a handler with it; payloads of
/// any other type are rejected.
pub struct DropTarget {
    accepted: TypeId,
    on_drop: ErasedDrop,
}

impl DropTarget {
    /// Builds a drop target accepting payloads of type `T`. The stored handler downcasts the dropped
    /// payload to `T` (guaranteed to match — [`try_drop`](Self::try_drop) only calls it on a type match)
    /// and runs `handler` with the owned value.
    pub fn new<T: Any>(mut handler: impl FnMut(T, &mut EventCx) + 'static) -> Self {
        Self {
            accepted: TypeId::of::<T>(),
            on_drop: Box::new(move |payload, cx| {
                if let Ok(payload) = payload.downcast::<T>() {
                    handler(*payload, cx);
                }
            }),
        }
    }

    /// Whether this target accepts `payload` (its concrete type matches `T`). Used to decide hover
    /// feedback (highlight an accepting target) and to validate a drop before delivering it.
    pub fn accepts(&self, payload: &dyn DragPayload) -> bool {
        payload.as_any().type_id() == self.accepted
    }

    /// Delivers a drop: if `payload`'s type matches, moves it out and runs the handler, returning
    /// `true`; otherwise rejects it (returns `false`, leaving the caller to try another target).
    pub fn try_drop(&mut self, payload: Box<dyn DragPayload>, cx: &mut EventCx) -> bool {
        // Deref to the inner `dyn DragPayload`: `Box<dyn DragPayload>` itself impls `DragPayload`
        // (blanket impl), so `payload.as_any()` would otherwise resolve to the box, not the payload.
        if (*payload).as_any().type_id() == self.accepted {
            (self.on_drop)(payload.into_any(), cx);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arena::Arena;
    use crate::event::Delivery;
    use kagari_base::NodeId;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Debug, PartialEq)]
    struct Foo(u32);
    #[derive(Debug, PartialEq)]
    struct Bar(u32);

    /// A minimal `EventCx` for exercising `try_drop` (no dispatch path is walked — drop handlers
    /// ignore it here). `EventCx::new` + `Delivery` are crate-internal.
    fn with_cx(f: impl FnOnce(&mut EventCx)) {
        let arena = Arena::new();
        let path: &[NodeId] = &[];
        let mut cx = EventCx::new(&arena, Delivery::Bubble { path });
        f(&mut cx);
    }

    #[test]
    fn drop_target_should_accept_matching_payload() {
        let got: Rc<RefCell<Option<u32>>> = Rc::new(RefCell::new(None));
        let g = Rc::clone(&got);
        let mut target = DropTarget::new::<Foo>(move |foo, _cx| *g.borrow_mut() = Some(foo.0));
        with_cx(|cx| {
            assert!(
                target.try_drop(Box::new(Foo(7)), cx),
                "a matching-type payload is accepted"
            );
        });
        assert_eq!(
            *got.borrow(),
            Some(7),
            "the handler receives the downcast payload"
        );
    }

    #[test]
    fn drop_target_should_reject_wrong_type() {
        let fired = Rc::new(RefCell::new(false));
        let f = Rc::clone(&fired);
        let mut target = DropTarget::new::<Foo>(move |_foo, _cx| *f.borrow_mut() = true);
        with_cx(|cx| {
            assert!(
                !target.try_drop(Box::new(Bar(1)), cx),
                "a wrong-type payload is rejected"
            );
        });
        assert!(
            !*fired.borrow(),
            "the handler does not fire for the wrong type"
        );
    }

    #[test]
    fn hover_should_flag_accepting_target() {
        let target = DropTarget::new::<Foo>(|_foo: Foo, _cx| {});
        assert!(
            target.accepts(&Foo(0)),
            "accepts a matching-type payload (would highlight)"
        );
        assert!(
            !target.accepts(&Bar(0)),
            "does not accept a wrong-type payload"
        );
    }

    #[test]
    fn drag_source_to_matching_target_should_drop() {
        // The end-to-end model flow: a source produces a payload, a type-matching target consumes it.
        let mut source = DragSource::new(|| Foo(42));
        let got: Rc<RefCell<Option<u32>>> = Rc::new(RefCell::new(None));
        let g = Rc::clone(&got);
        let mut target = DropTarget::new::<Foo>(move |foo, _cx| *g.borrow_mut() = Some(foo.0));
        with_cx(|cx| {
            assert!(target.try_drop(source.payload(), cx));
        });
        assert_eq!(
            *got.borrow(),
            Some(42),
            "the source's payload reaches the target"
        );
    }

    #[test]
    fn file_drop_should_reach_file_target() {
        let got: Rc<RefCell<Vec<PathBuf>>> = Rc::new(RefCell::new(Vec::new()));
        let g = Rc::clone(&got);
        let mut target = DropTarget::new::<FileDrop>(move |drop, _cx| *g.borrow_mut() = drop.0);
        let files = vec![PathBuf::from("/tmp/a.png"), PathBuf::from("/tmp/b.mp4")];
        with_cx(|cx| {
            assert!(target.try_drop(Box::new(FileDrop(files.clone())), cx));
        });
        assert_eq!(
            *got.borrow(),
            files,
            "the file paths reach a FileDrop target"
        );
    }
}
