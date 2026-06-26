//! The renderer: composites into an offscreen linear target and runs the
//! output-transform pass to the swapchain.

use std::sync::Arc;

use crate::atlas::Atlas;
use crate::color::{OFFSCREEN_FORMAT, OffscreenTarget, OutputTransform};
use crate::error::RenderError;
use crate::quad::QuadRenderer;
use crate::scene::{Batch, PrimitiveKind, Scene};
use crate::sprite::SpriteRenderer;

/// Owns the GPU resources for one window's rendering. The device/queue are shared
/// from the app shell (gpu.md §1). All resources are reconstructable from
/// descriptors for device-loss recovery (§2.9).
pub struct Renderer {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    target_format: wgpu::TextureFormat,
    size: (u32, u32),
    offscreen: OffscreenTarget,
    output: OutputTransform,
    output_bind: wgpu::BindGroup,
    quad: QuadRenderer,
    sprite: SpriteRenderer,
    atlas: Atlas,
    /// Group-1 bind group for the sprite pipeline (atlas array + sampler). Rebuilt
    /// when the atlas texture is re-created (device loss).
    atlas_bind: wgpu::BindGroup,
    /// Reused across frames so the per-frame painter's-order merge does not allocate
    /// (filled by `Scene::batches_into`; perf.md).
    batches: Vec<Batch>,
}

/// Monochrome atlas geometry: 4 pre-allocated 1024² R8 layers (4 MiB). Dynamic layer
/// growth is post-MVP; see `atlas.rs`.
const ATLAS_LAYER_SIZE: u32 = 1024;
const ATLAS_MAX_LAYERS: u32 = 4;

impl Renderer {
    /// `target_format` is the swapchain format (an sRGB format in MVP); the output
    /// pass renders to it and the HW performs the linear→sRGB encode.
    pub fn new(
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
        size: (u32, u32),
        target_format: wgpu::TextureFormat,
    ) -> Self {
        let offscreen = OffscreenTarget::new(&device, size);
        let output = OutputTransform::new(&device, target_format);
        let output_bind = output.bind(&device, &offscreen.view);
        let quad = QuadRenderer::new(&device, OFFSCREEN_FORMAT);
        let atlas = Atlas::new(
            device.clone(),
            queue.clone(),
            ATLAS_LAYER_SIZE,
            ATLAS_MAX_LAYERS,
        );
        let sprite = SpriteRenderer::new(&device, OFFSCREEN_FORMAT);
        let atlas_bind = sprite.make_atlas_bind(&device, atlas.texture_view());
        Self {
            device,
            queue,
            target_format,
            size,
            offscreen,
            output,
            output_bind,
            quad,
            sprite,
            atlas,
            atlas_bind,
            batches: Vec::new(),
        }
    }

    /// The monochrome glyph/coverage atlas — `kagari-text` inserts rasterized glyphs
    /// here (#22) and the sprite pipeline samples it (#19).
    pub fn atlas_mut(&mut self) -> &mut Atlas {
        &mut self.atlas
    }

    /// Render one frame: draw the scene's quads into the offscreen linear target,
    /// then output-transform it into `target_view`. `scale` is the logical→physical
    /// pixel ratio (px coordinates in the scene are logical).
    pub fn render(
        &mut self,
        scene: &mut Scene,
        target_view: &wgpu::TextureView,
        size: (u32, u32),
        scale: f32,
    ) -> Result<(), RenderError> {
        if size != self.size {
            self.size = size;
            self.offscreen = OffscreenTarget::new(&self.device, size);
            self.output_bind = self.output.bind(&self.device, &self.offscreen.view);
        }

        // Sort + merge all primitive kinds into one painter's-order batch list (into
        // the reused buffer), then pack each kind's instances (in that order) so the
        // batch ranges line up.
        scene.batches_into(&mut self.batches);
        self.quad
            .prepare(&self.device, &self.queue, scene, size, scale);
        self.sprite
            .prepare(&self.device, &self.queue, scene, size, scale);

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("kagari.frame.encoder"),
            });

        // Pass 1: clear the offscreen linear target, then draw the primitive batches
        // (painter's order). The clear color is a linear value.
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kagari.offscreen.pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.offscreen.view,
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
            for batch in &self.batches {
                match batch.kind {
                    PrimitiveKind::Quad => self.quad.draw(&mut pass, batch),
                    PrimitiveKind::Sprite => self.sprite.draw(&mut pass, batch, &self.atlas_bind),
                }
            }
        }

        // Pass 2: output transform — sample offscreen (linear) into the swapchain
        // (sRGB; HW encodes). No tone map (SDR).
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kagari.output.pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.output.draw(&mut pass, &self.output_bind);
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        Ok(())
    }

    /// Rebuild all GPU resources from descriptors after device loss (§2.9).
    pub fn recreate(&mut self, device: Arc<wgpu::Device>, queue: Arc<wgpu::Queue>) {
        self.device = device;
        self.queue = queue;
        self.offscreen = OffscreenTarget::new(&self.device, self.size);
        self.output = OutputTransform::new(&self.device, self.target_format);
        self.output_bind = self.output.bind(&self.device, &self.offscreen.view);
        self.quad = QuadRenderer::new(&self.device, OFFSCREEN_FORMAT);
        self.sprite = SpriteRenderer::new(&self.device, OFFSCREEN_FORMAT);
        // Re-create the atlas texture and re-upload every cached tile from its CPU cache,
        // then rebuild the sprite's atlas bind group against the new texture view.
        self.atlas.recreate(self.device.clone(), self.queue.clone());
        self.atlas_bind = self
            .sprite
            .make_atlas_bind(&self.device, self.atlas.texture_view());
    }
}
