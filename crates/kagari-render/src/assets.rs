//! Asset loader (#56, specs §2.10): decode raster images (PNG/JPG) with the `image` crate off the UI
//! thread, upload them into the RGBA atlas (#55), cache by id, and fall back to a placeholder on
//! failure.
//!
//! **Runtime-agnostic (§7.2):** the loader does not hardcode a threading model — it takes an injected
//! `spawn` closure (the app supplies `std::thread::spawn`, or a `rayon` / `tokio::spawn_blocking`
//! adapter; tests run decodes inline). A decode job sends its result down an `mpsc` channel; the UI
//! thread drains the channel each frame and uploads the decoded tile into the atlas (GPU work stays on
//! the UI thread). The live `EventLoopProxy` wakeup + the `provide_context` of this service land with
//! the app loop (deferred, mirroring the #44 theme watcher / #66 proxy bridge).

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use crate::atlas::{Atlas, AtlasCoord};
use crate::error::RenderError;

/// What to load. `#[non_exhaustive]` so a `Named` bundled-asset variant (SVG icons, #57) can be added
/// without a breaking change.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum AssetSource {
    /// A filesystem path (read off the UI thread). `PathBuf` — no OS-specific literals (rust.md).
    Path(PathBuf),
    /// In-memory encoded bytes (e.g. an embedded asset).
    Bytes(Arc<[u8]>),
}

/// A stable handle for a loaded asset: a hash of its [`AssetSource`]. The same source loads to the same
/// id, so `load` is idempotent and the atlas key is shared with the loader's cache.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct AssetId(u64);

/// The load state of an [`AssetId`] in the loader's cache.
#[derive(Clone, Copy, PartialEq, Debug)]
#[non_exhaustive]
pub enum LoadState {
    /// Decode dispatched, not yet drained.
    Loading,
    /// Decoded and uploaded; its atlas coord.
    Ready(AtlasCoord),
    /// Decode/IO failed; `resolve` yields the placeholder.
    Failed,
}

/// A decoded RGBA8 image (straight alpha, sRGB) ready for the RGBA atlas. `rgba` is `width*height*4`
/// bytes, row-major. Internal — the loader's decode intermediate, not part of the public API.
pub(crate) struct DecodedImage {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// The reserved atlas key for the shared placeholder tile (never collides with an `AssetId` hash).
const PLACEHOLDER_KEY: u64 = u64::MAX;

/// Decode encoded raster bytes to RGBA8 via the `image` crate. Runtime-agnostic and **panic-free on bad
/// input** — a malformed/unsupported image maps to [`RenderError::AssetDecode`]. `label` is the source
/// description used in the error (path or `<bytes>`).
pub(crate) fn decode_image(bytes: &[u8], label: &str) -> Result<DecodedImage, RenderError> {
    let img = image::load_from_memory(bytes)
        .map_err(|_| RenderError::AssetDecode {
            path: label.to_string(),
        })?
        .to_rgba8();
    let (width, height) = img.dimensions();
    Ok(DecodedImage {
        rgba: img.into_raw(),
        width,
        height,
    })
}

/// A 16×16 magenta/charcoal checkerboard (RGBA, straight alpha) shown for a missing/failed asset — an
/// intentionally conspicuous "broken image" marker.
pub(crate) fn checkerboard() -> DecodedImage {
    const SIZE: u32 = 16;
    const CELL: u32 = 8;
    let magenta = [255u8, 0, 255, 255];
    let charcoal = [40u8, 40, 40, 255];
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dark = ((x / CELL) + (y / CELL)) % 2 == 0;
            rgba.extend_from_slice(if dark { &charcoal } else { &magenta });
        }
    }
    DecodedImage {
        rgba,
        width: SIZE,
        height: SIZE,
    }
}

/// A decode job handed to the injected spawner: it decodes (off the UI thread) and sends its result
/// down the loader's channel.
type DecodeJob = Box<dyn FnOnce() + Send + 'static>;

