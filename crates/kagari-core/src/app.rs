//! Minimal app shell: a single winit window with wgpu init that clears and presents.
//!
//! This is the Phase-1 skeleton. The shared device lives on `App` (gpu.md §1); the
//! richer element/reactive API and multi-window support arrive in Phase 3 (specs §1.13).

use std::sync::Arc;

use kagari_base::Size;
use kagari_layout::LayoutTree;
use kagari_text::{FontDb, ImeEvent, TextSystem};
use winit::application::ApplicationHandler;
use winit::event::{Ime, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowAttributes, WindowId};

use kagari_style::Theme;

use crate::arena::Arena;
use crate::damage::DamageState;
use crate::element::{AnyElement, IntoElement, div, text};
use crate::error::AppError;
use crate::paint::render_tree;
use crate::reactive::prelude::*;
use crate::reactive::{Owner, RwSignal, create_effect, provide_context};
use crate::scheduler::{Scheduler, should_redraw};

/// The application shell. Owns the shared wgpu instance and, once resumed, the
/// single window's GPU state.
pub struct App {
    instance: wgpu::Instance,
    window: Option<WindowState>,
}

struct WindowState {
    // `Arc<Window>` lets wgpu hold a `Surface<'static>` via the safe `create_surface`
    // path — no hand-written raw-window-handle lifetime and no `unsafe`.
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    // `device` is kept for surface reconfigure; the queue is owned by the renderer.
    device: Arc<wgpu::Device>,
    config: wgpu::SurfaceConfiguration,
    renderer: kagari_render::Renderer,
    // The render scene, rebuilt each frame by the paint walk; its buffers are reused.
    scene: kagari_render::Scene,
    // Element-tree paint-pass state (#34): the retained arena, the layout tree, the text
    // system (shaping + glyph rasterization), the root element, and the damage sink (#35).
    arena: Arena,
    layout: LayoutTree,
    text: TextSystem,
    root: AnyElement,
    damage: Arc<DamageState>,
    // The root reactive owner (#43, RK-008): established before the element tree is built so
    // context/effects (theme, reactive props) bind to a stable scope that lives for the window's
    // lifetime. Held only to keep them alive — `set()` stores a Weak (RK-003).
    _owner: Owner,
    // The current theme, held in the root reactive context (#43, specs §1.8). Read each frame to
    // resolve tokens; writing the signal reskins every token and flags full damage (swap API: #44).
    theme: RwSignal<Arc<Theme>>,
    // Hybrid frame scheduler (#36): tracks active sources for continuous driving; `about_to_wait`
    // gates `request_redraw` on damage/active state so the app is idle when nothing changes.
    scheduler: Scheduler,
    // Whether the OS IME is currently composing into this window (set by
    // `Ime::Enabled`/`Disabled`). Gates preedit/commit forwarding.
    ime_enabled: bool,
}

/// The demo root element: a colored panel containing a line of text.
fn demo_root() -> AnyElement {
    use kagari_base::Color;
    use kagari_render::Background;
    div()
        .background(Background::Solid(Color::from_srgb([0.12, 0.13, 0.16, 1.0])))
        .child(text("Hello, kagari"))
        .into_element()
}

/// Wires `theme` into the root reactive context and turns every swap into a full repaint.
///
/// Descendants receive a read-only handle via `provide_context`, so token resolution can read the
/// current theme (specs §1.8). The effect subscribes to the theme: any swap re-runs it (synchronous
/// `ImmediateEffect`, ADR 0001) and flags full paint-damage, so the scheduler wakes and the next
/// frame re-resolves every token. Must be called with the root `Owner` current (RK-008).
fn provide_root_theme(theme: RwSignal<Arc<Theme>>, damage: Arc<DamageState>) {
    provide_context(theme.read_only());
    create_effect(move || {
        // Read to subscribe; the App reads the resolved value each frame, so the only job here is
        // to turn a swap into damage.
        let _ = theme.get();
        damage.mark_all_dirty();
    });
}

