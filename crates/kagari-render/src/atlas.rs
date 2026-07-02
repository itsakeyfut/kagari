//! Multi-page texel-format-parameterized atlas for glyphs/coverage tiles (R8) and
//! colored images (RGBA, #55) (specs §2.6).
//!
//! Pages are layers of one `texture_2d_array` (a single bind group serves all pages,
//! so sprites of any page batch into one draw — #19). The `Atlas` owns the
//! `key → AtlasCoord` cache *and* the LRU together, so a consumer's coord can never
//! outlive its slot (decision Q2): callers use `get_or_insert(key, size, rasterize)`.
//!
//! The texel format is chosen at construction (`R8Unorm` for the mono glyph atlas,
//! `Rgba8UnormSrgb` for the image atlas); packing and coord math are in pixels and
//! format-agnostic, while upload scales by the format's bytes-per-texel.
//!
//! Tiles are packed with a 1px transparent **gutter** so that linear sampling
//! (decision Q4) at a glyph's edge blends toward 0 coverage, not into a neighbor.
//!
//! The packing/cache/LRU core (`Packer`) is GPU-free and unit-tested directly; only
//! the texture upload touches wgpu. A CPU-side bitmap cache re-uploads every tile on
//! device loss (`recreate`, specs §2.9).
//!
//! MVP: the array is created once with a fixed `max_layers` (a `texture_2d_array`
//! cannot be resized), so "overflow → add page" means *use the next pre-allocated
//! layer*; when all layers are full the least-recently-used tiles are evicted. The
//! atlas therefore never reports `RenderError::AtlasFull` — it evicts instead.
//! Dynamic layer growth is post-MVP.

use std::collections::HashMap;
use std::sync::Arc;

use etagere::{AllocId, AtlasAllocator, size2};

/// Transparent border kept around each tile so linear filtering never samples a
/// neighbor's coverage at a glyph edge.
const GUTTER: u32 = 1;

/// A tile's location in the atlas: which layer (`page`) and its normalized uv rect.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AtlasCoord {
    pub page: u32,
    pub min: [f32; 2],
    pub max: [f32; 2],
}

/// Pixel placement of a tile: the padded box (`p*`, uploaded with a zero gutter) and
/// the inner glyph size (`w`,`h`). `w == 0` marks a degenerate/skipped tile.
#[derive(Clone, Copy)]
struct TileRect {
    page: u32,
    px: u32,
    py: u32,
    pw: u32,
    ph: u32,
    w: u32,
    h: u32,
}

/// Where a freshly inserted tile landed (so the `Atlas` can upload its pixels).
struct Placement {
    coord: AtlasCoord,
    rect: TileRect,
}

impl Placement {
    /// A skipped insert (invalid/oversize tile): no upload, degenerate coord.
    const SKIPPED: Placement = Placement {
        coord: AtlasCoord {
            page: 0,
            min: [0.0, 0.0],
            max: [0.0, 0.0],
        },
        rect: TileRect {
            page: 0,
            px: 0,
            py: 0,
            pw: 0,
            ph: 0,
            w: 0,
            h: 0,
        },
    };
}

/// A live tile: its placement, etagere allocation, last-use tick, and coord.
struct Slot {
    rect: TileRect,
    alloc: AllocId,
    lru: u64,
    coord: AtlasCoord,
}

/// GPU-free packing + cache + LRU core. One etagere allocator per active layer, a
/// `key → Slot` cache, and a `key → bitmap` CPU cache for device-loss re-upload.
struct Packer {
    layers: Vec<AtlasAllocator>,
    cache: HashMap<u64, Slot>,
    cpu: HashMap<u64, Vec<u8>>,
    lru: u64,
    layer_size: u32,
    max_layers: u32,
    /// Bytes per texel of the atlas format (R8 = 1, RGBA8 = 4). The CPU bitmap a tile
    /// carries is `w*h*bytes_per_texel` bytes; packing itself is in pixels.
    bytes_per_texel: usize,
}

impl Packer {
    fn new(layer_size: u32, max_layers: u32, bytes_per_texel: usize) -> Self {
        Self {
            layers: Vec::new(),
            cache: HashMap::new(),
            cpu: HashMap::new(),
            lru: 0,
            layer_size,
            max_layers,
            bytes_per_texel,
        }
    }

