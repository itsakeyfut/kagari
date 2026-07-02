//! App shell (#63): an [`App`] owns the shared GPU (adapter/device/queue), the app root reactive
//! owner, the App-level theme (single source), and the winit event loop; each window owns its
//! surface, root view, renderer, and per-window scene/scheduler/damage plus a per-window child
//! reactive owner. App→N windows (specs §1.13); MVP ships a single window.
//!
//! The shared device is created **lazily on the first window's `resumed`** (an adapter needs a
//! compatible surface; a wgpu device is surface-agnostic, so the first window's device serves all
//! windows) and `Arc`-shared into each window's renderer. The glyph/image atlases stay per-window
//! (sharing them is #191); FontDb sharing is #192. Design confirmed by a Tier-2 arch review.

use std::collections::HashMap;
use std::sync::Arc;

use kagari_base::{Size, WindowId};
use kagari_layout::LayoutTree;
use kagari_text::{FontDb, ImeEvent, TextSystem};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{Ime, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowAttributes, WindowId as WinitWindowId};

use kagari_style::Theme;

use crate::arena::Arena;
use crate::damage::DamageState;
use crate::element::{AnyElement, IntoElement};
use crate::error::AppError;
use crate::paint::render_tree;
use crate::reactive::prelude::*;
use crate::reactive::{Owner, RwSignal, create_effect, provide_context};
use crate::scheduler::{Scheduler, should_redraw};

/// The winit user event carrying a hot-reloaded theme to the UI thread (#44). The theme watcher runs
/// on a background thread and posts this via the `EventLoopProxy`; [`App::user_event`] writes it into
/// the App-level theme signal on the UI thread. Without `hot-reload` there is no user event.
#[cfg(feature = "hot-reload")]
struct ThemeReload(Theme);

#[cfg(feature = "hot-reload")]
type UserEvent = ThemeReload;
#[cfg(not(feature = "hot-reload"))]
type UserEvent = ();

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
    /// The app root reactive owner: each window's owner is a child of this, so App-provided context
    /// (theme/services, §1.8) is inherited by every window. Held for the app's lifetime (RK-008).
    root_owner: Owner,
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
    ime_enabled: bool,
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
    /// `resumed` (headless-safe: `App::new` needs no adapter).
    pub fn new() -> Result<Self, AppError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            flags: wgpu::InstanceFlags::default(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            backend_options: wgpu::BackendOptions::default(),
            display: None,
        });
        // The app root owner scopes App-level context (theme/services) so every window (a child
        // owner) inherits it. The theme is the App-level single source (§1.8).
        let root_owner = Owner::new();
        let theme = root_owner.with(|| {
            let theme = RwSignal::new(Arc::new(Theme::light()));
            provide_context(theme.read_only());
            theme
        });
        Ok(Self {
            instance,
            gpu: None,
            windows: HashMap::new(),
            pending: Vec::new(),
            next_id: 0,
            root_owner,
            theme,
            #[cfg(feature = "hot-reload")]
            theme_path: None,
            #[cfg(feature = "hot-reload")]
            theme_watcher: None,
        })
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
        match kagari_style::ThemeWatcher::new(path, move |theme| {
            let _ = proxy.send_event(ThemeReload(theme));
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
        let attrs = WindowAttributes::default()
            .with_title(pending.opts.title.clone())
            .with_inner_size(LogicalSize::new(
                pending.opts.inner_size.w,
                pending.opts.inner_size.h,
            ));
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
            self.gpu = Some(pollster::block_on(create_gpu(&self.instance, &surface))?);
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
        window.set_ime_allowed(true);

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
        let root = child_owner.with(|| {
            bind_window_theme_damage(theme, Arc::clone(&damage));
            (pending.root)()
        });

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
                scheduler: Scheduler::new(),
                ime_enabled: false,
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

/// Route a key event. IME-owned toggle/conversion keys are passed through to the OS IME and never
/// consumed as a shortcut; other keys are where the app keymap will dispatch (live wiring is #177).
fn route_key_event(event: &KeyEvent) {
    if ime_owns_key(event.physical_key) {
        tracing::trace!(key = ?event.physical_key, "ime-owned key passed through");
    }
}

impl WindowState {
    /// Handle a winit IME event: track enable state, report the caret area on enable, and forward
    /// preedit/commit (as `ImeEvent`) to the text layer when enabled.
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
                // #25 routes this to the focused TextBuffer. Log only the event shape, never the
                // composed text (IME content is user input, may include passwords).
                let (kind, text_len) = match &ev {
                    ImeEvent::Preedit { text, .. } => ("preedit", text.len()),
                    ImeEvent::Commit(text) => ("commit", text.len()),
                };
                tracing::debug!(kind, text_len, "ime event");
            }
        }
    }

    /// Report the IME candidate-window area to the OS. A default until #25 drives it from the caret.
    fn report_ime_caret_area(&self) {
        use winit::dpi::LogicalPosition;
        self.window
            .set_ime_cursor_area(LogicalPosition::new(0.0, 0.0), LogicalSize::new(1.0, 16.0));
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
        if let Err(e) = render_tree(
            &mut self.root,
            &mut self.arena,
            &mut self.layout,
            &mut self.text,
            Some(self.renderer.atlas_mut()),
            None,
            None,
            &mut self.scene,
            viewport,
            &self.damage,
            &theme,
        ) {
            tracing::error!(error = %e, "layout/paint failed");
            return;
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

    Ok(Gpu {
        adapter,
        device: Arc::new(device),
        queue: Arc::new(queue),
    })
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
                match other {
                    WindowEvent::Resized(size) => {
                        state.config.width = size.width.max(1);
                        state.config.height = size.height.max(1);
                        state.surface.configure(&state.device, &state.config);
                        state.window.request_redraw();
                    }
                    WindowEvent::RedrawRequested => state.redraw(),
                    WindowEvent::Ime(ime) => state.on_ime(ime),
                    WindowEvent::KeyboardInput { event, .. } => route_key_event(&event),
                    _ => {}
                }
            }
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: UserEvent) {
        // A theme hot-reload posted from the watcher thread (#44). Writing the App-level signal on
        // the UI thread re-runs every window's theme→damage effect → all windows reskin next frame.
        #[cfg(feature = "hot-reload")]
        {
            let ThemeReload(theme) = _event;
            self.theme.set(Arc::new(theme));
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // Per-window hybrid driving (#36, §1.6): request a redraw only for windows that are dirty or
        // animating; an idle window stays idle. Iterates in place (no per-frame allocation, perf.md).
        for state in self.windows.values() {
            if should_redraw(
                state.damage.is_dirty(),
                state.scheduler.has_active_sources(),
            ) {
                state.window.request_redraw();
            }
        }
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
        // `open_window` merely queues the window (created later in `resumed`).
        let mut app = App::new().expect("app shell");
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
}
