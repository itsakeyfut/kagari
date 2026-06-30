//! Events & input (Phase 5, specs §1.5/§1.9): hit-test, dispatch, focus, actions, gestures, DnD,
//! cursor. So far: the [`HitTest`] structure (#47) and mouse [`dispatch_mouse`] (#48) — the
//! data-oriented "what is under the pointer?" query plus capture+bubble delivery the rest of the
//! input system builds on.

mod action;
mod dispatch;
mod focus;
mod hit_test;

pub use hit_test::{HitRegion, HitTest, InteractFlags};

// Public event/dispatch API.
pub use action::{Action, KeyChord, KeyContext, Keymap};
pub use dispatch::{
    DispatchState, KeyCode, KeyEvent, Modifiers, MouseButton, MouseEvent, MouseKind,
    dispatch_action, dispatch_key, dispatch_mouse,
};
pub use focus::{FocusHandle, FocusId, FocusRegistry};

// Crate-internal dispatch machinery used by the element layer: the delivery mode + capture request
// (`EventCx` fields, cx.rs), the listener tagging + handler types (`Div` storage, div.rs), and the
// ancestor-path walk (focus-within, focus.rs).
pub(crate) use action::ActionHandler;
pub(crate) use dispatch::{
    CaptureOp, Delivery, KeyHandler, KeyListenerKind, ListenerKind, MouseHandler, ancestor_path,
};
