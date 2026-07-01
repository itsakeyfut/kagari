//! The renderer: composites into an offscreen linear target and runs the
//! output-transform pass to the swapchain.

use std::sync::Arc;

use crate::assets::{AssetLoader, checkerboard};
use crate::atlas::{Atlas, AtlasCoord};
use crate::color::{OFFSCREEN_FORMAT, OffscreenTarget, OutputTransform};
use crate::error::RenderError;
use crate::polychrome::PolychromeRenderer;
use crate::quad::QuadRenderer;
use crate::scene::{Batch, PrimitiveKind, Scene};
use crate::shadow::ShadowRenderer;
use crate::sprite::SpriteRenderer;
use crate::svg::{IconId, icon_key, rasterize_icon};
use crate::underline::UnderlineRenderer;

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
    shadow: ShadowRenderer,
    quad: QuadRenderer,
    sprite: SpriteRenderer,
    polychrome: PolychromeRenderer,
    underline: UnderlineRenderer,
    atlas: Atlas,
    /// Group-1 bind group for the sprite pipeline (mono atlas array + sampler). Rebuilt
    /// when the atlas texture is re-created (device loss).
    atlas_bind: wgpu::BindGroup,
    rgba_atlas: Atlas,
    /// Group-1 bind group for the polychrome pipeline (RGBA atlas array + sampler).
    /// Rebuilt when the RGBA atlas texture is re-created (device loss).
    rgba_atlas_bind: wgpu::BindGroup,
    /// Image asset loader (#56): decodes off-thread and uploads into the RGBA atlas. Drained at the
    /// start of each frame. Holds atlas coords (not GPU handles), so device-loss needs no rebuild — the
    /// RGBA atlas re-uploads its cached tiles, keeping the coords valid.
    assets: AssetLoader,
    /// Reused across frames so the per-frame painter's-order merge does not allocate
    /// (filled by `Scene::batches_into`; perf.md).
    batches: Vec<Batch>,
}

/// Monochrome atlas geometry: 4 pre-allocated 1024² R8 layers (4 MiB). Dynamic layer
/// growth is post-MVP; see `atlas.rs`.
const ATLAS_LAYER_SIZE: u32 = 1024;
const ATLAS_MAX_LAYERS: u32 = 4;

