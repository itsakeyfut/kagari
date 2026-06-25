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
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    config: wgpu::SurfaceConfiguration,
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
        surface.configure(&device, &config);

        Ok(WindowState {
            window,
            surface,
            device: Arc::new(device),
            queue: Arc::new(queue),
            config,
        })
    }
}

impl WindowState {
    fn redraw(&self) {
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
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("kagari.clear.encoder"),
            });
        {
            // Solid clear; the renderer (offscreen + output transform) lands in #10.
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kagari.clear.pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.05,
                            g: 0.05,
                            b: 0.07,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }
        self.queue.submit(std::iter::once(encoder.finish()));
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
