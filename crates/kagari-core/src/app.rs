//! App shell (#63): an [`App`] owns the shared GPU (adapter/device/queue), the app root reactive
//! owner, the App-level theme (single source), and the winit event loop; each window owns its
//! surface, root view, renderer, and per-window scene/scheduler/damage plus a per-window child
//! reactive owner. App→N windows (specs §1.13); MVP ships a single window.
//!
//! The shared device is created **lazily on the first window's `resumed`** (an adapter needs a
//! compatible surface; a wgpu device is surface-agnostic, so the first window's device serves all
//! windows) and `Arc`-shared into each window's renderer. The glyph/image atlases stay per-window
//! (sharing them is #191); FontDb sharing is #192. Design confirmed by a Tier-2 arch review.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use kagari_base::{NodeId, Point, Size, WindowId};
use kagari_layout::LayoutTree;
use kagari_text::{FontDb, ImeEvent, TextSystem};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalPosition, LogicalSize};
use winit::event::{ElementState, Ime, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowAttributes, WindowId as WinitWindowId};

use kagari_style::Theme;

use crate::a11y::A11yTree;
use crate::arena::Arena;
use crate::bridge::{UiProxy, UserEvent, run_ui_event};
use crate::damage::DamageState;
use crate::element::{AnyElement, IntoElement};
use crate::error::AppError;
use crate::event::{
    Action, CursorIcon, CursorRegistry, CursorState, DispatchState, DragPayload, FileDrop,
    FocusRegistry, GestureRecognizer, HitTest, KeyChord, KeyContext, KeyEvent, Keymap, Modifiers,
    MouseButton, MouseEvent, MouseKind, dispatch_action, dispatch_drag_leave, dispatch_drag_over,
    dispatch_drag_start, dispatch_drop, dispatch_gesture, dispatch_ime, dispatch_key,
    dispatch_mouse,
};
use crate::overlay::{OverlayLayers, OverlayRegistry};
use crate::paint::{paint_element_into, render_tree};
use crate::persist::{PersistenceService, WindowGeometry};
use crate::reactive::prelude::*;
use crate::reactive::{Owner, RwSignal, create_effect, provide_context};
use crate::scheduler::{Scheduler, should_redraw};

/// Extracts a human-readable message from a caught panic payload (#65). Panics carry `&str`
/// (`panic!("literal")`) or `String` (`panic!("{}", …)`); anything else falls back.
fn panic_message(payload: &(dyn Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("<non-string panic>")
}

/// Runs `f` under the frame/window panic boundary (#65, specs §1.11): catches an unwinding panic
/// (an invariant violation, as opposed to a recoverable `Result`) and returns its message on `Err`.
/// `AssertUnwindSafe` is deliberate — `f` borrows `&mut Window` state that may be left inconsistent
/// by a panic, so the caller must **degrade the window** (stop mutating) rather than continue. Pure
/// (no window state of its own) so the boundary is unit-testable without a live window/GPU.
fn run_guarded(f: impl FnOnce()) -> Result<(), String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .map_err(|payload| panic_message(payload.as_ref()).to_string())
}

/// Installs a process-global panic hook routing panics to `tracing::error!` with their source
/// location and payload (#65, specs §1.11) — the boundary's caught `Err` carries the payload but not
/// the location, so the hook supplies it. Chains the previous hook so a dev's stderr/backtrace is
/// preserved. Installed by [`App::run`] (which owns the process event loop).
fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".to_string());
        tracing::error!(location = %location, panic = %panic_message(info.payload()), "panic");
        prev(info);
    }));
}

/// The shared GPU: the wgpu adapter + device + queue, created once (lazily on the first window) and
/// `Arc`-cloned into every window's renderer (specs §1.13). Device-loss recovery (#64) regenerates
/// this one holder and fans out to each window's renderer.
struct Gpu {
    adapter: wgpu::Adapter,
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
}

/// How a window is decorated. MVP is native (OS-drawn) decorations; custom client-side decorations
/// (CSD) are post-MVP (§1.14) — `#[non_exhaustive]` so a `Custom` mode can be added without a break.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[non_exhaustive]
pub enum Decorations {
    /// OS-drawn native decorations (title bar, resize border).
    #[default]
    Native,
}

/// Options for a new window. `#[non_exhaustive]`: fields (min/max size, position, transparency, …) can
/// be added without a breaking change. Build from [`Default`] + the setters.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct WindowOptions {
    /// The window title.
    pub title: String,
    /// The initial inner (client-area) size in logical pixels.
    pub inner_size: Size,
    /// Native vs custom decorations (MVP: `Native`).
    pub decorations: Decorations,
}

impl Default for WindowOptions {
    fn default() -> Self {
        Self {
            title: "kagari".to_string(),
            inner_size: Size { w: 800.0, h: 600.0 },
            decorations: Decorations::Native,
        }
    }
}

impl WindowOptions {
    /// Sets the window title.
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    /// Sets the initial inner size (logical px).
    pub fn inner_size(mut self, size: Size) -> Self {
        self.inner_size = size;
        self
    }
}

/// A window queued by [`App::open_window`] before the loop runs; created in `resumed` (winit 0.30
/// requires window creation there). The root closure is `FnOnce`: it builds the element tree once,
/// under the window's child owner + the App context (the retained model rebuilds via signals, §1.3).
struct PendingWindow {
    id: WindowId,
    opts: WindowOptions,
    root: Box<dyn FnOnce() -> AnyElement>,
}

/// The application shell. Owns the shared GPU, the app root reactive owner, the App-level theme
/// (single source), and the winit event loop; windows are created in `resumed`.
pub struct App {
    instance: wgpu::Instance,
    /// The shared GPU, created lazily on the first window's `resumed` (then reused for all windows).
    gpu: Option<Gpu>,
    /// Live windows, keyed by winit's per-event `WindowId` (the identity events arrive under).
    windows: HashMap<WinitWindowId, WindowState>,
    /// Windows queued by `open_window`, drained in `resumed`.
    pending: Vec<PendingWindow>,
    /// Next public window id.
    next_id: u32,
    /// Monotonic count of windows created, used as the open-order index into
    /// [`PersistedState::windows`](crate::persist::PersistedState) so restored geometry maps back to
    /// the same slot (#68). Monotonic (not `windows.len()`) so a closed window doesn't shift indices.
    windows_created: usize,
    /// The persistence service (#68, §7.3): loaded at construction, provided as a context service, and
    /// flushed on exit. Window geometry is captured into it; theme + keymap overrides are applied from it.
    persist: PersistenceService,
    /// The app root reactive owner: each window's owner is a child of this, so App-provided context
    /// (theme/services, §1.8) is inherited by every window. Held for the app's lifetime (RK-008).
    root_owner: Owner,
    /// The background→UI bridge's deferred proxy cell (#66): [`ui_proxy`](Self::ui_proxy) hands out
    /// [`UiProxy`]s wrapping this before the loop exists; [`run`](Self::run) populates it with the
    /// event loop's `EventLoopProxy` so posted closures reach the UI thread's `user_event`.
    proxy_cell: Arc<OnceLock<EventLoopProxy<UserEvent>>>,
    /// Set by the wgpu device-lost callback (#64) when the shared device is lost (TDR / GPU reset /
    /// sleep-wake). [`about_to_wait`](Self::about_to_wait) polls + clears it and runs
    /// [`recover_device`](Self::recover_device). Shared with the callback (which runs off the UI thread).
    device_lost: Arc<AtomicBool>,
    /// The App-level theme (single source, §1.8). Each window holds a `Copy` of this signal; a swap
    /// (hot-reload) re-runs every window's theme→damage effect, so all windows reskin.
    theme: RwSignal<Arc<Theme>>,
    /// The theme RON file to watch for dev hot-reload (#44), set via [`App::watch_theme`].
    #[cfg(feature = "hot-reload")]
    theme_path: Option<std::path::PathBuf>,
    /// The live theme watcher, kept alive for the app's lifetime once spawned.
    #[cfg(feature = "hot-reload")]
    theme_watcher: Option<kagari_style::ThemeWatcher>,
}

/// Pointer travel (logical px) a press must exceed before a press-on-a-drag-source becomes a drag
/// (rather than a click). Compared squared to avoid a sqrt (#178).
const DRAG_THRESHOLD_PX: f32 = 4.0;

/// The action name for the dev-only debug-overlay toggle (F12, #69).
#[cfg(debug_assertions)]
const DEBUG_OVERLAY_ACTION: &str = "kagari.debug.toggle-overlay";

/// How long after the last persisted-state change to wait before writing to disk (#68). Coalesces a
/// burst of `Moved`/`Resized` events (e.g. a window drag) into one write once the burst settles.
const PERSIST_DEBOUNCE: Duration = Duration::from_millis(500);

/// The per-frame animation `dt` (#253) is clamped to this so a long idle gap, a paused window, or a
/// stalled frame cannot advance a spring by a huge step (a visible jump / instability). The spring
/// integrator also sub-steps internally.
const MAX_FRAME_DT: Duration = Duration::from_millis(64);

/// Resolves the persisted theme id to a [`Theme`] at startup (#68). Only built-in `light`/`dark` are
/// applied; an unknown id (a custom theme) is kept persisted but falls back to the default until a
/// theme registry exists (post-MVP).
fn initial_theme(id: Option<&str>) -> Theme {
    match id {
        Some("dark") => Theme::dark(),
        Some("light") | None => Theme::light(),
        Some(other) => {
            tracing::warn!(
                theme = other,
                "unknown persisted theme id; using the default light theme (a theme registry is post-MVP)"
            );
            Theme::light()
        }
    }
}

/// Whether the pointer has travelled far enough from an armed press `start` to begin a drag (#178).
/// Pure (squared distance vs [`DRAG_THRESHOLD_PX`]²) so the arm→active decision is unit-testable.
fn past_drag_threshold(start: Point, current: Point) -> bool {
    let dx = current.x - start.x;
    let dy = current.y - start.y;
    dx * dx + dy * dy > DRAG_THRESHOLD_PX * DRAG_THRESHOLD_PX
}

/// The drag hover transition when the topmost drop target under the pointer changes from `over`
/// (which was `accepted`) to `raw` (#178): returns `(leave, query)` — the node to send `drag-leave`
/// to (only a *previously-accepting* target, so a non-accepting target that never got `drag-over`
/// isn't spuriously left) and the node to query for acceptance (`raw`, or `None` off all targets).
/// Pure so the leave-only-if-accepted rule is unit-testable independent of the live window; the
/// caller invokes it only when `raw != over`.
fn hover_transition(
    raw: Option<NodeId>,
    over: Option<NodeId>,
    accepted: bool,
) -> (Option<NodeId>, Option<NodeId>) {
    let leave = if accepted { over } else { None };
    (leave, raw)
}

/// An in-flight drag (#178): the produced payload, its concrete [`TypeId`] (for hover-accept checks),
/// the drop target currently under the cursor (`over`), and whether that target accepts the payload.
struct ActiveDrag {
    payload: Box<dyn DragPayload>,
    payload_type: TypeId,
    over: Option<NodeId>,
    accepted: bool,
}