    fn tick(&mut self) -> u64 {
        self.lru += 1;
        self.lru
    }

    /// Occupancy snapshot for diagnostics (#69): packed area (summed across active layers) over the
    /// full texture-array capacity, plus live tile + page counts.
    fn usage(&self) -> AtlasUsage {
        let used_area: u64 = self
            .layers
            .iter()
            .map(|l| l.allocated_space().max(0) as u64)
            .sum();
        let total_area =
            u64::from(self.max_layers) * u64::from(self.layer_size) * u64::from(self.layer_size);
        AtlasUsage {
            used_area,
            total_area,
            tiles: self.cache.len(),
            pages: self.layers.len() as u32,
            max_pages: self.max_layers,
        }
    }

    /// The largest glyph extent that fits a layer once the gutter is added.
    fn inner_max(&self) -> u32 {
        self.layer_size.saturating_sub(2 * GUTTER)
    }

    /// Cache hit → the coord (and bump its LRU). Miss → `None`.
    fn get(&mut self, key: u64) -> Option<AtlasCoord> {
        let tick = self.tick();
        let slot = self.cache.get_mut(&key)?;
        slot.lru = tick;
        Some(slot.coord)
    }

    /// Try to allocate a `pw×ph` padded box in an existing layer, or activate the
    /// next pre-allocated layer if there is room. Does not evict.
    fn try_allocate(&mut self, size: etagere::Size) -> Option<(u32, etagere::Allocation)> {
        for page in 0..self.layers.len() {
            if let Some(a) = self.layers[page].allocate(size) {
                return Some((page as u32, a));
            }
        }
        if (self.layers.len() as u32) < self.max_layers {
            let mut layer =
                AtlasAllocator::new(size2(self.layer_size as i32, self.layer_size as i32));
            let a = layer.allocate(size)?;
            let page = self.layers.len() as u32;
            self.layers.push(layer);
            return Some((page, a));
        }
        None
    }

    fn lru_victim(&self) -> Option<u64> {
        self.cache
            .iter()
            .min_by_key(|(_, s)| s.lru)
            .map(|(k, _)| *k)
    }

    fn evict(&mut self, key: u64) {
        if let Some(slot) = self.cache.remove(&key) {
            self.layers[slot.rect.page as usize].deallocate(slot.alloc);
            self.cpu.remove(&key);
        }
    }

    /// Insert a tile (the `bitmap` is `w*h` R8 bytes), evicting LRU entries when all
    /// layers are full. Returns where it landed plus the keys evicted to make room.
    /// Invalid (zero-sized) or oversize tiles are skipped (`Placement::SKIPPED`).
    fn insert(&mut self, key: u64, w: u32, h: u32, bitmap: Vec<u8>) -> (Placement, Vec<u64>) {
        let inner_max = self.inner_max();
        if w == 0 || h == 0 || w > inner_max || h > inner_max {
            // A runtime input we cannot atlas at this layer size (pathological for real
            // glyphs) — skip gracefully rather than fail the frame.
            tracing::warn!(w, h, inner_max, "atlas: invalid/oversize tile; skipping");
            return (Placement::SKIPPED, Vec::new());
        }
        debug_assert_eq!(
            bitmap.len(),
            (w * h) as usize * self.bytes_per_texel,
            "atlas tile bitmap must be w*h*bytes_per_texel bytes"
        );

        let (pw, ph) = (w + 2 * GUTTER, h + 2 * GUTTER);
        let psize = size2(pw as i32, ph as i32);

        let mut evicted = Vec::new();
        let (page, alloc) = loop {
            if let Some(found) = self.try_allocate(psize) {
                break found;
            }
            // All layers full → evict the least-recently-used tile and retry. When the
            // cache is empty every layer is empty and `psize <= layer_size`, so this
            // always converges; the `else` is unreachable but kept panic-free.
            let Some(victim) = self.lru_victim() else {
                tracing::error!(w, h, "atlas: tile did not fit even when empty; skipping");
                return (Placement::SKIPPED, evicted);
            };
            self.evict(victim);
            evicted.push(victim);
        };

        // Place the glyph inside the gutter; uv covers only the inner w×h.
        let r = alloc.rectangle;
        let (px, py) = (r.min.x as u32, r.min.y as u32);
        let (ix, iy) = (px + GUTTER, py + GUTTER);
        let ls = self.layer_size as f32;
        let coord = AtlasCoord {
            page,
            min: [ix as f32 / ls, iy as f32 / ls],
            max: [(ix + w) as f32 / ls, (iy + h) as f32 / ls],
        };
        let rect = TileRect {
            page,
            px,
            py,
            pw,
            ph,
            w,
            h,
        };
        let lru = self.tick();
        self.cache.insert(
            key,
            Slot {
                rect,
                alloc: alloc.id,
                lru,
                coord,
            },
        );
        self.cpu.insert(key, bitmap);
        (Placement { coord, rect }, evicted)
    }
}