/// RGBA image atlas geometry (#55): 4 pre-allocated 2048² Rgba8UnormSrgb layers (64 MiB) —
/// larger than the glyph atlas since UI images/icons exceed glyph sizes. Images larger than
/// a layer are skipped (the live-texture path is post-MVP). Tunable; growth is post-MVP.
const RGBA_ATLAS_LAYER_SIZE: u32 = 2048;
const RGBA_ATLAS_MAX_LAYERS: u32 = 4;

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
        let shadow = ShadowRenderer::new(&device, OFFSCREEN_FORMAT);
        let quad = QuadRenderer::new(&device, OFFSCREEN_FORMAT);
        let atlas = Atlas::new(
            device.clone(),
            queue.clone(),
            ATLAS_LAYER_SIZE,
            ATLAS_MAX_LAYERS,
            wgpu::TextureFormat::R8Unorm,
        );
        let mut rgba_atlas = Atlas::new(
            device.clone(),
            queue.clone(),
            RGBA_ATLAS_LAYER_SIZE,
            RGBA_ATLAS_MAX_LAYERS,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        );
        // Upload the shared "broken image" placeholder once; the asset loader resolves to it while a
        // tile is still loading or after a decode failure (#56).
        let ph = checkerboard();
        let (pw, ph_h) = (ph.width, ph.height);
        let placeholder =
            rgba_atlas.get_or_insert(AssetLoader::placeholder_key(), (pw, ph_h), || ph.rgba);
        // Prod spawner: decode on a detached background thread (runtime-agnostic — §7.2). The result is
        // delivered via the loader's channel and drained on the UI thread.
        let assets = AssetLoader::new(
            |job| {
                std::thread::spawn(job);
            },
            placeholder,
        );
        let sprite = SpriteRenderer::new(&device, OFFSCREEN_FORMAT);
        let polychrome = PolychromeRenderer::new(&device, OFFSCREEN_FORMAT);
        let underline = UnderlineRenderer::new(&device, OFFSCREEN_FORMAT);
        let atlas_bind = sprite.make_atlas_bind(&device, atlas.texture_view());
        let rgba_atlas_bind = polychrome.make_atlas_bind(&device, rgba_atlas.texture_view());
        Self {
            device,
            queue,
            target_format,
            size,
            offscreen,
            output,
            output_bind,
            shadow,
            quad,
            sprite,
            polychrome,
            underline,
            atlas,
            atlas_bind,
            rgba_atlas,
            rgba_atlas_bind,
            assets,
            batches: Vec::new(),
        }
    }

    /// The monochrome glyph/coverage atlas — `kagari-text` inserts rasterized glyphs
    /// here (#22) and the sprite pipeline samples it (#19).
    pub fn atlas_mut(&mut self) -> &mut Atlas {
        &mut self.atlas
    }

    /// The RGBA image atlas — the image/SVG loaders (#56/#57) insert decoded tiles here
    /// and the polychrome pipeline samples it (#55).
    pub fn rgba_atlas_mut(&mut self) -> &mut Atlas {
        &mut self.rgba_atlas
    }

    /// The image asset loader (#56) — the app calls `load` to decode an image off-thread; the renderer
    /// drains finished decodes into the RGBA atlas at the start of each `render`.
    pub fn assets_mut(&mut self) -> &mut AssetLoader {
        &mut self.assets
    }

    /// The atlas coord for a bundled icon (#57) rasterized at `px` (logical size × scale_factor).
    /// Glyph-style: rasterized synchronously into the RGBA atlas on a cache miss and cached by
    /// `(icon, px)`, so the icon is available the frame it is requested; a different `px` re-rasterizes
    /// crisply. The icon is monochrome white — tint it via the `PolychromeSprite::tint`. A rasterization
    /// failure (not expected for a bundled icon) degrades to a transparent tile, never panics.
    pub fn icon_coord(&mut self, icon: IconId, px: u32) -> AtlasCoord {
        let key = icon_key(icon, px);
        let coord = self.rgba_atlas.get_or_insert(key, (px, px), || {
            rasterize_icon(icon, px)
                .map(|d| d.rgba)
                .unwrap_or_else(|error| {
                    tracing::warn!(
                        ?error,
                        ?icon,
                        px,
                        "icon rasterization failed; using transparent tile"
                    );
                    vec![0u8; (px as usize) * (px as usize) * 4]
                })
        });
        // An icon larger than the atlas layer is skipped (degenerate coord) → it would draw nothing.
        // Icons are small so this is out of normal range, but surface it rather than fail silently (RK-017).
        if coord.max == [0.0, 0.0] {
            tracing::warn!(
                ?icon,
                px,
                "icon px exceeds the atlas layer; the icon will be blank"
            );
        }
        coord
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

        // Drain any image decodes that finished since the last frame into the RGBA atlas (#56), so a
        // freshly-loaded image is uploaded before this frame samples it. Disjoint field borrow.
        self.assets.drain(&mut self.rgba_atlas);

        // Sort + merge all primitive kinds into one painter's-order batch list (into
        // the reused buffer), then pack each kind's instances (in that order) so the
        // batch ranges line up.
        scene.batches_into(&mut self.batches);
        self.shadow
            .prepare(&self.device, &self.queue, scene, size, scale);
        self.quad
            .prepare(&self.device, &self.queue, scene, size, scale);
        self.sprite
            .prepare(&self.device, &self.queue, scene, size, scale);
        self.polychrome
            .prepare(&self.device, &self.queue, scene, size, scale);
        self.underline
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
                    PrimitiveKind::Shadow => self.shadow.draw(&mut pass, batch),
                    PrimitiveKind::Quad => self.quad.draw(&mut pass, batch),
                    PrimitiveKind::Image => {
                        self.polychrome
                            .draw(&mut pass, batch, &self.rgba_atlas_bind)
                    }
                    PrimitiveKind::Sprite => self.sprite.draw(&mut pass, batch, &self.atlas_bind),
                    PrimitiveKind::Underline => self.underline.draw(&mut pass, batch),
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
        self.shadow = ShadowRenderer::new(&self.device, OFFSCREEN_FORMAT);
        self.quad = QuadRenderer::new(&self.device, OFFSCREEN_FORMAT);
        self.sprite = SpriteRenderer::new(&self.device, OFFSCREEN_FORMAT);
        self.polychrome = PolychromeRenderer::new(&self.device, OFFSCREEN_FORMAT);
        self.underline = UnderlineRenderer::new(&self.device, OFFSCREEN_FORMAT);
        // Re-create each atlas texture and re-upload every cached tile from its CPU cache,
        // then rebuild each pipeline's atlas bind group against the new texture view.
        self.atlas.recreate(self.device.clone(), self.queue.clone());
        self.atlas_bind = self
            .sprite
            .make_atlas_bind(&self.device, self.atlas.texture_view());
        self.rgba_atlas
            .recreate(self.device.clone(), self.queue.clone());
        self.rgba_atlas_bind = self
            .polychrome
            .make_atlas_bind(&self.device, self.rgba_atlas.texture_view());
    }
}