/// The window's drag state machine (#178): idle → armed (pressed on a source) → active (past the
/// threshold) → drop/cancel.
enum DragState {
    Idle,
    /// A press landed on a drag source; a drag begins if the pointer travels past the threshold.
    Armed {
        source: NodeId,
        start: Point,
    },
    /// A drag is live: hit-testing drop targets, tracking hover, following the cursor with the image.
    Active(ActiveDrag),
}

struct WindowState {
    // `Arc<Window>` lets wgpu hold a `Surface<'static>` via the safe `create_surface` path — no
    // hand-written raw-window-handle lifetime and no `unsafe` (RK-002).
    window: Arc<Window>,
    // Field order: `surface` before `device` so a dropped window releases its surface before its
    // shared-device Arc clone (device-loss / close drop order, #64).
    surface: wgpu::Surface<'static>,
    // A clone of the shared device, kept for surface reconfigure on resize.
    device: Arc<wgpu::Device>,
    config: wgpu::SurfaceConfiguration,
    renderer: kagari_render::Renderer,
    scene: kagari_render::Scene,
    arena: Arena,
    layout: LayoutTree,
    text: TextSystem,
    root: AnyElement,
    damage: Arc<DamageState>,
    // The per-window child owner (of the App root, RK-006/008): built-once tree effects (theme,
    // reactive props) bind here and dispose when the window closes. Held to keep them alive.
    _owner: Owner,
    // A `Copy` handle to the App-level theme signal (single source): read each frame to resolve
    // tokens. Writing happens App-side (hot-reload); the per-window theme→damage effect reskins.
    theme: RwSignal<Arc<Theme>>,
    scheduler: Scheduler,
    // The per-window animation ticker (#253): drivers (`Animated<T>`) register themselves when they
    // start; `redraw` advances them each frame. Provided into the window's reactive context so
    // `Animated::new` captures it. `last_frame` gives the tick its `dt`.
    ticker: crate::scheduler::Ticker,
    last_frame: Option<Instant>,
    ime_enabled: bool,
    // The IME candidate-window transport (#299): the focused editable leaf publishes its caret rect
    // (window coords) here at paint; `sync_ime` reads it after the frame to point `set_ime_cursor_area`
    // and toggle `set_ime_allowed`. A per-frame-record sink (RK-019): cleared before each paint.
    ime_caret: crate::ime::ImeCaretArea,
    // Whether `set_ime_allowed(true)` is currently requested (#299). Tracked so the toggle fires only on a
    // text-field focus enter/leave, not every frame.
    ime_allowed_requested: bool,
    // Set once a panic is caught in this window's frame/event work (#65): a degraded window skips all
    // further frames + events (its last-good frame stays on screen; it can still be closed), rather
    // than continuing to mutate state left inconsistent by the panic.
    degraded: bool,
    // Per-window interactive state, persisted across frames (live winit input wiring, #177).
    // `hit_test` is re-recorded every paint; `focus`/`cursor_reg` are populated on the build-once
    // pass (they are not cleared per frame). `dispatch` keeps pointer capture across a drag.
    hit_test: HitTest,
    focus: FocusRegistry,
    cursor_reg: CursorRegistry,
    // Interactive overlay substrate (#219). `overlay_reg` (z-slots so stacked/nested overlays get
    // distinct painter-order bands) is re-recorded each paint by `render_tree`. `overlay_layers` (the
    // interactive layer stack the shell reads for outside/Esc dismiss routing + focus-trap) is
    // context-provided to open overlays and cleared by the shell before each paint.
    overlay_reg: OverlayRegistry,
    overlay_layers: OverlayLayers,
    cursor_state: CursorState,
    dispatch: DispatchState,
    gestures: GestureRecognizer,
    keymap: Keymap,
    // The last known pointer position (logical px, window coords), tracked from `CursorMoved` and
    // used for `MouseInput`/`MouseWheel` (which carry no position) and gesture targeting.
    pointer: Point,
    // The modifiers held, tracked from `ModifiersChanged` (winit key/mouse events don't carry them).
    modifiers: Modifiers,
    // Live drag-and-drop (#178). `drag` is the state machine; `drag_image` is the shell-owned
    // cursor-tracked image (built once at drag start from the source's `drag_image()`), painted above
    // all content each frame at the pointer into its own `drag_image_arena`/`drag_image_layout`.
    drag: DragState,
    drag_image: Option<AnyElement>,
    drag_image_arena: Arena,
    drag_image_layout: LayoutTree,
    // A per-drag child owner (of the window owner) scoping the drag-image's build-once reactive-prop
    // effects, so a reactive drag-image's effects dispose when the drag ends rather than accumulating
    // on the long-lived window owner (RK-005/006/020). `None` when no drag is active.
    drag_owner: Option<Owner>,
    // Accessibility (#67). `a11y` is the per-frame sink annotated elements record into during paint
    // (absolute bounds, cleared each frame like `hit_test`); `adapter` bridges the derived accesskit
    // tree to the OS. `redraw` pushes a full `TreeUpdate` via `adapter.update_if_active` (no-op unless
    // an assistive tech is active), and `window_event` feeds winit events to `adapter.process_event`.
    a11y: A11yTree,
    adapter: accesskit_winit::Adapter,
    // The open-order index into the persisted `windows` (#68): geometry captured on Moved/Resized is
    // written back to this slot, and the window's initial geometry was restored from it at creation.
    persist_index: usize,
    // Dev-build debug overlay (#69): `debug_overlay` is toggled by F12 (see `handle_action`); when on,
    // `redraw` injects the diagnostic HUD after the frame. `frame_clock` tracks recent frame times for
    // FPS/frame-time. Both are `cfg(debug_assertions)` — absent (zero cost) in release.
    #[cfg(debug_assertions)]
    debug_overlay: bool,
    #[cfg(debug_assertions)]
    frame_clock: crate::debug::FrameClock,
}

/// Binds a window's theme→damage effect: reading the App-level theme subscribes, so a swap re-runs
/// this and flags **this window's** full repaint (synchronous `ImmediateEffect`, ADR 0001). Called
/// under the window's child owner so it disposes on close (RK-006). Must run with an owner current.
fn bind_window_theme_damage(theme: RwSignal<Arc<Theme>>, damage: Arc<DamageState>) {
    create_effect(move || {
        // Subscribe to the theme; the frame loop reads the resolved value, so the only job here is
        // to turn a swap into this window's damage.
        let _ = theme.get();
        damage.mark_all_dirty();
    });
}

impl App {
    /// Creates the app shell: the wgpu instance and the app root reactive owner + App-level theme
    /// context. **No GPU device is created here** — it is created lazily on the first window's
    /// `resumed` (headless-safe: `App::new` needs no adapter). Loads the persisted state (#68) from the
    /// OS config dir; use [`with_persistence`](Self::with_persistence) to supply a custom service
    /// (a custom location, or an in-memory one for tests).
    pub fn new() -> Result<Self, AppError> {
        Self::with_persistence(PersistenceService::load())
    }

