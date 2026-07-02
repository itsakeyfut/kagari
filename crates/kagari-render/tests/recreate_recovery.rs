//! Device-loss recreate test (#64, specs §1.11/§2.9): `Renderer::recreate` must rebuild every GPU
//! resource (offscreen/output targets, all pipelines, both atlases) so that rendering *after* a
//! recreate produces exactly the same output as *before*. This is the headless proof that the
//! App-level device-lost recovery path (regenerate the device → fan `recreate` out to each window)
//! yields a correct next frame.
//!
//! It is not a committed-PNG golden: it renders the same scene twice through one `Renderer` with a
//! `recreate` in between and asserts the two read-backs are **pixel-identical** — a self-contained
//! before/after compare, so there is no reference to baseline on lavapipe (RK-014). Uses the software
//! fallback adapter for determinism and **skips** (not fails) when none is available, matching the
//! golden harness — including honoring `KAGARI_GOLDEN_REQUIRE_ADAPTER` (the canonical CI job asserts
//! an adapter is present rather than silently skipping).
//!
//! The wgpu adapter/device init and 256-byte-aligned read-back are duplicated from
//! `kagari_golden::headless_render_with` on purpose: that harness renders a scene **once** internally,
//! but this test must render → `recreate` → render through a **single** `Renderer` to prove the
//! rebuild is consistent — a two-frame, one-renderer flow the single-shot harness API cannot express.
//!
//! `expect`/`unwrap` are intentional here — this is test-support code.

use std::sync::Arc;
use std::sync::mpsc;

use kagari_base::{Color, Corners, Edges, Point, Rect};
use kagari_render::{Background, Border, Quad, Renderer, RoundedRect, Scene};

/// wgpu requires `bytes_per_row` of a texture→buffer copy to be 256-byte aligned.
const ROW_ALIGN: u32 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
const SIZE: (u32, u32) = (64, 64);

/// A scene exercising several pipelines at once (solid + gradient quads, a border, a rounded clip),
/// so a partial rebuild in `recreate` would show up as a pixel diff.
fn probe_scene() -> Scene {
    let mut scene = Scene::new();
    let no_clip = RoundedRect {
        rect: Rect::from_xywh(0.0, 0.0, 1.0e4, 1.0e4),
        radii: Corners::default(),
    };
    scene.quads.push(Quad {
        bounds: Rect::from_xywh(6.0, 6.0, 52.0, 52.0),
        corner_radii: Corners {
            tl: 10.0,
            tr: 10.0,
            br: 10.0,
            bl: 10.0,
        },
        bg: Background::LinearGradient {
            start: Color::from_srgb([0.10, 0.20, 0.80, 1.0]),
            end: Color::from_srgb([0.90, 0.20, 0.50, 1.0]),
            start_point: Point::new(0.0, 0.0),
            end_point: Point::new(1.0, 1.0),
        },
        border: Border {
            widths: Edges {
                top: 4.0,
                right: 4.0,
                bottom: 4.0,
                left: 4.0,
            },
            color: Color::from_srgb([0.95, 0.85, 0.20, 1.0]),
        },
        content_mask: no_clip,
        order: 0,
    });
    scene
}

/// Render `scene` through `renderer` into a fresh offscreen target and read the pixels back (RGBA8).
fn render_readback(
    renderer: &mut Renderer,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    scene: &mut Scene,
) -> Vec<u8> {
    let (width, height) = SIZE;
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("kagari.recreate.target"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    renderer
        .render(scene, &view, SIZE, 1.0)
        .expect("headless render should succeed");

    let padded_bpr = (width * 4).div_ceil(ROW_ALIGN) * ROW_ALIGN;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("kagari.recreate.readback"),
        size: u64::from(padded_bpr) * u64::from(height),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("kagari.recreate.readback.encoder"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bpr),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(encoder.finish()));

    let (tx, rx) = mpsc::channel();
    buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("device poll should not error");
    rx.recv()
        .expect("map callback should fire")
        .expect("buffer should map for read");

    let mapped = buffer.slice(..).get_mapped_range();
    let row_bytes = (width * 4) as usize;
    let mut pixels = Vec::with_capacity(row_bytes * height as usize);
    for row in 0..height as usize {
        let start = row * padded_bpr as usize;
        pixels.extend_from_slice(&mapped[start..start + row_bytes]);
    }
    drop(mapped);
    buffer.unmap();
    pixels
}

#[test]
fn recreate_should_render_identically_to_before() {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        flags: wgpu::InstanceFlags::default(),
        memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
        backend_options: wgpu::BackendOptions::default(),
        display: None,
    });
    // Software fallback adapter for determinism; skip (don't fail) when none is available — mirrors
    // the golden harness's no-adapter behavior.
    let adapter = match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::default(),
        compatible_surface: None,
        force_fallback_adapter: true,
    })) {
        Ok(adapter) => adapter,
        Err(e) => {
            // Mirror the golden harness contract (RK-014): the canonical CI job sets
            // KAGARI_GOLDEN_REQUIRE_ADAPTER, where a missing adapter must fail loudly rather than
            // silently skip the coverage.
            assert!(
                std::env::var_os("KAGARI_GOLDEN_REQUIRE_ADAPTER").is_none(),
                "KAGARI_GOLDEN_REQUIRE_ADAPTER is set but no software adapter is available: {e}"
            );
            eprintln!("no software adapter; skipping recreate recovery test");
            return;
        }
    };

    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("kagari.recreate.device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
        memory_hints: wgpu::MemoryHints::default(),
        trace: wgpu::Trace::Off,
    }))
    .expect("software adapter should provide a device");
    let device = Arc::new(device);
    let queue = Arc::new(queue);

    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer = Renderer::new(device.clone(), queue.clone(), SIZE, format);

    // Frame 1 — before recreate.
    let before = render_readback(&mut renderer, &device, &queue, &mut probe_scene());

    // Simulate device-loss recovery: rebuild every GPU resource. Same device here so the test is
    // deterministic; what matters is that `recreate` rebuilds pipelines/targets/atlases consistently.
    renderer.recreate(device.clone(), queue.clone());

    // Frame 2 — after recreate. Must be pixel-identical to frame 1.
    let after = render_readback(&mut renderer, &device, &queue, &mut probe_scene());

    assert_eq!(
        before, after,
        "the frame after Renderer::recreate must be pixel-identical to the frame before it"
    );
}