/// A snapshot of atlas occupancy, for the debug overlay (#69). `used_area`/`total_area` are in
/// texels² (packed area across active layers vs the whole texture-array capacity); `tiles` is the
/// number of live cached tiles, `pages`/`max_pages` the active vs maximum layers.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AtlasUsage {
    pub used_area: u64,
    pub total_area: u64,
    pub tiles: usize,
    pub pages: u32,
    pub max_pages: u32,
}

impl AtlasUsage {
    /// The occupied fraction of the atlas's total capacity, in `0.0..=1.0` (0 when empty).
    pub fn fraction(&self) -> f32 {
        if self.total_area == 0 {
            0.0
        } else {
            self.used_area as f32 / self.total_area as f32
        }
    }
}

/// The GPU atlas: the `Packer` plus the `texture_2d_array` it packs into. The texel
/// format is set at construction (`R8Unorm` mono glyphs / `Rgba8UnormSrgb` images).
pub struct Atlas {
    packer: Packer,
    queue: Arc<wgpu::Queue>,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    format: wgpu::TextureFormat,
    /// Bytes per texel of `format` (the upload's row stride / padded-box size scale).
    bytes_per_texel: usize,
}

impl Atlas {
    pub fn new(
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
        layer_size: u32,
        max_layers: u32,
        format: wgpu::TextureFormat,
    ) -> Self {
        // INVARIANT: the atlas formats we use (R8Unorm / Rgba8UnormSrgb) are
        // uncompressed single-plane, so `block_copy_size(None)` is always `Some`.
        let bytes_per_texel = format
            .block_copy_size(None)
            .expect("atlas format must be uncompressed single-plane")
            as usize;
        let (texture, view) = Self::create_texture(&device, layer_size, max_layers, format);
        Self {
            packer: Packer::new(layer_size, max_layers, bytes_per_texel),
            queue,
            texture,
            view,
            format,
            bytes_per_texel,
        }
    }

    fn create_texture(
        device: &wgpu::Device,
        layer_size: u32,
        max_layers: u32,
        format: wgpu::TextureFormat,
    ) -> (wgpu::Texture, wgpu::TextureView) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("kagari.atlas"),
            size: wgpu::Extent3d {
                width: layer_size,
                height: layer_size,
                depth_or_array_layers: max_layers,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("kagari.atlas.view"),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        (texture, view)
    }

    /// Cache hit → the cached coord (no re-raster). Miss → run `rasterize` to get the
    /// tile bitmap (`w*h*bytes_per_texel` bytes, row-major — R8 coverage for the mono
    /// atlas, RGBA for the image atlas), pack it (evicting LRU when full), upload it to
    /// its layer, and return the coord.
    pub fn get_or_insert(
        &mut self,
        key: u64,
        size: (u32, u32),
        rasterize: impl FnOnce() -> Vec<u8>,
    ) -> AtlasCoord {
        if let Some(coord) = self.packer.get(key) {
            return coord;
        }
        let (w, h) = size;
        let (placement, _evicted) = self.packer.insert(key, w, h, rasterize());
        // Upload from the CPU-cached copy (no extra clone). Skipped tiles have `w == 0`.
        if placement.rect.w != 0 {
            if let Some(bitmap) = self.packer.cpu.get(&key) {
                self.upload(&placement.rect, bitmap);
            }
        }
        placement.coord
    }