    /// Like [`new`](Self::new) but with a caller-supplied [`PersistenceService`] — for a custom config
    /// location or an [`in_memory`](PersistenceService::in_memory) service (tests / persistence off).
    pub fn with_persistence(persist: PersistenceService) -> Result<Self, AppError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            flags: wgpu::InstanceFlags::default(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            backend_options: wgpu::BackendOptions::default(),
            display: None,
        });
        // The app root owner scopes App-level context (theme/services) so every window (a child
        // owner) inherits it. The theme is the App-level single source (§1.8); the persistence service
        // is a context service (§1.8) so app code (recent files, etc.) shares it.
        let root_owner = Owner::new();
        let theme = root_owner.with(|| {
            let theme = RwSignal::new(Arc::new(initial_theme(persist.snapshot().theme.as_deref())));
            provide_context(theme.read_only());
            provide_context(persist.clone());
            theme
        });
        Ok(Self {
            instance,
            gpu: None,
            windows: HashMap::new(),
            pending: Vec::new(),
            next_id: 0,
            windows_created: 0,
            persist,
            root_owner,
            theme,
            proxy_cell: Arc::new(OnceLock::new()),
            device_lost: Arc::new(AtomicBool::new(false)),
            #[cfg(feature = "hot-reload")]
            theme_path: None,
            #[cfg(feature = "hot-reload")]
            theme_watcher: None,
        })
    }

    /// Returns a cloneable, `Send` [`UiProxy`] for the background→UI bridge (#66): move a clone into a
    /// background task (on the consumer's runtime) and call [`UiProxy::spawn`] to run a closure on the
    /// UI thread (to update signals, one-way). May be called before [`run`](Self::run); closures posted
    /// before the loop starts are dropped.
    pub fn ui_proxy(&self) -> UiProxy {
        UiProxy::new(Arc::clone(&self.proxy_cell))
    }

    /// Recovers from a lost GPU device (#64, specs §1.11/§2.9): regenerate the shared device once from
    /// the surviving adapter, then fan `Renderer::recreate` + surface reconfigure out to every window
    /// and flag a full repaint. Called by [`about_to_wait`](Self::about_to_wait) when the device-lost
    /// flag is set. If the device cannot be re-created, log and degrade all windows (survive, #65).
    fn recover_device(&mut self) {
        // Nothing to recover if no device was ever created.
        let Some(old) = self.gpu.take() else {
            return;
        };
        // The adapter survives a device loss; request a fresh device + queue from it.
        let (device, queue) = match pollster::block_on(create_device(&old.adapter)) {
            Ok(dq) => dq,
            Err(e) => {
                tracing::error!(error = %e, "device re-creation failed after loss; degrading all windows");
                for state in self.windows.values_mut() {
                    state.degraded = true;
                }
                return; // `self.gpu` stays None; the app survives with frozen windows.
            }
        };
        register_device_lost(&device, &self.device_lost, &self.proxy_cell);
        // Fan out: rebuild each window's GPU resources against the new device, reconfigure its surface
        // (new device before the old device's Arc clones drop), and flag a full repaint.
        for state in self.windows.values_mut() {
            state
                .renderer
                .recreate(Arc::clone(&device), Arc::clone(&queue));
            state.device = Arc::clone(&device);
            state.surface.configure(&device, &state.config);
            state.damage.mark_all_dirty();
            state.window.request_redraw();
        }
        self.gpu = Some(Gpu {
            adapter: old.adapter,
            device,
            queue,
        });
        tracing::info!("device recovered; all windows rebuilt");
    }

    /// Handles an accessibility event forwarded by a window's `accesskit_winit` adapter (#67). MVP is
    /// read-only: serve the derived tree + focus on the initial request; action routing (click/focus/
    /// set-value) into the event system is post-MVP (§1.12).
    fn on_accessibility_event(&mut self, event: accesskit_winit::Event) {
        use accesskit_winit::WindowEvent as AkWindowEvent;
        let Some(state) = self.windows.get_mut(&event.window_id) else {
            return;
        };
        match event.window_event {
            AkWindowEvent::InitialTreeRequested => {
                // Serve the current tree (the last painted frame's nodes; root-only if nothing has
                // painted yet), then request a redraw so a fresh frame repopulates the sink.
                let scale = state.window.scale_factor();
                state.adapter.update_if_active(|| {
                    state
                        .a11y
                        .build_update(&state.arena, state.focus.focused_node(), scale)
                });
                state.window.request_redraw();
            }
            AkWindowEvent::ActionRequested(request) => {
                tracing::debug!(
                    action = ?request.action,
                    "accesskit action requested (ignored; routing is post-MVP)"
                );
            }
            AkWindowEvent::AccessibilityDeactivated => {
                tracing::debug!("accessibility deactivated");
            }
        }
    }

    /// Captures window `id`'s current outer position + inner size (logical px) into the persisted
    /// state (#68) and arms the debounce, so [`about_to_wait`](Self::about_to_wait) writes it once the
    /// change settles. Reads the window's authoritative geometry (rather than folding the event), so a
    /// `Moved` and a `Resized` both produce a complete record.
    fn capture_window_geometry(&mut self, id: WinitWindowId, event_loop: &ActiveEventLoop) {
        let Some((index, geometry)) = self.windows.get(&id).map(|state| {
            let scale = state.window.scale_factor() as f32;
            let position = state
                .window
                .outer_position()
                .map(|p| Point::new(p.x as f32 / scale, p.y as f32 / scale))
                .unwrap_or_else(|_| Point::new(0.0, 0.0));
            let inner = state.window.inner_size();
            let size = Size {
                w: inner.width as f32 / scale,
                h: inner.height as f32 / scale,
            };
            (state.persist_index, WindowGeometry { position, size })
        }) else {
            return;
        };
        self.persist.with(|s| {
            // Grow the Vec so this window's open-order slot exists (gaps filled with a zero geometry,
            // overwritten when those windows report their own geometry).
            if s.windows.len() <= index {
                s.windows.resize_with(index + 1, || WindowGeometry {
                    position: Point::new(0.0, 0.0),
                    size: Size { w: 0.0, h: 0.0 },
                });
            }
            s.windows[index] = geometry;
        });
        // Arm the debounce: wake the loop at the deadline so the write happens even when idle.
        event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + PERSIST_DEBOUNCE));
    }

    /// Provides an App-level context value (a service, §1.8), inherited by every window's root view.
    /// Call before `run`; values are visible to windows opened afterward (each window's owner is a
    /// child of the app root owner).
    pub fn provide<T: Send + Sync + 'static>(&mut self, value: T) {
        self.root_owner.with(|| provide_context(value));
    }

    /// Queues a window to open with `opts`, its root view built by `root` (once, when the window is
    /// created in `resumed`). Returns the public [`WindowId`]. MVP: call before [`run`](Self::run)
    /// (dynamic open after the loop starts is post-MVP).
    pub fn open_window<R: IntoElement>(
        &mut self,
        opts: WindowOptions,
        root: impl FnOnce() -> R + 'static,
    ) -> Result<WindowId, AppError> {
        let id = WindowId::from_raw(self.next_id);
        self.next_id += 1;
        self.pending.push(PendingWindow {
            id,
            opts,
            root: Box::new(move || root().into_element()),
        });
        Ok(id)
    }

    /// Watches a theme RON file for dev hot-reload (#44). Requires the `hot-reload` feature.
    #[cfg(feature = "hot-reload")]
    pub fn watch_theme(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.theme_path = Some(path.into());
        self
    }

    /// Runs the winit event loop until the last window closes. Returns a startup/loop error (rather
    /// than diverging) so `main` can surface a nonzero exit.
    pub fn run(mut self) -> Result<(), AppError> {
        // Route panics to tracing (with location) before the loop runs, so the frame/window panic
        // boundary's caught panics are logged with their source location (#65, specs §1.11).
        install_panic_hook();
        // Apply the initial theme (the watched file if set) to the App-level single source before the
        // loop starts (no window/effect exists yet, so this just sets the value).
        #[cfg(feature = "hot-reload")]
        {
            let initial = self.initial_theme();
            self.theme.set(Arc::new(initial));
        }
        let event_loop = EventLoop::<UserEvent>::with_user_event()
            .build()
            .map_err(|e| AppError::WindowCreate(e.to_string()))?;
        event_loop.set_control_flow(ControlFlow::Wait);
        // Populate the bridge's proxy cell so any `UiProxy` handed out before `run` (via `ui_proxy`)
        // goes live and can post closures to `user_event` (#66).
        let _ = self.proxy_cell.set(event_loop.create_proxy());
        #[cfg(feature = "hot-reload")]
        self.spawn_theme_watcher(&event_loop);
        event_loop
            .run_app(&mut self)
            .map_err(|e| AppError::WindowCreate(e.to_string()))
    }

    /// Spawns the theme-file watcher (if [`watch_theme`](Self::watch_theme) set a path), wired to post
    /// each reload to the UI thread via the winit `EventLoopProxy`. A setup error degrades to a warning.
    #[cfg(feature = "hot-reload")]
    fn spawn_theme_watcher(&mut self, event_loop: &EventLoop<UserEvent>) {
        let Some(path) = self.theme_path.clone() else {
            return;
        };
        let proxy = event_loop.create_proxy();
        // The watcher runs on a background thread; unify it onto the #66 bridge by posting a UI
        // closure that swaps the App-level theme signal (Copy + Send) on the UI thread (#44/#66).
        let theme = self.theme;
        match kagari_style::ThemeWatcher::new(path, move |reloaded| {
            let _ = proxy.send_event(UserEvent::Ui(Box::new(move || {
                theme.set(Arc::new(reloaded))
            })));
        }) {
            Ok(watcher) => self.theme_watcher = Some(watcher),
            Err(e) => tracing::warn!(error = %e, "failed to start theme hot-reload watcher"),
        }
    }

    /// The theme to start with: the watched file loaded once (if set), else the built-in light theme.
    #[cfg(feature = "hot-reload")]
    fn initial_theme(&self) -> Theme {
        if let Some(path) = &self.theme_path {
            match kagari_style::load_theme_file(path) {
                Ok(theme) => return theme,
                Err(e) => {
                    tracing::warn!(error = %e, "failed to load initial theme file; using light theme")
                }
            }
        }
        Theme::light()
    }

    /// Creates one window (winit window + surface + renderer + root view), creating the shared GPU
    /// lazily on the first window. Returns the winit id, the public id, and the window state.
    fn create_window(
        &mut self,
        event_loop: &ActiveEventLoop,
        pending: PendingWindow,
    ) -> Result<(WinitWindowId, WindowId, WindowState), AppError> {
        // Restore this window's persisted geometry (#68), keyed by open-order index. A persisted size
        // overrides the requested `inner_size`; a persisted position is applied too. Absent → defaults.
        let persist_index = self.windows_created;
        let geometry = self.persist.snapshot().windows.get(persist_index).cloned();
        let inner_size = geometry
            .as_ref()
            .map(|g| g.size)
            .unwrap_or(pending.opts.inner_size);

        // Create the window initially hidden: the accesskit adapter must be built before the window
        // is first shown (`accesskit_winit` panics on an already-visible window, #67/RK-022). It is
        // made visible at the end of this function, once the adapter exists.
        let mut attrs = WindowAttributes::default()
            .with_title(pending.opts.title.clone())
            .with_visible(false)
            .with_inner_size(LogicalSize::new(inner_size.w, inner_size.h));
        if let Some(g) = &geometry {
            attrs = attrs.with_position(LogicalPosition::new(g.position.x, g.position.y));
        }
        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .map_err(|e| AppError::WindowCreate(e.to_string()))?,
        );
        let winit_id = window.id();
        let surface = self
            .instance
            .create_surface(window.clone())
            .map_err(|e| AppError::DeviceInit(e.to_string()))?;

        // Lazily create the shared GPU on the first window; reuse it for all subsequent windows.
        if self.gpu.is_none() {
            let gpu = pollster::block_on(create_gpu(&self.instance, &surface))?;
            // Detect a genuine device loss on this device → recover in `about_to_wait` (#64).
            register_device_lost(&gpu.device, &self.device_lost, &self.proxy_cell);
            self.gpu = Some(gpu);
        }
        let (device, queue) = {
            let gpu = self.gpu.as_ref().expect("gpu created above");
            (Arc::clone(&gpu.device), Arc::clone(&gpu.queue))
        };
        let config = {
            let gpu = self.gpu.as_ref().expect("gpu created above");
            surface_config(&surface, &gpu.adapter, window.inner_size())?
        };
        surface.configure(&device, &config);
        // IME starts disallowed; `sync_ime` enables it on a text-field focus enter (#299), driven by the
        // focused editor's published caret area, and disables it on leave/unmount.

        let renderer = kagari_render::Renderer::new(
            Arc::clone(&device),
            queue,
            (config.width, config.height),
            config.format,
        );

        // Build the root view under a per-window child owner (of the app root) so App context
        // (theme/services) is inherited and the window's effects dispose on close (RK-006/008).
        let child_owner = self.root_owner.child();
        let theme = self.theme;
        let damage = Arc::new(DamageState::default());
        let scheduler = Scheduler::new();
        let ticker = crate::scheduler::Ticker::new();
        let ime_caret = crate::ime::ImeCaretArea::new();
        let (root, focus, overlay_layers) = child_owner.with(|| {
            bind_window_theme_damage(theme, Arc::clone(&damage));
            // `FocusRegistry::new` creates the current-focus `RwSignal`, so build it under the
            // window's child owner (RK-005/008); it disposes when the window closes.
            let focus = FocusRegistry::new();
            // Provide a clone into the window's context (#218) before building the root view, so a
            // widget's `use_focus_handle()` mints into this same registry (shared inner) and joins its
            // tab order / `focused_node` resolution. Disposed with the child owner on close (RK-006/008).
            provide_context(focus.clone());
            // OS clipboard (#298): provide the system backend so an editable `text_edit` leaf can
            // `use_context` it for Ctrl+C/V/X. A unit backend (opened per-op); no per-window state.
            provide_context::<std::sync::Arc<dyn crate::clipboard::Clipboard>>(
                std::sync::Arc::new(crate::clipboard::SystemClipboard),
            );
            // Interactive overlay layer stack (#219): provide a clone into context so an open `Overlay`
            // registers here at paint; the shell reads it for dismiss routing + focus-trap. Disposed with
            // the child owner on close.
            let overlay_layers = OverlayLayers::new();
            provide_context(overlay_layers.clone());
            // The scheduler active-source handle (#36/#253): provide it so `Animated::new` captures the
            // *same* counter `about_to_wait` reads via `has_active_sources` — a started animation keeps the
            // window on continuous per-vsync redraw until it settles. Without this the ticker only advances
            // on incidental repaints (the knob would crawl one frame per unrelated click).
            provide_context(scheduler.active_sources());
            // The animation ticker (#253): provide a clone so `Animated::new` (widget / scroll build)
            // captures it and registers itself when it starts; `redraw` drives it each frame.
            provide_context(ticker.clone());
            // The IME caret transport (#299): provide a clone so an editable `text_edit` leaf captures it
            // and publishes its caret area at paint; `redraw`/`sync_ime` read it to drive the OS IME.
            provide_context(ime_caret.clone());
            let root = (pending.root)();
            (root, focus, overlay_layers)
        });

        // The accessibility adapter (#67) forwards accesskit events as winit user events (an initial
        // tree request / action request / deactivation) via the same `EventLoopProxy` as the #66
        // bridge. The proxy is populated in `run()` before the loop drives `resumed` → here, so it is
        // present; fail the window rather than panic if that invariant is somehow broken.
        let adapter = match self.proxy_cell.get() {
            Some(proxy) => {
                accesskit_winit::Adapter::with_event_loop_proxy(event_loop, &window, proxy.clone())
            }
            None => {
                return Err(AppError::WindowCreate(
                    "event loop proxy not initialized before window creation".into(),
                ));
            }
        };
        // The adapter now exists, so it is safe to show the window (#67/RK-022: the adapter must be
        // built while the window is still hidden).
        window.set_visible(true);

        // The window's keymap: the framework defaults with the user's persisted overrides layered on (#68).
        let mut keymap = Keymap::default();
        self.persist
            .snapshot()
            .keymap_overrides
            .merge_into(&mut keymap);
        // Dev-only F12 → toggle the debug overlay (#69).
        #[cfg(debug_assertions)]
        keymap.bind(
            None::<&str>,
            KeyChord::new(KeyCode::F12, Modifiers::default()),
            Action::Named(DEBUG_OVERLAY_ACTION.into()),
        );

        // One more window successfully created — advance the open-order index for the next one (#68).
        self.windows_created += 1;

        Ok((
            winit_id,
            pending.id,
            WindowState {
                window,
                surface,
                device,
                config,
                renderer,
                scene: kagari_render::Scene::new(),
                arena: Arena::new(),
                layout: LayoutTree::new(),
                text: TextSystem::new(FontDb::new()),
                root,
                damage,
                _owner: child_owner,
                theme,
                scheduler,
                ticker,
                last_frame: None,
                ime_enabled: false,
                ime_caret,
                ime_allowed_requested: false,
                hit_test: HitTest::new(),
                focus,
                cursor_reg: CursorRegistry::new(),
                overlay_reg: OverlayRegistry::new(),
                overlay_layers,
                cursor_state: CursorState::new(),
                dispatch: DispatchState::default(),
                gestures: GestureRecognizer::new(),
                keymap,
                persist_index,
                pointer: Point::new(0.0, 0.0),
                modifiers: Modifiers::default(),
                drag: DragState::Idle,
                drag_image: None,
                drag_image_arena: Arena::new(),
                drag_image_layout: LayoutTree::new(),
                drag_owner: None,
                degraded: false,
                a11y: A11yTree::default(),
                adapter,
                #[cfg(debug_assertions)]
                debug_overlay: false,
                #[cfg(debug_assertions)]
                frame_clock: crate::debug::FrameClock::new(),
            },
        ))
    }
}