impl App {
    pub fn new() -> Result<Self, AppError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            flags: wgpu::InstanceFlags::default(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            backend_options: wgpu::BackendOptions::default(),
            display: None,
        });
        Ok(Self {
            instance,
            window: None,
        })
    }

    /// Run the winit event loop until the window closes.
    pub fn run(mut self) -> Result<(), AppError> {
        let event_loop = EventLoop::new().map_err(|e| AppError::WindowCreate(e.to_string()))?;
        event_loop.set_control_flow(ControlFlow::Wait);
        event_loop
            .run_app(&mut self)
            .map_err(|e| AppError::WindowCreate(e.to_string()))
    }

    fn create_window_state(&self, event_loop: &ActiveEventLoop) -> Result<WindowState, AppError> {
        let window = Arc::new(
            event_loop
                .create_window(WindowAttributes::default().with_title("kagari"))
                .map_err(|e| AppError::WindowCreate(e.to_string()))?,
        );
        let surface = self
            .instance
            .create_surface(window.clone())
            .map_err(|e| AppError::DeviceInit(e.to_string()))?;
        let (device, queue, config) =
            pollster::block_on(init_gpu(&self.instance, &surface, window.inner_size()))?;
        let device = Arc::new(device);
        let queue = Arc::new(queue);
        surface.configure(&device, &config);

        // Allow the OS IME for this window. No focus system yet, so the window is the
        // sole focus target; focus-driven enable/disable arrives with the focus layer.
        window.set_ime_allowed(true);

        // The queue is moved into the renderer; the app shell keeps only `device`.
        let renderer = kagari_render::Renderer::new(
            device.clone(),
            queue,
            (config.width, config.height),
            config.format,
        );

        // Establish the root reactive owner before building the element tree so context/effects
        // (theme, reactive props) bind to a stable scope that lives with the window (RK-008).
        let owner = Owner::new();
        owner.set();

        let damage = Arc::new(DamageState::default());
        // The theme lives in the root reactive context (#43, specs §1.8); start on the built-in
        // light theme (#45). Writing this signal reskins every token; the swap trigger is #44.
        let theme = RwSignal::new(Arc::new(Theme::light()));
        provide_root_theme(theme, Arc::clone(&damage));
        let root = demo_root();

        Ok(WindowState {
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
            _owner: owner,
            theme,
            scheduler: Scheduler::new(),
            ime_enabled: false,
        })
    }
}

/// Map a winit IME event to kagari-text's abstract `ImeEvent`. `Enabled`/`Disabled`
/// are state transitions (handled by the caller), so they produce no `ImeEvent`. This
/// is the seam that keeps winit types out of kagari-text (design.md).
fn map_ime_event(ime: Ime) -> Option<ImeEvent> {
    match ime {
        Ime::Preedit(text, cursor) => Some(ImeEvent::Preedit { text, cursor }),
        Ime::Commit(text) => Some(ImeEvent::Commit(text)),
        Ime::Enabled | Ime::Disabled => None,
    }
}