type DecodeResult = (AssetId, Result<DecodedImage, RenderError>);

/// The asset service: decode off-thread (via an injected spawner), cache by [`AssetId`], drain finished
/// decodes into the RGBA atlas on the UI thread, and resolve to a placeholder until ready.
///
/// The `provide_context` of this service is deferred to the app loop (like `FocusRegistry` /
/// `CursorRegistry` in core); this is the standalone service it will provide.
pub struct AssetLoader {
    spawn: Box<dyn Fn(DecodeJob)>,
    tx: Sender<DecodeResult>,
    rx: Receiver<DecodeResult>,
    cache: HashMap<AssetId, LoadState>,
    placeholder: AtlasCoord,
}

impl AssetLoader {
    /// Builds a loader with an injected `spawn` (prod: `|job| { std::thread::spawn(job); }`; tests:
    /// `|job| job()` to run inline) and the `placeholder` atlas coord (uploaded by the renderer).
    pub fn new(spawn: impl Fn(DecodeJob) + 'static, placeholder: AtlasCoord) -> Self {
        let (tx, rx) = channel();
        Self {
            spawn: Box::new(spawn),
            tx,
            rx,
            cache: HashMap::new(),
            placeholder,
        }
    }

    /// The atlas key the renderer uploads the placeholder tile under.
    pub(crate) fn placeholder_key() -> u64 {
        PLACEHOLDER_KEY
    }

    /// Loads `source`, returning its [`AssetId`]. Idempotent: a cached id (in any state) is a no-op
    /// (no re-decode). Otherwise the id is marked [`LoadState::Loading`] and a decode job is dispatched
    /// via the injected spawner (a `Path` is read off-thread; `Bytes` is decoded directly); the job
    /// sends its result down the channel for [`drain`](Self::drain).
    pub fn load(&mut self, source: AssetSource) -> AssetId {
        let id = id_of(&source);
        if self.cache.contains_key(&id) {
            return id;
        }
        self.cache.insert(id, LoadState::Loading);
        let tx = self.tx.clone();
        let job: DecodeJob = Box::new(move || {
            let result = match source {
                AssetSource::Path(path) => {
                    let label = path.display().to_string();
                    std::fs::read(&path)
                        .map_err(|_| RenderError::AssetDecode {
                            path: label.clone(),
                        })
                        .and_then(|bytes| decode_image(&bytes, &label))
                }
                AssetSource::Bytes(bytes) => decode_image(&bytes, "<bytes>"),
            };
            // The receiver lives as long as the loader; a send error only means the loader was dropped.
            let _ = tx.send((id, result));
        });
        (self.spawn)(job);
        id
    }

    /// Drains finished decodes (called on the UI thread once per frame): uploads each `Ok` tile into
    /// `atlas` and records `Ready(coord)`; logs and records `Failed` for each error (the placeholder is
    /// shown for those). Caches the outcome so a broken asset is not retried every frame.
    pub fn drain(&mut self, atlas: &mut Atlas) {
        while let Ok((id, result)) = self.rx.try_recv() {
            match result {
                Ok(img) => {
                    let (w, h) = (img.width, img.height);
                    let coord = atlas.get_or_insert(id.0, (w, h), move || img.rgba);
                    // An image too large for the atlas layer (> inner_max) is skipped by the packer,
                    // which returns a degenerate coord (`max == [0, 0]`). Decode succeeded, but the tile
                    // is not in the atlas — fall back to the placeholder rather than drawing nothing.
                    if coord.max == [0.0, 0.0] {
                        tracing::warn!(
                            w,
                            h,
                            "asset too large for the image atlas; showing placeholder"
                        );
                        self.cache.insert(id, LoadState::Failed);
                    } else {
                        self.cache.insert(id, LoadState::Ready(coord));
                    }
                }
                Err(error) => {
                    tracing::warn!(?error, "asset decode failed; showing placeholder");
                    self.cache.insert(id, LoadState::Failed);
                }
            }
        }
    }

