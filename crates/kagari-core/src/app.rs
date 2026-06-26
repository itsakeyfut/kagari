//! Minimal app shell: a single winit window with wgpu init that clears and presents.
//!
//! This is the Phase-1 skeleton. The shared device lives on `App` (gpu.md §1); the
//! richer element/reactive API and multi-window support arrive in Phase 3 (specs §1.13).

use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowAttributes, WindowId};

use crate::error::AppError;

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
    // TEMP: a hand-built demo scene so the window shows quads. Replaced by the
    // core paint walk (which builds the scene from the element tree) in M3.
    scene: kagari_render::Scene,
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

        // The queue is moved into the renderer; the app shell keeps only `device`.
        let renderer = kagari_render::Renderer::new(
            device.clone(),
            queue,
            (config.width, config.height),
            config.format,
        );

        Ok(WindowState {
            window,
            surface,
            device,
            config,
            renderer,
            scene: demo_scene(),
        })
    }
}

/// TEMP: a two-quad demo scene (overlapping, different colors/order) so #12's Quad
/// pipeline is visible in the window. Replaced by the core paint walk in M3.
fn demo_scene() -> kagari_render::Scene {
    use kagari_base::{Color, Corners, Edges, Rect};
    use kagari_render::{Background, Border, Quad, RoundedRect, Scene};

    // #12 ignores the content mask (solid fill); a large mask is a no-op clip.
    let no_clip = RoundedRect {
        rect: Rect::from_xywh(0.0, 0.0, 1.0e4, 1.0e4),
        radii: Corners::default(),
    };
    let solid = |bounds: Rect, color: Color, order: u32| Quad {
        bounds,
        corner_radii: Corners::default(),
        bg: Background::Solid(color),
        border: Border {
            widths: Edges::default(),
            color: Color::TRANSPARENT,
        },
        content_mask: no_clip,
        order,
    };

    let mut scene = Scene::new();
    scene.quads.push(solid(
        Rect::from_xywh(40.0, 40.0, 240.0, 160.0),
        Color::from_srgb([0.20, 0.50, 0.90, 1.0]),
        0,
    ));
    // Semi-transparent red on top — exercises painter's order + premultiplied blend.
    scene.quads.push(solid(
        Rect::from_xywh(140.0, 120.0, 200.0, 140.0),
        Color::from_srgb([0.90, 0.30, 0.30, 0.8]),
        1,
    ));
    // A rounded, per-edge-bordered quad to exercise #13's SDF + border band + AA.
    // Asymmetric radii and border widths make a corner/edge mix-up visually obvious.
    scene.quads.push(Quad {
        bounds: Rect::from_xywh(360.0, 60.0, 220.0, 180.0),
        corner_radii: Corners {
            tl: 24.0,
            tr: 8.0,
            br: 24.0,
            bl: 8.0,
        },
        bg: Background::Solid(Color::from_srgb([0.15, 0.70, 0.45, 1.0])),
        border: Border {
            widths: Edges {
                top: 6.0,
                right: 2.0,
                bottom: 6.0,
                left: 12.0,
            },
            color: Color::from_srgb([0.95, 0.85, 0.20, 1.0]),
        },
        content_mask: no_clip,
        order: 2,
    });
    scene
}

impl WindowState {
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
        // The renderer composites the scene into its offscreen linear target and
        // runs the output-transform pass into this swapchain frame.
        let scale = self.window.scale_factor() as f32;
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
            _ => {}
        }
    }
}