/// Whether `key` is an IME-owned toggle/conversion key (henkan/muhenkan, hankaku/
/// zenkaku, kana, …). Such keys must never be consumed as an app shortcut — the OS IME
/// owns them — so the (future) keymap dispatches only when this is `false`. Gating is
/// unconditional (not on `ime_enabled`) because the on/off toggle keys are pressed
/// while the IME is *off* to turn it on; gating would re-introduce the Zed defects in
/// specs §5.2 (#40321 / #40592 / #40638 / #40300). Classified on the physical key,
/// which is independent of the current IME state.
fn ime_owns_key(key: PhysicalKey) -> bool {
    // winit's physical `KeyCode` (W3C UI Events `code`) names the Japanese IME keys as
    // Convert(変換) / NonConvert(無変換) / KanaMode, plus Lang1..Lang5
    // (kana / eisu / katakana / hiragana / **zenkaku-hankaku toggle**) and Hiragana/Katakana.
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

/// Route a key event. IME-owned toggle/conversion keys are passed through to the OS IME
/// and never consumed as an app shortcut (Zed §5.2: #40321/#40592/#40638/#40300); other
/// keys are where the app keymap will dispatch actions (none yet — Phase 3).
fn route_key_event(event: &KeyEvent) {
    if ime_owns_key(event.physical_key) {
        tracing::trace!(key = ?event.physical_key, "ime-owned key passed through");
    }
    // TODO(keymap): dispatch app actions for non-IME keys here once the keymap lands.
}

impl WindowState {
    /// Handle a winit IME event: track enable state, report the caret area on enable,
    /// and forward preedit/commit (as `ImeEvent`) to the text layer when enabled.
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
                // #25 routes this to the focused TextBuffer (preedit render / commit).
                // Log only the event shape, never the composed text: IME content is
                // user input (may include passwords) and must not land in logs.
                let (kind, text_len) = match &ev {
                    ImeEvent::Preedit { text, .. } => ("preedit", text.len()),
                    ImeEvent::Commit(text) => ("commit", text.len()),
                };
                tracing::debug!(kind, text_len, "ime event");
            }
        }
    }

    /// Report the IME candidate-window area to the OS. A default until #25 drives it
    /// from the real caret rect (the text/caret state does not exist yet).
    fn report_ime_caret_area(&self) {
        use winit::dpi::{LogicalPosition, LogicalSize};
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

        // Build the scene from the element tree: build-once + layout + paint (#34). Scene
        // coordinates are logical, so the layout viewport is the physical size over `scale`.
        let viewport = Size {
            w: self.config.width as f32 / scale,
            h: self.config.height as f32 / scale,
        };
        // Read the current theme from the reactive context for this frame (#43). Untracked: the
        // dedicated swap effect (`provide_root_theme`) is the sole subscriber that turns a swap
        // into damage, so the render path must not subscribe.
        let theme = self.theme.get_untracked();
        if let Err(e) = render_tree(
            &mut self.root,
            &mut self.arena,
            &mut self.layout,
            &mut self.text,
            Some(self.renderer.atlas_mut()),
            &mut self.scene,
            viewport,
            &self.damage,
            &theme,
        ) {
            tracing::error!(error = %e, "layout/paint failed");
            return;
        }

        // The renderer composites the scene into its offscreen linear target and
        // runs the output-transform pass into this swapchain frame.
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

async fn init_gpu(
    instance: &wgpu::Instance,
    surface: &wgpu::Surface<'static>,
    size: winit::dpi::PhysicalSize<u32>,
) -> Result<(wgpu::Device, wgpu::Queue, wgpu::SurfaceConfiguration), AppError> {
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

    let caps = surface.get_capabilities(&adapter);
    // Prefer an sRGB swapchain format so the HW performs the linear->sRGB encode (#10, Q2).
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

    let config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        width: size.width.max(1),
        height: size.height.max(1),
        present_mode: wgpu::PresentMode::Fifo,
        alpha_mode,
        view_formats: vec![],
        desired_maximum_frame_latency: 2,
    };
    Ok((device, queue, config))
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        match self.create_window_state(event_loop) {
            Ok(state) => {
                state.window.request_redraw();
                self.window = Some(state);
                tracing::info!("window created");
            }
            Err(e) => {
                // resumed() can't return Result; degrade by logging and exiting (specs §1.11).
                tracing::error!(error = %e, "failed to initialize window/GPU; exiting");
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(state) = self.window.as_mut() else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
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

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        let Some(state) = self.window.as_ref() else {
            return;
        };
        // Hybrid driving (#36, §1.6): request a redraw only when something changed (#35 damage)
        // or an active source is animating; otherwise the loop stays in `ControlFlow::Wait` and
        // the app is idle (no redraw, no GPU work). `request_redraw` is the gate — `redraw()`
        // itself always paints, so OS-expose / first-frame / resize repaints are unaffected.
        if should_redraw(
            state.damage.is_dirty(),
            state.scheduler.has_active_sources(),
        ) {
            state.window.request_redraw();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn winit_ime_should_map_to_ime_event() {
        // Preedit/Commit carry through; Enabled/Disabled are state-only (no ImeEvent).
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
        // IME toggle/conversion keys are owned by the OS IME — the app must not consume
        // them as shortcuts. These cover the Zed-defect scenarios (§5.2).
        for code in [
            KeyCode::Lang5,      // zenkaku/hankaku toggle (#40592/#40638/#40300)
            KeyCode::Convert,    // henkan
            KeyCode::NonConvert, // muhenkan (#40321)
            KeyCode::KanaMode,
            KeyCode::Katakana,
            KeyCode::Lang1, // kana
        ] {
            assert!(
                ime_owns_key(PhysicalKey::Code(code)),
                "{code:?} should be IME-owned"
            );
        }
        // Ordinary keys are not IME-owned, so the app keymap may bind them.
        assert!(!ime_owns_key(PhysicalKey::Code(KeyCode::KeyA)));
        assert!(!ime_owns_key(PhysicalKey::Code(KeyCode::Enter)));
    }

    #[test]
    fn root_theme_swap_should_flag_full_damage_and_update_context() {
        use crate::reactive::{ReadSignal, use_context};

        // Synchronous `ImmediateEffect`, so this is hang-free under the default harness; keep the
        // owner alive for the whole test since `set` stores only a Weak (RK-003/005).
        let owner = Owner::new();
        owner.set();

        let damage = Arc::new(DamageState::default());
        let theme = RwSignal::new(Arc::new(Theme::light()));
        provide_root_theme(theme, Arc::clone(&damage));

        // The effect runs once on creation → an initial full repaint is flagged.
        assert!(
            damage.is_dirty(),
            "wiring the theme flags an initial full repaint"
        );
        damage.clear();
        assert!(!damage.is_dirty(), "clear resets the full-damage flag");

        // A swap re-runs the effect synchronously → full damage again (reskin trigger).
        theme.set(Arc::new(Theme::dark()));
        assert!(damage.is_dirty(), "a theme swap flags full damage");

        // Descendants resolve the swapped theme through the read-only context handle.
        let ctx = use_context::<ReadSignal<Arc<Theme>>>().expect("theme provided in context");
        assert_eq!(
            ctx.get_untracked(),
            Arc::new(Theme::dark()),
            "the context reflects the swapped theme"
        );

        drop(owner);
    }
}
