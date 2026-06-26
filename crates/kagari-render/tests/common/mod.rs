//! Headless render + golden-image test harness (#16).
//!
//! Renders a `Scene` with no window/surface, reads the pixels back, and compares
//! them to a committed reference PNG within a per-channel tolerance. Determinism is
//! via a software/reference adapter (`force_fallback_adapter`) + the tolerance, not
//! pixel-exact matching across GPUs (specs §8.1, test.md §3).
//!
//! **References are canonical to one software rasterizer.** Antialiased (SDF
//! coverage) edges and the `Rgba16Float`→`Rgba8UnormSrgb` rounding differ between
//! rasterizers (e.g. DX12 WARP vs Vulkan lavapipe) by more than the `≤ 2` tolerance,
//! so a reference generated on one will not match another. The committed PNGs are
//! generated locally on DX12 WARP today; the CI job (#123) establishes lavapipe as
//! the canonical rasterizer and re-baselines (`UPDATE_GOLDEN=1`) there.
//!
//! `expect`/`unwrap` are intentional here — this is test-only code.

// Shared test-support module: each integration-test binary uses a different subset of
// these helpers, so unused-in-one-binary items are expected (not dead code).
#![allow(dead_code)]

use std::sync::Arc;
use std::sync::mpsc;

use image::RgbaImage;
use kagari_render::{Renderer, Scene};

/// wgpu requires `bytes_per_row` of a texture→buffer copy to be 256-byte aligned.
const ROW_ALIGN: u32 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;

/// A headless render result: the read-back image plus whether it was produced on the
/// canonical golden rasterizer (Mesa lavapipe / "llvmpipe").
pub struct Rendered {
    pub image: RgbaImage,
    /// `true` only on Mesa lavapipe. Goldens are committed from lavapipe, and the
    /// f16-offscreen + sRGB-encode path is not bit-identical across software
    /// rasterizers (DX12 WARP differs by ~15/255 even for a flat fill), so a pixel
    /// comparison is only meaningful here — callers render-only on other adapters.
    pub canonical: bool,
}

/// Render `scene` headlessly into an `Rgba8UnormSrgb` offscreen target and read it
/// back. Returns `None` when no software adapter is available (e.g. a GPU-less CI
/// without lavapipe), so callers can skip rather than fail.
pub fn headless_render(scene: &mut Scene, size: (u32, u32), scale: f32) -> Option<Rendered> {
    let (width, height) = size;

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        flags: wgpu::InstanceFlags::default(),
        memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
        backend_options: wgpu::BackendOptions::default(),
        display: None,
    });

    // A software/reference adapter (DX12 WARP / Vulkan lavapipe) for determinism.
    // No adapter ⇒ return `None` so the caller skips — unless the canonical
    // software-adapter CI job (#123) sets `KAGARI_GOLDEN_REQUIRE_ADAPTER`, where a
    // missing adapter must fail loudly rather than silently skip the goldens.
    let adapter = match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::default(),
        compatible_surface: None,
        force_fallback_adapter: true,
    })) {
        Ok(adapter) => adapter,
        Err(e) => {
            assert!(
                std::env::var_os("KAGARI_GOLDEN_REQUIRE_ADAPTER").is_none(),
                "KAGARI_GOLDEN_REQUIRE_ADAPTER is set but no software adapter is available: {e}"
            );
            return None;
        }
    };

    // Goldens are canonical to Mesa lavapipe ("llvmpipe"); see `Rendered::canonical`.
    let canonical = adapter.get_info().name.to_lowercase().contains("llvmpipe");

    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("kagari.headless.device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
        memory_hints: wgpu::MemoryHints::default(),
        trace: wgpu::Trace::Off,
    }))
    .expect("software adapter should provide a device");
    let device = Arc::new(device);
    let queue = Arc::new(queue);

    // Match what the window presents: an sRGB swapchain format, so the HW does the
    // linear→sRGB encode in the output-transform pass.
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer = Renderer::new(device.clone(), queue.clone(), size, format);

    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("kagari.headless.target"),
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
        .render(scene, &view, size, scale)
        .expect("headless render should succeed");

    // Texture → buffer copy with a 256-byte-aligned row stride, then de-pad on read.
    let padded_bpr = (width * 4).div_ceil(ROW_ALIGN) * ROW_ALIGN;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("kagari.headless.readback"),
        size: u64::from(padded_bpr) * u64::from(height),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("kagari.headless.readback.encoder"),
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

    // Block on the map: map_async fires the callback once the device is polled.
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

    let image =
        RgbaImage::from_raw(width, height, pixels).expect("readback pixels should fill the image");
    Some(Rendered { image, canonical })
}

/// Compare `img` to the committed reference `tests/golden/{name}.png` within a
/// per-channel tolerance of `≤ 2`. `UPDATE_GOLDEN=1` regenerates the reference; on a
/// mismatch the actual image is written to `target/{name}-actual.png` for inspection.
pub fn compare_golden(name: &str, img: &RgbaImage) {
    let golden_path = format!("{}/tests/golden/{name}.png", env!("CARGO_MANIFEST_DIR"));

    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        if let Some(parent) = std::path::Path::new(&golden_path).parent() {
            std::fs::create_dir_all(parent).expect("golden dir should be creatable");
        }
        img.save(&golden_path).expect("golden should save");
        eprintln!("UPDATE_GOLDEN: wrote reference {golden_path}");
        return;
    }

    let reference = image::open(&golden_path)
        .unwrap_or_else(|e| {
            panic!("golden {golden_path} should open (UPDATE_GOLDEN=1 to create): {e}")
        })
        .to_rgba8();
    assert_eq!(
        reference.dimensions(),
        img.dimensions(),
        "golden {name}: dimension mismatch"
    );

    let max_delta = reference
        .as_raw()
        .iter()
        .zip(img.as_raw())
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap_or(0);
    if max_delta > 2 {
        // Write under the crate's `target/` (gitignored via `**/target/`) for CI to
        // upload. The dir is created first: a workspace build emits to the root
        // `/target`, so `crates/kagari-render/target/` does not otherwise exist.
        let actual_path = format!("{}/target/{name}-actual.png", env!("CARGO_MANIFEST_DIR"));
        if let Some(parent) = std::path::Path::new(&actual_path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = img.save(&actual_path);
        panic!(
            "golden {name}: max per-channel delta {max_delta} > 2 (actual written to {actual_path})"
        );
    }
}

/// Render `scene` headlessly and assert it matches the committed golden `name`,
/// bundling the two skip-guards used by every golden test: no software adapter →
/// skip; non-canonical rasterizer (e.g. local DX12 WARP) → render-only/skip-compare
/// (see [`Rendered::canonical`]). The pixel comparison runs only on lavapipe (CI).
pub fn assert_scene_golden(name: &str, scene: &mut Scene, size: (u32, u32), scale: f32) {
    let Some(rendered) = headless_render(scene, size, scale) else {
        eprintln!("skipping golden '{name}': no software adapter available");
        return;
    };
    if !rendered.canonical {
        eprintln!(
            "skipping golden compare '{name}': non-canonical rasterizer (goldens are lavapipe-canonical)"
        );
        return;
    }
    compare_golden(name, &rendered.image);
}

/// Assert a rendered image matches the committed golden `name` (per-channel `≤ 2`).
/// Thin wrapper over [`compare_golden`] matching the documented `assert_golden!`
/// API; obtain `img` from [`headless_render`] (which signals skip via `None`).
#[macro_export]
macro_rules! assert_golden {
    ($name:expr, $img:expr) => {
        $crate::common::compare_golden($name, &$img)
    };
}