    /// The atlas coord for `id`: its tile when [`Ready`](LoadState::Ready), else the placeholder
    /// (while loading, on failure, or for an unknown id).
    pub fn resolve(&self, id: AssetId) -> AtlasCoord {
        match self.cache.get(&id) {
            Some(LoadState::Ready(coord)) => *coord,
            _ => self.placeholder,
        }
    }

    /// The current [`LoadState`] of `id`, if known (mainly for tests / diagnostics).
    pub fn state(&self, id: AssetId) -> Option<LoadState> {
        self.cache.get(&id).copied()
    }
}

/// Hashes a source into its stable [`AssetId`]. `Path` hashes the path; `Bytes` hashes the content.
fn id_of(source: &AssetSource) -> AssetId {
    let mut hasher = DefaultHasher::new();
    match source {
        AssetSource::Path(path) => {
            0u8.hash(&mut hasher);
            path.hash(&mut hasher);
        }
        AssetSource::Bytes(bytes) => {
            1u8.hash(&mut hasher);
            bytes.hash(&mut hasher);
        }
    }
    AssetId(hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a known `w*h` solid-color RGBA image to PNG bytes in memory (no committed fixture).
    fn png_bytes(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
        let buf = image::RgbaImage::from_pixel(w, h, image::Rgba(rgba));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(buf)
            .write_to(&mut out, image::ImageFormat::Png)
            .expect("encode test PNG");
        out.into_inner()
    }

    /// A dummy non-placeholder coord for loaders under test (the renderer supplies a real one).
    fn dummy_coord() -> AtlasCoord {
        AtlasCoord {
            page: 7,
            min: [0.0, 0.0],
            max: [0.5, 0.5],
        }
    }

    #[test]
    fn decode_image_should_roundtrip_rgba() {
        let bytes = png_bytes(3, 2, [10, 20, 30, 255]);
        let decoded = decode_image(&bytes, "test").expect("decode the encoded PNG");
        assert_eq!((decoded.width, decoded.height), (3, 2));
        assert_eq!(decoded.rgba.len(), 3 * 2 * 4);
        assert_eq!(
            &decoded.rgba[0..4],
            &[10, 20, 30, 255],
            "first texel round-trips"
        );
    }

    #[test]
    fn decode_image_should_error_on_garbage() {
        let result = decode_image(&[0xDE, 0xAD, 0xBE, 0xEF, 0x00], "garbage");
        assert!(
            matches!(result, Err(RenderError::AssetDecode { .. })),
            "garbage bytes map to AssetDecode (no panic)"
        );
    }

    #[test]
    fn checkerboard_should_be_rgba_16x16() {
        let c = checkerboard();
        assert_eq!((c.width, c.height), (16, 16));
        assert_eq!(c.rgba.len(), 16 * 16 * 4);
    }

    #[test]
    fn load_should_be_idempotent_by_id() {
        // Inline spawner: the decode runs synchronously inside `load`, but the result is only applied
        // by `drain` — so without draining, a second `load` of the same source is a cache no-op.
        let mut loader = AssetLoader::new(|job| job(), dummy_coord());
        let src = AssetSource::Bytes(Arc::from(
            png_bytes(2, 2, [0, 255, 0, 255]).into_boxed_slice(),
        ));
        let a = loader.load(src.clone());
        let b = loader.load(src);
        assert_eq!(a, b, "same source → same id");
        assert_eq!(
            loader.state(a),
            Some(LoadState::Loading),
            "one entry, still loading pre-drain"
        );
    }

    #[test]
    fn resolve_should_return_placeholder_until_ready() {
        let loader = AssetLoader::new(|job| job(), dummy_coord());
        let unknown = id_of(&AssetSource::Bytes(Arc::from(&[1u8, 2, 3][..])));
        assert_eq!(
            loader.resolve(unknown),
            dummy_coord(),
            "an unknown/not-ready id resolves to the placeholder"
        );
    }
}
