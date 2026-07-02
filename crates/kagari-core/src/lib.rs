#![forbid(unsafe_code)]
//! kagari-core — element tree, reactive, damage, events, scheduler, and app shell.
//!
//! So far: the minimal app shell (a single window with wgpu init), the reactive runtime
//! wrapper (`reactive`), the retained node arena (`arena`), the element tree (`element`), the
//! paint pass (`paint`) that builds the render scene from the tree, fine-grained damage
//! tracking (`damage`), the hybrid frame scheduler (`scheduler`), and the events/input layer
//! (`event`) — so far the hit-test structure (#47).

pub mod anim;
pub mod app;
pub mod arena;
pub mod bridge;
pub mod damage;
pub mod element;
pub mod error;
pub mod event;
pub mod overlay;
pub mod paint;
pub mod reactive;
pub mod scheduler;

pub use anim::{Animatable, Animated, AnimationSpec, Easing};
pub use app::{App, Decorations, WindowOptions};
pub use bridge::UiProxy;
pub use damage::DamageState;
pub use element::{
    AnchorHandle, AnyElement, Element, IntoElement, Placement, ScrollHandle, div, overlay, scroll,
    text, virtualized_list,
};
pub use error::AppError;
pub use event::{
    Action, CursorIcon, CursorRegistry, CursorState, DispatchState, DragPayload, DragSource,
    DropTarget, FileDrop, FocusHandle, FocusId, FocusRegistry, GestureEvent, GestureRecognizer,
    HitRegion, HitTest, InteractFlags, KeyChord, KeyCode, KeyContext, KeyEvent, Keymap, Modifiers,
    MouseButton, MouseEvent, MouseKind, dispatch_action, dispatch_drag_leave, dispatch_drag_over,
    dispatch_drag_start, dispatch_drop, dispatch_gesture, dispatch_key, dispatch_mouse,
};
pub use kagari_base::WindowId;
pub use overlay::{OverlayEntry, OverlayRegistry};
pub use scheduler::{ActiveSources, Scheduler};