    /// Upload a tile's pixels into its padded box, zeroing the gutter so linear
    /// sampling at the tile edge blends toward 0 (not a neighbor). `bitmap` is the
    /// inner `w*h` tile (`bytes_per_texel` bytes per texel), centered in the `pw*ph` box.
    fn upload(&self, rect: &TileRect, bitmap: &[u8]) {
        let bpp = self.bytes_per_texel;
        let padded = pad_tile(bitmap, rect, bpp);
        // write_texture takes a tight `bytes_per_row` (no 256 alignment, unlike a
        // buffer copy): `pw` texels × `bpp` bytes each.
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: rect.px,
                    y: rect.py,
                    z: rect.page,
                },
                aspect: wgpu::TextureAspect::All,
            },
            &padded,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(rect.pw * bpp as u32),
                rows_per_image: Some(rect.ph),
            },
            wgpu::Extent3d {
                width: rect.pw,
                height: rect.ph,
                depth_or_array_layers: 1,
            },
        );
    }

    /// The atlas array view, for the sprite pipeline's bind group (#19).
    pub fn texture_view(&self) -> &wgpu::TextureView {
        &self.view
    }

    /// An occupancy snapshot for the debug overlay (#69): packed area, tile count, and page usage.
    pub fn usage(&self) -> AtlasUsage {
        self.packer.usage()
    }

    /// Rebuild the GPU texture and re-upload every cached tile from the CPU bitmap
    /// cache (device-loss recovery, specs §2.9). The packing/cache state is retained.
    pub fn recreate(&mut self, device: Arc<wgpu::Device>, queue: Arc<wgpu::Queue>) {
        let (texture, view) = Self::create_texture(
            &device,
            self.packer.layer_size,
            self.packer.max_layers,
            self.format,
        );
        self.queue = queue;
        self.texture = texture;
        self.view = view;
        for (key, slot) in &self.packer.cache {
            if let Some(bitmap) = self.packer.cpu.get(key) {
                self.upload(&slot.rect, bitmap);
            }
        }
    }
}

/// Center a tile's `w*h` texels (`bpp` bytes each) inside its `pw*ph` padded box with a
/// zeroed gutter, row-major. GPU-free (the upload's CPU half) so it is unit-testable for
/// any `bpp`. The gutter stays 0 so linear sampling at the tile edge blends toward a
/// transparent/zero texel (R8 ⇒ 0 coverage; RGBA ⇒ transparent black), not a neighbor.
fn pad_tile(bitmap: &[u8], rect: &TileRect, bpp: usize) -> Vec<u8> {
    let (w, pw, ph) = (rect.w as usize, rect.pw as usize, rect.ph as usize);
    let g = GUTTER as usize;
    let row_bytes = w * bpp;
    let pw_bytes = pw * bpp;
    let mut padded = vec![0u8; pw_bytes * ph];
    for row in 0..rect.h as usize {
        let src = &bitmap[row * row_bytes..row * row_bytes + row_bytes];
        let start = (row + g) * pw_bytes + g * bpp;
        padded[start..start + row_bytes].copy_from_slice(src);
    }
    padded
}

#[cfg(test)]
mod tests {
    use super::*;

    // The packing/cache/LRU core is GPU-free, so these run without a device. A small
    // 16px layer with 2 layers (inner max 14 after the 1px gutter) drives overflow.
    // Mono (R8): one byte per texel.
    fn packer() -> Packer {
        Packer::new(16, 2, 1)
    }

    #[test]
    fn atlas_should_reuse_coord_for_same_key() {
        let mut p = packer();
        let (first, _) = p.insert(1, 8, 8, vec![0u8; 64]);
        let again = p.get(1).expect("key 1 should be cached");
        assert_eq!(first.coord, again);
        assert_eq!(p.cache.len(), 1, "no second allocation");
    }

    #[test]
    fn atlas_should_add_page_on_overflow() {
        let mut p = packer();
        // A 14×14 glyph + 1px gutter = a 16×16 padded box that fills the whole layer,
        // so the second tile must land on the next page.
        let (a, _) = p.insert(1, 14, 14, vec![0u8; 196]);
        let (b, _) = p.insert(2, 14, 14, vec![0u8; 196]);
        assert_eq!(a.coord.page, 0);
        assert_eq!(b.coord.page, 1);
    }

