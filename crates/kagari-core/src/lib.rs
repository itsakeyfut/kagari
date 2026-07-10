#![forbid(unsafe_code)]
//! kagari-core — element tree, reactive, damage, events, scheduler, and app shell.
//!
//! So far: the minimal app shell (a single window with wgpu init), the reactive runtime
//! wrapper (`reactive`), the retained node arena (`arena`), the element tree (`element`), the
//! paint pass (`paint`) that builds the render scene from the tree, fine-grained damage
//! tracking (`damage`), the hybrid frame scheduler (`scheduler`), and the events/input layer
//! (`event`) — so far the hit-test structure (#47).

pub mod a11y;
pub mod anim;
pub mod app;
pub mod arena;
pub mod bridge;
pub mod clipboard;
pub mod damage;
/// Dev-build debug overlay (#69). Compiled out in release builds.
#[cfg(debug_assertions)]
pub mod debug;
pub mod element;
pub mod error;
pub mod event;
pub mod ime;
pub mod overlay;
pub mod paint;
pub mod persist;
pub mod reactive;
pub mod scheduler;

pub use a11y::Role;
pub use anim::{Animatable, Animated, AnimationSpec, Easing};
pub use app::{App, Decorations, WindowOptions};
pub use bridge::UiProxy;
pub use clipboard::{Clipboard, SystemClipboard};
pub use damage::DamageState;
pub use element::{
    AnchorHandle, AnyElement, Element, Icon, IntoElement, Placement, ScrollHandle, TextEdit,
    TransformOrigin, Transformed, div, icon, overlay, scroll, text, text_edit, transform,
    virtualized_list,
};
pub use error::AppError;
pub use event::{
    Action, CursorIcon, CursorRegistry, CursorState, DispatchState, DragPayload, DragSource,
    DropTarget, FileDrop, FocusHandle, FocusId, FocusRegistry, GestureEvent, GestureRecognizer,
    HitRegion, HitTest, InteractFlags, KeyChord, KeyCode, KeyContext, KeyEvent, Keymap, Modifiers,
    MouseButton, MouseEvent, MouseKind, dispatch_action, dispatch_drag_leave, dispatch_drag_over,
    dispatch_drag_start, dispatch_drop, dispatch_gesture, dispatch_ime, dispatch_key,
    dispatch_mouse, use_focus_handle,
};
pub use ime::ImeCaretArea;
pub use kagari_base::{Transform, WindowId};
pub use kagari_render::IconId;
pub use overlay::{OverlayEntry, OverlayLayer, OverlayLayers, OverlayRegistry};
pub use persist::{
    KeymapOverrides, OverrideBinding, PersistedState, PersistenceService, WindowGeometry,
};
pub use scheduler::{ActiveSources, Scheduler, Tickable, Ticker};