/// Map a winit IME event to kagari-text's abstract `ImeEvent`. `Enabled`/`Disabled` are state
/// transitions (handled by the caller), so they produce no `ImeEvent`. Keeps winit types out of
/// kagari-text (design.md).
fn map_ime_event(ime: Ime) -> Option<ImeEvent> {
    match ime {
        Ime::Preedit(text, cursor) => Some(ImeEvent::Preedit { text, cursor }),
        Ime::Commit(text) => Some(ImeEvent::Commit(text)),
        Ime::Enabled | Ime::Disabled => None,
    }
}

/// Whether `key` is an IME-owned toggle/conversion key. Such keys must never be consumed as an app
/// shortcut — the OS IME owns them — so the (future) keymap dispatches only when this is `false`.
/// Classified on the physical key (independent of IME state); see specs §5.2 (Zed #40321/#40592/…).
fn ime_owns_key(key: PhysicalKey) -> bool {
    use KeyCode::{
        Convert, Hiragana, KanaMode, Katakana, Lang1, Lang2, Lang3, Lang4, Lang5, NonConvert,
    };
    matches!(
        key,
        PhysicalKey::Code(
            Convert
                | NonConvert
                | KanaMode
                | Hiragana
                | Katakana
                | Lang1
                | Lang2
                | Lang3
                | Lang4
                | Lang5
        )
    )
}

/// Logical-pixel scroll distance per wheel "line" (winit `LineDelta`), roughly a text line height.
const WHEEL_LINE_PX: f32 = 16.0;

/// Maps winit's modifier state to kagari's [`Modifiers`] (tracked from `ModifiersChanged`, since
/// winit key/mouse events don't carry modifiers).
fn map_modifiers(state: winit::keyboard::ModifiersState) -> Modifiers {
    Modifiers {
        ctrl: state.control_key(),
        shift: state.shift_key(),
        alt: state.alt_key(),
        meta: state.super_key(),
    }
}

/// Maps a winit mouse button to kagari's [`MouseButton`]. Total: extra buttons (`Back`/`Forward`/
/// `Other`) fold into [`MouseButton::Other`] with a platform index.
fn map_mouse_button(button: winit::event::MouseButton) -> MouseButton {
    use winit::event::MouseButton as W;
    match button {
        W::Left => MouseButton::Left,
        W::Right => MouseButton::Right,
        W::Middle => MouseButton::Middle,
        W::Back => MouseButton::Other(3),
        W::Forward => MouseButton::Other(4),
        W::Other(n) => MouseButton::Other(n),
    }
}

/// Maps kagari's [`CursorIcon`] to winit's, at the window boundary so the winit type never leaks
/// through the public builder (design.md §1). Exhaustive over kagari's variants (adding one is a
/// compile error here — intended).
fn map_cursor_icon(icon: CursorIcon) -> winit::window::CursorIcon {
    use winit::window::CursorIcon as W;
    match icon {
        CursorIcon::Default => W::Default,
        CursorIcon::Pointer => W::Pointer,
        CursorIcon::Text => W::Text,
        CursorIcon::Crosshair => W::Crosshair,
        CursorIcon::Grab => W::Grab,
        CursorIcon::Grabbing => W::Grabbing,
        CursorIcon::ColResize => W::ColResize,
        CursorIcon::RowResize => W::RowResize,
        CursorIcon::EwResize => W::EwResize,
        CursorIcon::NsResize => W::NsResize,
    }
}

/// Converts a winit scroll delta to a logical-pixel `(dx, dy)`. Line deltas (mouse wheels) scale by
/// [`WHEEL_LINE_PX`]; pixel deltas (precision touchpads) are physical, so divide by `scale`.
fn scroll_delta(delta: MouseScrollDelta, scale: f32) -> (f32, f32) {
    match delta {
        MouseScrollDelta::LineDelta(x, y) => (x * WHEEL_LINE_PX, y * WHEEL_LINE_PX),
        MouseScrollDelta::PixelDelta(p) => (p.x as f32 / scale, p.y as f32 / scale),
    }
}

/// The [`KeyChord`] for a resolved key event: its physical code + the modifiers held.
fn key_chord(ev: &KeyEvent) -> KeyChord {
    KeyChord::new(ev.code, ev.modifiers)
}

/// The routing decision for a key event (#177 Q2, keymap-first-consume).
#[derive(Clone, PartialEq, Eq, Debug)]
enum KeyRoute {
    /// A chord bound in the keymap (on press): consumed as this `Action`, not delivered as a key.
    Consume(Action),
    /// Deliver the raw key to the focused node (unbound key, or any release).
    Deliver,
}

/// Decides how a key event routes: a keymap-bound chord **on press** is consumed as an `Action`;
/// unbound keys and all releases are delivered to the focused node. Pure (no dispatch) so the
/// keymap-first-consume policy is unit-testable independent of the live window.
fn route_key(keymap: &Keymap, ev: &KeyEvent) -> KeyRoute {
    if ev.pressed {
        if let Some(action) = keymap.resolve(key_chord(ev), &KeyContext::default()) {
            return KeyRoute::Consume(action);
        }
    }
    KeyRoute::Deliver
}

impl WindowState {
    /// Handle a winit IME event: track enable state, report the caret area on enable, and route
    /// preedit/commit (as `ImeEvent`) to the focused editable element (#296) when enabled.
    fn on_ime(&mut self, ime: Ime) {
        match &ime {
            Ime::Enabled => {
                self.ime_enabled = true;
                self.report_ime_caret_area();
            }
            Ime::Disabled => self.ime_enabled = false,
            _ => {}
        }
        if let Some(ev) = map_ime_event(ime) {
            if self.ime_enabled {
                // Log only the event shape, never the composed text (IME content is user input, may
                // include passwords).
                let (kind, text_len) = match &ev {
                    ImeEvent::Preedit { text, .. } => ("preedit", text.len()),
                    ImeEvent::Commit(text) => ("commit", text.len()),
                    _ => ("ime", 0),
                };
                tracing::debug!(kind, text_len, "ime event");
                // Route to the focused editable element, bubbling its ancestor chain (#296).
                dispatch_ime(&mut self.root, &self.arena, self.focus.focused_node(), &ev);
            }
        }
    }

    /// Drive the OS IME from the focused editor's published caret area (#299): a published rect means a
    /// text field is focused, so enable IME (once, on the enter edge) and point the candidate window at the
    /// caret; its absence means none is focused, so disable IME (once, on the leave edge). Called after each
    /// frame's paint, so a blurred or unmounted editor tears IME down on its own.
    fn sync_ime(&mut self) {
        if self.ime_caret.get().is_some() {
            if !self.ime_allowed_requested {
                self.window.set_ime_allowed(true);
                self.ime_allowed_requested = true;
            }
            self.report_ime_caret_area();
        } else if self.ime_allowed_requested {
            self.window.set_ime_allowed(false);
            self.ime_allowed_requested = false;
        }
    }

    /// Point the OS IME candidate window at the focused editor's caret area (#299). The editor publishes the
    /// caret rect in window-logical coords, which `set_ime_cursor_area` expects directly (no scale factor).
    fn report_ime_caret_area(&self) {
        use winit::dpi::LogicalPosition;
        if let Some(rect) = self.ime_caret.get() {
            self.window.set_ime_cursor_area(
                LogicalPosition::new(rect.origin.x, rect.origin.y),
                LogicalSize::new(rect.size.w.max(1.0), rect.size.h),
            );
        }
    }