    #[test]
    fn atlas_should_evict_lru_when_full() {
        let mut p = packer();
        let _ = p.insert(1, 14, 14, vec![0u8; 196]); // fills layer 0
        let _ = p.insert(2, 14, 14, vec![0u8; 196]); // fills layer 1
        let _ = p.get(2); // touch key 2 → key 1 is now least-recently-used
        let (_, evicted) = p.insert(3, 14, 14, vec![0u8; 196]);
        assert_eq!(evicted, vec![1]);
        assert!(p.get(1).is_none(), "evicted key 1 must be gone");
        assert!(p.get(2).is_some(), "key 2 must survive");
        assert!(p.get(3).is_some(), "key 3 must be present");
    }

    #[test]
    fn atlas_should_normalize_coord_to_layer_uv() {
        let mut p = packer();
        // First 8×8 tile: padded box at (0,0), glyph inset by the 1px gutter to (1,1),
        // so uv = [1/16, 1/16] .. [9/16, 9/16] in the 16px layer.
        let (placement, _) = p.insert(1, 8, 8, vec![0u8; 64]);
        assert_eq!(placement.coord.page, 0);
        assert_eq!(placement.coord.min, [1.0 / 16.0, 1.0 / 16.0]);
        assert_eq!(placement.coord.max, [9.0 / 16.0, 9.0 / 16.0]);
    }

    #[test]
    fn packer_should_pack_rgba_tile_with_bpp4() {
        // The packer is format-agnostic in pixels; the only bpp-dependent step is the
        // bitmap-length check. A bpp=4 packer accepts a `w*h*4`-byte tile and produces the
        // same normalized coord as the mono path (coords are in pixels, not bytes).
        let mut p = Packer::new(16, 2, 4);
        let (placement, evicted) = p.insert(1, 8, 8, vec![0u8; 8 * 8 * 4]);
        assert!(evicted.is_empty());
        assert_eq!(placement.coord.page, 0);
        // Same gutter inset as the mono 8×8 case: uv = [1/16, 1/16] .. [9/16, 9/16].
        assert_eq!(placement.coord.min, [1.0 / 16.0, 1.0 / 16.0]);
        assert_eq!(placement.coord.max, [9.0 / 16.0, 9.0 / 16.0]);
        assert_eq!(p.get(1), Some(placement.coord), "cached for reuse");
    }

    #[test]
    fn pad_tile_should_center_rgba_tile_in_gutter() {
        // A 2×1 RGBA tile (bpp=4) → padded box 4×3 (1px gutter all around). The two
        // texels must land at row 1, starting one texel (4 bytes) in; the gutter is 0.
        let rect = TileRect {
            page: 0,
            px: 0,
            py: 0,
            pw: 4,
            ph: 3,
            w: 2,
            h: 1,
        };
        let red = [255u8, 0, 0, 255];
        let green = [0u8, 255, 0, 255];
        let bitmap: Vec<u8> = red.iter().chain(green.iter()).copied().collect();
        let padded = pad_tile(&bitmap, &rect, 4);

        assert_eq!(padded.len(), 4 * 3 * 4, "padded box is pw*ph*bpp bytes");
        // Row 0 (top gutter) is all zero.
        assert!(
            padded[0..16].iter().all(|&b| b == 0),
            "top gutter row is zero"
        );
        // Row 1: [gutter texel][red][green][gutter texel].
        let row1 = &padded[16..32];
        assert_eq!(&row1[0..4], &[0, 0, 0, 0], "left gutter texel is zero");
        assert_eq!(&row1[4..8], &red, "first texel is red");
        assert_eq!(&row1[8..12], &green, "second texel is green");
        assert_eq!(&row1[12..16], &[0, 0, 0, 0], "right gutter texel is zero");
        // Row 2 (bottom gutter) is all zero.
        assert!(
            padded[32..48].iter().all(|&b| b == 0),
            "bottom gutter row is zero"
        );
    }

    #[test]
    fn atlas_should_skip_oversize_tile() {
        let mut p = packer();
        // 20×20 exceeds the inner max (14) → skipped, degenerate coord, no panic, no
        // cache entry.
        let (placement, evicted) = p.insert(9, 20, 20, vec![0u8; 400]);
        assert_eq!(placement.rect.w, 0, "skipped tile is degenerate");
        assert_eq!(placement.coord.max, [0.0, 0.0]);
        assert!(evicted.is_empty());
        assert!(p.cache.is_empty());
    }
}