    /// Handle a pointer move: update the tracked position + gesture cursor, dispatch a `Move` (hover
    /// diff + capture), then resolve the cursor and apply it only when it changes (#53).
    /// Whether an open modal backdrop should swallow pointer input at `pos` (#219): a `backdrop(true)`
    /// overlay blocks input to content beneath, so a pointer event outside every open overlay's content
    /// is consumed (not dispatched to the tree). The dismiss of the topmost overlay is handled separately
    /// in [`on_mouse_input`](Self::on_mouse_input)'s press path.
    fn modal_blocks_pointer(&self, pos: Point) -> bool {
        self.overlay_layers.has_open_backdrop() && self.overlay_layers.pointer_outside_all(pos)
    }

    fn on_cursor_moved(&mut self, position: winit::dpi::PhysicalPosition<f64>, scale: f32) {
        self.pointer = Point::new(position.x as f32 / scale, position.y as f32 / scale);
        self.gestures.set_cursor(self.pointer);

        // A live drag owns the pointer: track drop targets + the image instead of normal move/hover.
        if matches!(self.drag, DragState::Active(_)) {
            self.drag_move();
            return;
        }
        // An armed drag becomes active once the pointer travels past the threshold (squared compare).
        // Copy the start point out first so the `&self.drag` borrow ends before `begin_drag` (&mut).
        let armed_start = match &self.drag {
            DragState::Armed { start, .. } => Some(*start),
            _ => None,
        };
        if let Some(start) = armed_start {
            if past_drag_threshold(start, self.pointer) {
                self.begin_drag();
                return;
            }
        }

        // A modal backdrop blocks hover to content beneath (#219): the pointer is still tracked above,
        // but the move is not dispatched (and the background cursor is not resolved).
        if self.modal_blocks_pointer(self.pointer) {
            return;
        }

        let ev = MouseEvent {
            kind: MouseKind::Move,
            pos: self.pointer,
            modifiers: self.modifiers,
        };
        dispatch_mouse(
            &mut self.root,
            &self.arena,
            &self.hit_test,
            &ev,
            &mut self.dispatch,
        );

        let resolved = self
            .cursor_reg
            .resolve(&self.hit_test, &self.arena, self.pointer);
        if let Some(icon) = self.cursor_state.apply(resolved) {
            self.window.set_cursor(map_cursor_icon(icon));
        }
    }

    /// Handle a mouse button press/release at the tracked pointer (winit `MouseInput` carries no
    /// position). The persisted [`DispatchState`] maintains pointer capture across a drag.
    fn on_mouse_input(&mut self, state: ElementState, button: winit::event::MouseButton) {
        let button = map_mouse_button(button);
        match state {
            ElementState::Pressed => {
                // A pointer press marks the input modality as pointer, so focus rings stay hidden for
                // click-initiated focus (`:focus-visible`). Deduped inside `set_focus_visible`.
                self.focus.set_focus_visible(false);
                // Interactive overlay dismiss (#219): a press outside every open overlay's content
                // dismisses the topmost dismissible overlay (innermost-first). A dismissal — or an open
                // modal backdrop — consumes the press so it does not reach content beneath. A press
                // inside an overlay, or with no dismissible/modal overlay, falls through to dispatch.
                if !self.overlay_layers.is_empty()
                    && self.overlay_layers.pointer_outside_all(self.pointer)
                {
                    let dismissed = self.overlay_layers.dismiss_topmost();
                    if dismissed || self.overlay_layers.has_open_backdrop() {
                        self.damage.mark_all_dirty();
                        return;
                    }
                }
                let ev = MouseEvent {
                    kind: MouseKind::Down(button),
                    pos: self.pointer,
                    modifiers: self.modifiers,
                };
                dispatch_mouse(
                    &mut self.root,
                    &self.arena,
                    &self.hit_test,
                    &ev,
                    &mut self.dispatch,
                );
                // Arm a drag if the press landed on a drag source (left button, not already dragging).
                if button == MouseButton::Left && matches!(self.drag, DragState::Idle) {
                    if let Some(source) = self.hit_test.pick_where(self.pointer, |f| f.drag_source)
                    {
                        self.drag = DragState::Armed {
                            source,
                            start: self.pointer,
                        };
                    }
                }
            }
            ElementState::Released => {
                // A live drag consumes the release (drop over an accepting target, else a no-op end).
                if matches!(self.drag, DragState::Active(_)) {
                    self.end_drag(true);
                    return;
                }
                // A press that never passed the threshold was a click, not a drag: disarm.
                self.drag = DragState::Idle;
                // A modal backdrop that swallowed the press (#219) also swallows the matching release, so
                // no stray `Up` reaches content beneath.
                if self.modal_blocks_pointer(self.pointer) {
                    return;
                }
                let ev = MouseEvent {
                    kind: MouseKind::Up(button),
                    pos: self.pointer,
                    modifiers: self.modifiers,
                };
                dispatch_mouse(
                    &mut self.root,
                    &self.arena,
                    &self.hit_test,
                    &ev,
                    &mut self.dispatch,
                );
            }
        }
    }

    /// Handle a scroll: dispatch the raw wheel to `on_wheel` **and** the recognized gesture (pan, or
    /// zoom with ctrl) to `on_gesture` — independent opt-in channels (#177 Q1).
    fn on_mouse_wheel(&mut self, delta: MouseScrollDelta, scale: f32) {
        // A modal backdrop blocks scrolling of content beneath (#219): swallow a wheel outside the
        // overlay content so the background does not scroll under the modal.
        if self.modal_blocks_pointer(self.pointer) {
            return;
        }
        let (dx, dy) = scroll_delta(delta, scale);
        let wheel = MouseEvent {
            kind: MouseKind::Wheel { dx, dy },
            pos: self.pointer,
            modifiers: self.modifiers,
        };
        dispatch_mouse(
            &mut self.root,
            &self.arena,
            &self.hit_test,
            &wheel,
            &mut self.dispatch,
        );

        let gesture = self.gestures.recognize_scroll(dx, dy, self.modifiers);
        let target = self.hit_test.pick(self.pointer);
        dispatch_gesture(&mut self.root, &self.arena, target, &gesture);
    }

    /// Handle a native touchpad magnify (winit `PinchGesture`): a zoom gesture about the cursor.
    fn on_pinch(&mut self, delta: f64) {
        let gesture = self.gestures.recognize_magnify(delta as f32);
        let target = self.hit_test.pick(self.pointer);
        dispatch_gesture(&mut self.root, &self.arena, target, &gesture);
    }

    /// Route a keyboard event: IME-owned keys pass through to the OS IME (RK-002); otherwise the pure
    /// [`route_key`] decision (keymap-first-consume, #177 Q2) says whether to consume the chord as an
    /// `Action` or deliver the raw key to the focused node.
    fn on_keyboard(&mut self, event: &winit::event::KeyEvent) {
        // Any key press marks the input modality as keyboard, so focus rings become visible
        // (`:focus-visible`). Stamped before the IME/Escape early-returns so every press counts;
        // deduped inside `set_focus_visible`, so auto-repeat does not churn.
        if event.state.is_pressed() {
            self.focus.set_focus_visible(true);
        }
        if ime_owns_key(event.physical_key) {
            tracing::trace!(key = ?event.physical_key, "ime-owned key passed through");
            return;
        }
        let PhysicalKey::Code(code) = event.physical_key else {
            return; // An unidentified physical key: nothing to route.
        };
        // Esc cancels a live drag (#178) and consumes the key — before keymap/focus routing.
        if code == KeyCode::Escape
            && event.state.is_pressed()
            && matches!(self.drag, DragState::Active(_))
        {
            self.end_drag(false);
            return;
        }
        // Esc dismisses the topmost dismissible overlay (#219), innermost-first, one layer per press,
        // before keymap/focus routing — so Esc closes an open menu/dialog rather than reaching the tree.
        if code == KeyCode::Escape
            && event.state.is_pressed()
            && self.overlay_layers.dismiss_topmost()
        {
            self.damage.mark_all_dirty();
            return;
        }
        let kev = KeyEvent {
            code,
            modifiers: self.modifiers,
            pressed: event.state.is_pressed(),
            repeat: event.repeat,
            // The layout-resolved typed text winit computed for this press (#303) — the direct/ASCII path
            // to the focused editor (IME-composed text comes via `on_ime`). Via `String`: SharedString has
            // no `From<&str>`. Delivered verbatim; the editor decides what to insert.
            text: event.text.as_ref().map(|s| s.to_string().into()),
        };
        match route_key(&self.keymap, &kev) {
            KeyRoute::Consume(action) => self.handle_action(action),
            KeyRoute::Deliver => {
                // Focus trap (#219): while a trap overlay is topmost, don't deliver a raw key to a focused
                // node outside its scope — otherwise typing leaks to the background before Tab (a consumed
                // `FocusNext`, confined in `handle_action`) moves focus into the overlay.
                if let Some(trap) = self.overlay_layers.topmost_trap() {
                    if !self.focus.is_focus_within(&self.arena, trap.node) {
                        return;
                    }
                }
                dispatch_key(&mut self.root, &self.arena, self.focus.focused_node(), &kev)
            }
        }
    }

    /// Apply a keymap-resolved action: framework focus navigation is handled directly; an app-named
    /// action bubbles to `on_action` handlers along the focus chain.
    fn handle_action(&mut self, action: Action) {
        match action {
            // While a focus-trapping overlay is topmost (#219), confine Tab/Shift+Tab to its focusables;
            // otherwise navigate the whole window's tab order.
            Action::FocusNext => match self.overlay_layers.topmost_trap() {
                Some(layer) => self.focus.focus_next_within(&self.arena, layer.node),
                None => self.focus.focus_next(),
            },
            Action::FocusPrev => match self.overlay_layers.topmost_trap() {
                Some(layer) => self.focus.focus_prev_within(&self.arena, layer.node),
                None => self.focus.focus_prev(),
            },
            // Dev-only: F12 toggles the debug overlay (#69); repaint so it appears/disappears.
            #[cfg(debug_assertions)]
            Action::Named(ref name) if name.as_str() == DEBUG_OVERLAY_ACTION => {
                self.debug_overlay = !self.debug_overlay;
                self.damage.mark_all_dirty();
            }
            _ => {
                let focused = self.focus.focused_node();
                dispatch_action(&mut self.root, &self.arena, focused, &action);
            }
        }
    }

    /// The pointer left the window: revert to the default cursor.
    fn on_cursor_left(&mut self) {
        if let Some(icon) = self.cursor_state.apply(CursorIcon::Default) {
            self.window.set_cursor(map_cursor_icon(icon));
        }
    }

    /// Begin an active drag from an armed source (#178): ask the source to produce its payload +
    /// drag-image (`dispatch_drag_start`), then track it. A source that produces nothing disarms.
    fn begin_drag(&mut self) {
        let source = match &self.drag {
            DragState::Armed { source, .. } => *source,
            _ => return,
        };
        let Some((payload, image)) = dispatch_drag_start(&mut self.root, &self.arena, source)
        else {
            self.drag = DragState::Idle;
            return;
        };
        // Deref to the inner payload for its concrete `TypeId` (RK-015: `Box<dyn DragPayload>` itself
        // impls `DragPayload`, so `payload.as_any()` would resolve to the box).
        let payload_type = (*payload).as_any().type_id();
        self.drag_image = image;
        // Fresh arena/layout per drag so the previous drag's nodes don't accumulate.
        self.drag_image_arena = Arena::new();
        self.drag_image_layout = LayoutTree::new();
        // Per-drag child owner: the drag-image's build-once reactive-prop effects scope here and are
        // disposed in `end_drag`, so they don't accumulate on the long-lived window owner (RK-006/020).
        self.drag_owner = Some(self._owner.child());
        self.drag = DragState::Active(ActiveDrag {
            payload,
            payload_type,
            over: None,
            accepted: false,
        });
        // No signal write happens here, so flag damage explicitly to paint the image next frame.
        self.damage.mark_all_dirty();
    }

    /// Track the pointer during an active drag (#178): follow drop targets, firing `on_drag_over` on
    /// entering an accepting target and `on_drag_leave` on leaving it, and repaint the drag-image.
    fn drag_move(&mut self) {
        let (payload_type, over, accepted) = match &self.drag {
            DragState::Active(d) => (d.payload_type, d.over, d.accepted),
            _ => return,
        };
        let raw = self.hit_test.pick_where(self.pointer, |f| f.drop_target);
        if raw != over {
            let (leave, query) = hover_transition(raw, over, accepted);
            // Leave the previous target (only if it had accepted — i.e. `on_drag_over` had fired).
            if let Some(old) = leave {
                dispatch_drag_leave(&mut self.root, &self.arena, old);
            }
            let new_accepted = match query {
                Some(t) => {
                    dispatch_drag_over(&mut self.root, &self.arena, t, payload_type, self.pointer)
                }
                None => false,
            };
            if let DragState::Active(d) = &mut self.drag {
                d.over = raw;
                d.accepted = new_accepted;
            }
        }
        // The pointer moved; repaint so the drag-image tracks it.
        self.damage.mark_all_dirty();
    }

    /// End an active drag (#178): on `commit` (mouse-up) over an accepting target, deliver the payload
    /// (`try_drop`); always clear the target's hover feedback and drop the drag-image. `commit = false`
    /// is a cancel (Esc). A mouse-up off any accepting target passes `commit = true` but delivers
    /// nothing (`over`/`accepted` gate it), so it is a no-op end.
    fn end_drag(&mut self, commit: bool) {
        if let DragState::Active(d) = std::mem::replace(&mut self.drag, DragState::Idle) {
            if let Some(target) = d.over {
                if d.accepted {
                    if commit {
                        dispatch_drop(&mut self.root, &self.arena, target, d.payload, self.pointer);
                    }
                    dispatch_drag_leave(&mut self.root, &self.arena, target);
                }
            }
        }
        // Dispose the per-drag owner so the drag-image's reactive effects are torn down, not leaked
        // onto the window owner (RK-006/020).
        if let Some(owner) = self.drag_owner.take() {
            owner.cleanup();
        }
        // Release any pointer grab a drag source took on mouse-down: a live drag consumes the
        // mouse-up (it never reaches `dispatch_mouse`'s Up path, which would otherwise clear this),
        // so clear it here to avoid misrouting later moves to a stale captured node.
        self.dispatch.captured = None;
        self.drag_image = None;
        self.damage.mark_all_dirty();
    }

    /// An OS file drop (winit `DroppedFile`): deliver a [`FileDrop`] to the topmost file-accepting drop
    /// target under the pointer (#178). winit sends one path per event; each is delivered on its own.
    fn on_dropped_file(&mut self, path: PathBuf) {
        if let Some(target) = self.hit_test.pick_where(self.pointer, |f| f.drop_target) {
            let payload: Box<dyn DragPayload> = Box::new(FileDrop(vec![path]));
            // Unlike begin/move/end_drag, no explicit `mark_all_dirty`: the drop handler's signal
            // writes flag damage via their synchronous effects; a handler that changes nothing
            // visible (e.g. only logs) needs no repaint.
            dispatch_drop(&mut self.root, &self.arena, target, payload, self.pointer);
        }
    }

    fn redraw(&mut self) {
        use wgpu::CurrentSurfaceTexture as Cst;
        let frame = match self.surface.get_current_texture() {
            Cst::Success(frame) | Cst::Suboptimal(frame) => frame,
            Cst::Outdated | Cst::Lost => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
            Cst::Timeout | Cst::Occluded => return,
            Cst::Validation => {
                tracing::warn!("dropped frame: surface validation error");
                return;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let scale = self.window.scale_factor() as f32;

        let viewport = Size {
            w: self.config.width as f32 / scale,
            h: self.config.height as f32 / scale,
        };
        // Read the current App-level theme for this frame (untracked: the per-window theme→damage
        // effect is the sole subscriber that turns a swap into damage, so the render path must not
        // subscribe).
        let theme = self.theme.get_untracked();

        // Debug overlay (#69, dev-only): record this frame's time, and — while the overlay is on —
        // capture the damage rects *before* `render_tree` clears them (using last frame's layout,
        // which is fine for a diagnostic viz). A pending full repaint (no per-node rects) shows as the
        // whole viewport (RK-012).
        #[cfg(debug_assertions)]
        self.frame_clock.record(Instant::now());
        #[cfg(debug_assertions)]
        let debug_damage = if self.debug_overlay {
            let mut rects = self.damage.damage_rects(&self.arena, &self.layout);
            if rects.is_empty() && self.damage.is_dirty() {
                rects.push(kagari_base::Rect::from_xywh(
                    0.0, 0.0, viewport.w, viewport.h,
                ));
            }
            rects
        } else {
            Vec::new()
        };

        // Clear the interactive overlay layer stack before painting; open overlays repopulate it during
        // the paint walk (mirrors `hit_test`, which `render_tree` clears internally). The shell reads
        // last frame's stack during input, so it is cleared here at the start of the next paint (#219).
        self.overlay_layers.clear();

        // Clear the IME caret cell before painting; the focused editor republishes it during the paint walk
        // (mirrors `hit_test`/`overlay_layers`). After the frame, `sync_ime` reads it: a published rect means
        // a text field is focused (enable + track), its absence means none is (disable) — RK-019 (#299).
        self.ime_caret.clear();

        // Advance active animations (#253) by the elapsed `dt` **before** building/painting, so each
        // `Animated`'s signal write resolves into its paint slot this frame (ADR 0001 synchronous
        // effects). `dt` is clamped so a long idle gap (or a paused window) cannot fling a spring; the
        // integrator sub-steps besides. A settled animation's `tick` returns `false` and the ticker drops
        // it — dropping the active source, so the window returns to idle. Ticked every redraw (not just
        // animating ones) so a `snap_to`-orphaned entry is pruned on its own damage-driven repaint.
        let now = Instant::now();
        let dt = self.last_frame.map_or(Duration::ZERO, |last| {
            now.saturating_duration_since(last).min(MAX_FRAME_DT)
        });
        self.last_frame = Some(now);

        // Build + paint under the window owner (#219): build-once reactive-prop effects (overlay
        // open-state, reactive positions) register under it and live for the window, rather than dying
        // with no ambient owner (RK-008). Cloning the owner handle sidesteps borrowing `self._owner`
        // while the closure mutably borrows the rest of `self` (as `drag_owner` does for the drag-image).
        let owner = self._owner.clone();
        let root_id = match owner.with(|| {
            self.ticker.tick_all(dt);
            render_tree(
                &mut self.root,
                &mut self.arena,
                &mut self.layout,
                &mut self.text,
                Some(self.renderer.atlas_mut()),
                Some(&mut self.hit_test),
                Some(&mut self.overlay_reg), // z-slots for stacked/nested overlays (#219)
                Some(&mut self.focus),
                Some(&mut self.cursor_reg),
                Some(&mut self.a11y),
                &mut self.scene,
                viewport,
                &self.damage,
                &theme,
            )
        }) {
            Ok(root_id) => root_id,
            Err(e) => {
                tracing::error!(error = %e, "layout/paint failed");
                return;
            }
        };
        // `root_id` is only read by the (dev-only) debug overlay below.
        #[cfg(not(debug_assertions))]
        let _ = root_id;

        // Push the derived accessibility tree to the OS (#67). `update_if_active` is a no-op unless an
        // assistive tech has activated, so this is free when a11y is idle; it runs on the paint pass
        // (damage-gated), so the tree stays in sync with what changed. Disjoint field borrows: `&mut
        // adapter` with `&a11y`/`&arena`/`&focus`.
        let a11y_scale = self.window.scale_factor();
        self.adapter.update_if_active(|| {
            self.a11y
                .build_update(&self.arena, self.focus.focused_node(), a11y_scale)
        });

        // Drive the OS IME from the caret area the focused editor just published (#299): enable/track while a
        // text field is focused, disable on leave/unmount.
        self.sync_ime();

        // Paint the cursor-tracked drag-image on top of the main tree (#178), into the same scene.
        // Built under the per-drag owner so any reactive props register there and dispose when the
        // drag ends, rather than leaking onto the window owner (RK-006/008/020).
        if let Some(owner) = self.drag_owner.clone() {
            owner.with(|| {
                if let Some(image) = self.drag_image.as_mut() {
                    if let Err(e) = paint_element_into(
                        image,
                        &mut self.drag_image_arena,
                        &mut self.drag_image_layout,
                        &mut self.text,
                        Some(self.renderer.atlas_mut()),
                        &mut self.scene,
                        self.pointer,
                        viewport,
                        true, // keep the ghost inside the window (#239)
                        &self.damage,
                        &theme,
                    ) {
                        tracing::error!(error = %e, "drag-image paint failed");
                    }
                }
            });
        }

        // Debug overlay injection (#69, dev-only): above all content, into the same scene, just before
        // present. Gather diagnostics (atlas usage read into owned values before borrowing the atlas
        // mutably for the HUD text), then inject damage/bounds quads + the FPS/atlas HUD.
        #[cfg(debug_assertions)]
        if self.debug_overlay {
            let (mono_atlas, rgba_atlas) = self.renderer.atlas_usage();
            let diagnostics = crate::debug::Diagnostics {
                fps: self.frame_clock.fps(),
                frame_ms: self.frame_clock.frame_ms(),
                mono_atlas,
                rgba_atlas,
                damage_rects: debug_damage,
                element_bounds: crate::debug::collect_element_bounds(
                    &self.arena,
                    &self.layout,
                    root_id,
                ),
            };
            crate::debug::paint_debug_overlay(
                &mut self.scene,
                &mut self.text,
                Some(self.renderer.atlas_mut()),
                &diagnostics,
            );
        }

        if let Err(e) = self.renderer.render(
            &mut self.scene,
            &view,
            (self.config.width, self.config.height),
            scale,
        ) {
            tracing::error!(error = %e, "render failed");
        }
        frame.present();
    }
}

/// Creates the shared GPU (adapter + device + queue) against the first window's surface. The device
/// is surface-agnostic, so it serves all windows; each surface picks its own format later.
async fn create_gpu(
    instance: &wgpu::Instance,
    surface: &wgpu::Surface<'static>,
) -> Result<Gpu, AppError> {
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(surface),
            force_fallback_adapter: false,
        })
        .await
        .map_err(|e| AppError::DeviceInit(e.to_string()))?;
    tracing::info!(backend = ?adapter.get_info().backend, "renderer adapter selected");

    let (device, queue) = create_device(&adapter).await?;
    Ok(Gpu {
        adapter,
        device,
        queue,
    })
}

/// Requests a fresh device + queue from `adapter` (`Arc`-wrapped). Used for first-window GPU creation
/// and to regenerate the device after a loss (#64) — the adapter survives a device loss.
async fn create_device(
    adapter: &wgpu::Adapter,
) -> Result<(Arc<wgpu::Device>, Arc<wgpu::Queue>), AppError> {
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("kagari.device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
        })
        .await
        .map_err(|e| AppError::DeviceInit(e.to_string()))?;
    Ok((Arc::new(device), Arc::new(queue)))
}

/// Registers a device-lost callback (#64): on a genuine device loss (TDR / GPU reset / sleep-wake),
/// wgpu invokes this — flag the loss and wake the loop (via the #66 proxy) so [`App::about_to_wait`]
/// polls the flag and runs [`App::recover_device`]. Set on every device the app creates.
fn register_device_lost(
    device: &wgpu::Device,
    flag: &Arc<AtomicBool>,
    proxy_cell: &Arc<OnceLock<EventLoopProxy<UserEvent>>>,
) {
    let flag = Arc::clone(flag);
    let proxy_cell = Arc::clone(proxy_cell);
    device.set_device_lost_callback(move |reason, message| {
        // Only a genuine loss (`Unknown`, e.g. a driver TDR/reset) warrants recovery. `Destroyed`
        // fires solely from an explicit `device.destroy()` (which the app never calls today), so
        // treating it as a loss would arm a spurious full-recreate — ignore it (RK-021).
        if reason != wgpu::DeviceLostReason::Unknown {
            tracing::debug!(reason = ?reason, "device-lost callback (non-loss); ignored");
            return;
        }
        tracing::error!(reason = ?reason, message = %message, "wgpu device lost");
        flag.store(true, Ordering::SeqCst);
        // Wake the loop so `about_to_wait` polls the flag promptly even if the app is idle. The empty
        // closure is a no-op; the wake is the point (reuses the #66 bridge).
        if let Some(proxy) = proxy_cell.get() {
            let _ = proxy.send_event(UserEvent::Ui(Box::new(|| {})));
        }
    });
}

/// Builds a surface configuration for `surface` against the shared `adapter`, choosing an sRGB
/// swapchain format so the HW performs the linear→sRGB encode (#10). Per-surface (formats can differ).
fn surface_config(
    surface: &wgpu::Surface<'static>,
    adapter: &wgpu::Adapter,
    size: winit::dpi::PhysicalSize<u32>,
) -> Result<wgpu::SurfaceConfiguration, AppError> {
    let caps = surface.get_capabilities(adapter);
    let format = caps
        .formats
        .iter()
        .copied()
        .find(wgpu::TextureFormat::is_srgb)
        .or_else(|| caps.formats.first().copied())
        .ok_or_else(|| AppError::DeviceInit("surface has no supported formats".to_string()))?;
    let alpha_mode = caps
        .alpha_modes
        .first()
        .copied()
        .unwrap_or(wgpu::CompositeAlphaMode::Auto);
    Ok(wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        width: size.width.max(1),
        height: size.height.max(1),
        present_mode: wgpu::PresentMode::Fifo,
        alpha_mode,
        view_formats: vec![],
        desired_maximum_frame_latency: 2,
    })
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // Drain queued windows (created here per winit 0.30). On a spurious re-resume there is
        // nothing pending, so this is a no-op.
        let pending = std::mem::take(&mut self.pending);
        for pw in pending {
            match self.create_window(event_loop, pw) {
                Ok((winit_id, public_id, state)) => {
                    state.window.request_redraw();
                    self.windows.insert(winit_id, state);
                    tracing::info!(window = public_id.raw(), "window created");
                }
                Err(e) => {
                    // Per-window failure: log and skip, keeping other windows alive (§1.11).
                    tracing::error!(error = %e, "failed to create window; skipping");
                }
            }
        }
        // Nothing to display — no window was opened, or every requested window failed to init — so
        // exit (a windowless app is fatal, §1.13). A spurious re-resume keeps existing windows, so
        // this only fires when the map is truly empty.
        if self.windows.is_empty() {
            tracing::error!("no window to display; exiting");
            event_loop.exit();
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        id: WinitWindowId,
        event: WindowEvent,
    ) {
        // Feed every window event to the accessibility adapter first (#67) so the AT keeps tree/focus
        // synced (runs even for a degraded window, which still reports its last-good semantics). Takes
        // `&event`; the match below then consumes it.
        if let Some(state) = self.windows.get_mut(&id) {
            state.adapter.process_event(&state.window, &event);
        }
        // Capture window geometry into the persisted state on move/resize (#68). Updates the in-memory
        // slot + arms the debounce; the disk write happens in `about_to_wait` once the burst settles.
        if matches!(event, WindowEvent::Moved(_) | WindowEvent::Resized(_)) {
            self.capture_window_geometry(id, event_loop);
        }
        match event {
            WindowEvent::CloseRequested => {
                // Remove the closed window (dropping its surface then its shared-device Arc clone;
                // dropping its `_owner` disposes the window's reactive effects, ARK-002).
                self.windows.remove(&id);
                if self.windows.is_empty() {
                    event_loop.exit();
                }
            }
            other => {
                let Some(state) = self.windows.get_mut(&id) else {
                    return;
                };
                // A degraded window (a prior panic) skips all further frame/event work; only
                // `CloseRequested` (handled above) still acts on it. (#65)
                if state.degraded {
                    return;
                }
                let scale = state.window.scale_factor() as f32;
                // Frame/window panic boundary (#65): the per-frame work (`redraw` = build/layout/paint)
                // and event dispatch below run user code (handlers, #177/#178). Catch a panic so one
                // element/handler cannot abort the whole app. Reborrow `state` into the guarded
                // closure so the original binding is usable afterward to degrade the window on `Err`.
                let outcome = {
                    let state = &mut *state;
                    run_guarded(move || match other {
                        WindowEvent::Resized(size) => {
                            state.config.width = size.width.max(1);
                            state.config.height = size.height.max(1);
                            state.surface.configure(&state.device, &state.config);
                            state.window.request_redraw();
                        }
                        WindowEvent::RedrawRequested => state.redraw(),
                        WindowEvent::Ime(ime) => state.on_ime(ime),
                        WindowEvent::ModifiersChanged(m) => {
                            state.modifiers = map_modifiers(m.state())
                        }
                        // Clear tracked modifiers on focus loss: winit doesn't guarantee a
                        // `ModifiersChanged` when focus leaves (e.g. Alt+Tab), so a held modifier
                        // would otherwise stay stuck and leak into the next click/key.
                        WindowEvent::Focused(false) => state.modifiers = Modifiers::default(),
                        WindowEvent::CursorMoved { position, .. } => {
                            state.on_cursor_moved(position, scale)
                        }
                        WindowEvent::CursorLeft { .. } => state.on_cursor_left(),
                        WindowEvent::MouseInput {
                            state: btn_state,
                            button,
                            ..
                        } => state.on_mouse_input(btn_state, button),
                        WindowEvent::MouseWheel { delta, .. } => state.on_mouse_wheel(delta, scale),
                        WindowEvent::PinchGesture { delta, .. } => state.on_pinch(delta),
                        WindowEvent::DroppedFile(path) => state.on_dropped_file(path),
                        WindowEvent::KeyboardInput { event, .. } => state.on_keyboard(&event),
                        _ => {}
                    })
                };
                if outcome.is_err() {
                    // Degrade this window (skip further work) but keep the app + other windows alive.
                    // The panic hook already logged the location + message; this only ties it to the
                    // window (avoiding a duplicate of the payload/location the hook emitted).
                    state.degraded = true;
                    tracing::error!(window = ?id, "window degraded after a panic");
                }
            }
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            // Run a background-posted UI closure on the UI thread under the app root owner (#66): its
            // signal writes are tracked and their synchronous effects flag damage → redraw next frame.
            // The hot-reload watcher's theme swap arrives this way, re-running every window's
            // theme→damage effect so all windows reskin. Guard it with the panic boundary (#65) — a
            // panic in the (consumer-supplied) closure is caught + logged so it can't abort the app.
            // It is app-level (not tied to a window), so there is nothing to degrade; just survive.
            UserEvent::Ui(closure) => {
                if run_guarded(|| run_ui_event(&self.root_owner, closure)).is_err() {
                    tracing::error!("a background UI closure panicked; dropped (app kept alive)");
                }
            }
            // Accessibility (#67): the adapter asks for the initial tree, requests an action, or
            // deactivates. MVP is read-only — serve the tree + focus; action routing is post-MVP.
            UserEvent::Accessibility(event) => self.on_accessibility_event(event),
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Recover from a lost GPU device before driving redraws (#64, specs §1.11/§2.9). The device-lost
        // callback (on a wgpu thread) sets this flag and wakes the loop via the #66 proxy; swap it back
        // to false and regenerate the device + rebuild every window once here on the UI thread.
        if self.device_lost.swap(false, Ordering::SeqCst) {
            self.recover_device();
        }
        // Debounced persistence write (#68): once a geometry change has settled for `PERSIST_DEBOUNCE`,
        // write once; before then, ask the loop to wake at the deadline (`WaitUntil`) so the write
        // still happens when the app is otherwise idle. Coalesces a burst of `Moved`/`Resized` events.
        if self.persist.is_dirty() {
            // Due when the debounce window has elapsed, or (defensively) when the change time is
            // somehow absent — never leave a dirty state un-written until exit.
            let due = self
                .persist
                .last_change()
                .is_none_or(|last| last.elapsed() >= PERSIST_DEBOUNCE);
            if due {
                if let Err(e) = self.persist.save() {
                    tracing::error!(error = %e, "failed to save persisted state");
                }
                event_loop.set_control_flow(ControlFlow::Wait);
            } else if let Some(last) = self.persist.last_change() {
                event_loop.set_control_flow(ControlFlow::WaitUntil(last + PERSIST_DEBOUNCE));
            }
        }
        // Per-window hybrid driving (#36, §1.6): request a redraw only for windows that are dirty or
        // animating; an idle window stays idle. Iterates in place (no per-frame allocation, perf.md).
        for state in self.windows.values() {
            // A degraded window (#65) never repaints — skip it (its `redraw` would early-return anyway).
            if !state.degraded
                && should_redraw(
                    state.damage.is_dirty(),
                    state.scheduler.has_active_sources(),
                )
            {
                state.window.request_redraw();
            }
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // Final on-exit save of any pending persisted-state changes (#68, §7.3). Logged, not fatal.
        self.persist.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::div;

    #[test]
    fn winit_ime_should_map_to_ime_event() {
        assert_eq!(
            map_ime_event(Ime::Preedit("あ".to_string(), Some((0, 3)))),
            Some(ImeEvent::Preedit {
                text: "あ".to_string(),
                cursor: Some((0, 3)),
            })
        );
        assert_eq!(
            map_ime_event(Ime::Commit("日本".to_string())),
            Some(ImeEvent::Commit("日本".to_string()))
        );
        assert_eq!(map_ime_event(Ime::Enabled), None);
        assert_eq!(map_ime_event(Ime::Disabled), None);
    }

    #[test]
    fn ime_owned_key_should_not_be_handled() {
        for code in [
            KeyCode::Lang5,
            KeyCode::Convert,
            KeyCode::NonConvert,
            KeyCode::KanaMode,
            KeyCode::Katakana,
            KeyCode::Lang1,
        ] {
            assert!(
                ime_owns_key(PhysicalKey::Code(code)),
                "{code:?} should be IME-owned"
            );
        }
        assert!(!ime_owns_key(PhysicalKey::Code(KeyCode::KeyA)));
        assert!(!ime_owns_key(PhysicalKey::Code(KeyCode::Enter)));
    }

    #[test]
    fn window_options_default_should_be_native() {
        let opts = WindowOptions::default();
        assert_eq!(opts.decorations, Decorations::Native);
        assert_eq!(opts.title, "kagari");
        assert_eq!(opts.inner_size, Size { w: 800.0, h: 600.0 });
    }

    #[test]
    fn app_open_window_should_mint_distinct_window_ids() {
        // `App::new` creates only the wgpu instance (no adapter/device), so this is headless-safe;
        // `open_window` merely queues the window (created later in `resumed`). Use an in-memory
        // persistence service so the test never touches the real OS config dir (#68).
        let mut app = App::with_persistence(PersistenceService::in_memory()).expect("app shell");
        let a = app
            .open_window(WindowOptions::default(), div)
            .expect("first window id");
        let b = app
            .open_window(WindowOptions::default().title("second"), div)
            .expect("second window id");
        assert_ne!(a, b, "each window gets a distinct public id");
        assert_eq!(app.pending.len(), 2, "both windows are queued for creation");
    }

    #[test]
    fn window_theme_effect_should_dispose_on_owner_drop() {
        // A window's theme→damage effect must stop firing once its child owner is dropped — window
        // close drops `WindowState._owner`, and `Owner`'s drop disposes the effect (ARK-002; no leak,
        // no damage flagged on a closed window). Synchronous + owner held to the end (RK-003/005/006).
        let parent = Owner::new();
        parent.set();

        let child = parent.child();
        let damage = Arc::new(DamageState::default());
        let theme = RwSignal::new(Arc::new(Theme::light()));
        child.with(|| bind_window_theme_damage(theme, Arc::clone(&damage)));
        damage.clear();

        // Close the window: dropping its child owner disposes the effect.
        drop(child);
        theme.set(Arc::new(Theme::dark()));
        assert!(
            !damage.is_dirty(),
            "a closed window's disposed effect no longer flags damage"
        );

        drop(parent);
    }

    #[test]
    fn window_theme_effect_should_flag_damage_on_swap_and_expose_context() {
        use crate::reactive::{ReadSignal, use_context};

        // Synchronous `ImmediateEffect` → hang-free; keep the owner alive (set stores a Weak, RK-003/005).
        let owner = Owner::new();
        owner.set();

        let damage = Arc::new(DamageState::default());
        let theme = RwSignal::new(Arc::new(Theme::light()));
        // App-side: provide the read-only theme handle into context (what `App::new` does).
        provide_context(theme.read_only());
        // Per-window: bind the theme→damage effect (what `create_window` does under the child owner).
        bind_window_theme_damage(theme, Arc::clone(&damage));

        assert!(damage.is_dirty(), "binding flags an initial full repaint");
        damage.clear();
        assert!(!damage.is_dirty());

        theme.set(Arc::new(Theme::dark()));
        assert!(
            damage.is_dirty(),
            "a theme swap flags this window's full damage"
        );

        let ctx = use_context::<ReadSignal<Arc<Theme>>>().expect("theme provided in context");
        assert_eq!(
            ctx.get_untracked(),
            Arc::new(Theme::dark()),
            "the context reflects the swapped theme"
        );

        drop(owner);
    }

    #[test]
    fn map_modifiers_should_translate_winit_state() {
        use winit::keyboard::ModifiersState;
        let m = map_modifiers(ModifiersState::CONTROL | ModifiersState::SHIFT);
        assert!(m.ctrl && m.shift, "ctrl+shift are set");
        assert!(!m.alt && !m.meta, "alt/meta are not");
        assert_eq!(
            map_modifiers(ModifiersState::empty()),
            Modifiers::default(),
            "no modifiers maps to the default"
        );
    }

    #[test]
    fn map_mouse_button_should_map_common_and_extra() {
        use winit::event::MouseButton as W;
        assert_eq!(map_mouse_button(W::Left), MouseButton::Left);
        assert_eq!(map_mouse_button(W::Right), MouseButton::Right);
        assert_eq!(map_mouse_button(W::Middle), MouseButton::Middle);
        // Extra buttons fold into `Other` with a platform index.
        assert_eq!(map_mouse_button(W::Back), MouseButton::Other(3));
        assert_eq!(map_mouse_button(W::Forward), MouseButton::Other(4));
        assert_eq!(map_mouse_button(W::Other(7)), MouseButton::Other(7));
    }

    #[test]
    fn map_cursor_icon_should_cover_every_kagari_icon() {
        use winit::window::CursorIcon as W;
        // Representative of each mapped shape (the match is exhaustive, so a new kagari variant is a
        // compile error until mapped).
        assert_eq!(map_cursor_icon(CursorIcon::Default), W::Default);
        assert_eq!(map_cursor_icon(CursorIcon::Pointer), W::Pointer);
        assert_eq!(map_cursor_icon(CursorIcon::Text), W::Text);
        assert_eq!(map_cursor_icon(CursorIcon::Grabbing), W::Grabbing);
        assert_eq!(map_cursor_icon(CursorIcon::ColResize), W::ColResize);
        assert_eq!(map_cursor_icon(CursorIcon::NsResize), W::NsResize);
    }

    #[test]
    fn scroll_delta_should_scale_line_and_pixel() {
        use winit::dpi::PhysicalPosition;
        // A line wheel scales by WHEEL_LINE_PX.
        assert_eq!(
            scroll_delta(MouseScrollDelta::LineDelta(0.0, 1.0), 1.0),
            (0.0, WHEEL_LINE_PX)
        );
        // A pixel delta is physical → divided by the scale factor to logical px.
        assert_eq!(
            scroll_delta(
                MouseScrollDelta::PixelDelta(PhysicalPosition::new(20.0, 40.0)),
                2.0
            ),
            (10.0, 20.0)
        );
    }

    #[test]
    fn key_chord_should_carry_code_and_modifiers() {
        let mods = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        let ev = KeyEvent {
            code: KeyCode::KeyS,
            modifiers: mods,
            pressed: true,
            repeat: false,
            text: None,
        };
        let chord = key_chord(&ev);
        assert_eq!(chord.code, KeyCode::KeyS);
        assert_eq!(chord.modifiers, mods);
    }

    #[test]
    fn drag_threshold_should_activate_only_past_distance() {
        let start = Point::new(10.0, 10.0);
        assert!(
            !past_drag_threshold(start, Point::new(11.0, 11.0)),
            "a tiny move stays armed (a click, not a drag)"
        );
        assert!(
            past_drag_threshold(start, Point::new(20.0, 20.0)),
            "moving past the threshold begins a drag"
        );
    }

    #[test]
    fn hover_transition_should_leave_only_a_previously_accepted_target() {
        let a = NodeId::from_raw(1);
        let b = NodeId::from_raw(2);
        // Entering a target from empty: nothing to leave; query the new one.
        assert_eq!(hover_transition(Some(a), None, false), (None, Some(a)));
        // Accepting target a → another target b: leave a, query b.
        assert_eq!(hover_transition(Some(b), Some(a), true), (Some(a), Some(b)));
        // Non-accepting target a → b: do NOT leave a (it never received drag-over); query b.
        assert_eq!(hover_transition(Some(b), Some(a), false), (None, Some(b)));
        // Accepting target a → off all targets: leave a, nothing to query.
        assert_eq!(hover_transition(None, Some(a), true), (Some(a), None));
    }

    /// A key event for `code` with default modifiers.
    fn key(code: KeyCode, pressed: bool) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: Modifiers::default(),
            pressed,
            repeat: false,
            text: None,
        }
    }

    #[test]
    fn route_key_should_consume_bound_chord_on_press() {
        // The default keymap binds Tab → FocusNext, so a Tab press is consumed as that action.
        let km = Keymap::default();
        assert_eq!(
            route_key(&km, &key(KeyCode::Tab, true)),
            KeyRoute::Consume(Action::FocusNext)
        );
    }

    #[test]
    fn route_key_should_deliver_bound_chord_on_release() {
        // Actions are press-triggered: the Tab *release* is delivered to the focused node, not
        // consumed (keymap is only consulted on press).
        let km = Keymap::default();
        assert_eq!(route_key(&km, &key(KeyCode::Tab, false)), KeyRoute::Deliver);
    }

    #[test]
    fn route_key_should_deliver_unbound_key() {
        // An unbound key press falls through to the focused node.
        let km = Keymap::default();
        assert_eq!(route_key(&km, &key(KeyCode::KeyA, true)), KeyRoute::Deliver);
    }

    #[test]
    fn panic_boundary_should_catch_and_report_message() {
        // Silence the default panic hook during the intentional panic so test output stays clean
        // (kagari-core lib tests run serially per RK-005, so swapping the global hook is safe here).
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let caught = run_guarded(|| panic!("boom"));
        std::panic::set_hook(prev);
        assert_eq!(
            caught,
            Err("boom".to_string()),
            "a panic is caught and its message reported so the caller can degrade"
        );
    }

    #[test]
    fn panic_boundary_should_pass_ok_through() {
        assert!(
            run_guarded(|| { /* no panic */ }).is_ok(),
            "a non-panicking closure passes through as Ok"
        );
    }

    #[test]
    fn panic_message_should_extract_str_and_string() {
        // A `&str` payload (panic!("literal")) and a `String` payload (panic!("{}", …)); anything
        // else falls back.
        let s: &str = "literal";
        assert_eq!(panic_message(&s), "literal");
        let owned = String::from("owned");
        assert_eq!(panic_message(&owned), "owned");
        let non_string: u32 = 42;
        assert_eq!(panic_message(&non_string), "<non-string panic>");
    }
}
