// MonochromeSprite primitive (#19): an alpha-coverage tile from the R8 atlas
// (`texture_2d_array`, decision Q1) multiplied by a premultiplied-linear color, with
// the shared content-mask clip. Used by glyphs (#22). The `sdf.wgsl` prelude
// (`mask_coverage`) is composed ahead of this file on the Rust side.
//
// All color is linear premultiplied; no sRGB encode here (the output pass owns it).

struct Globals {
    viewport: vec2<f32>,   // physical px
    scale: f32,            // logical px -> physical px
    _pad: f32,
};
@group(0) @binding(0) var<uniform> globals: Globals;

// The R8 coverage atlas (decision Q1: one array texture, sampled linearly, Q4).
@group(1) @binding(0) var atlas: texture_2d_array<f32>;
@group(1) @binding(1) var atlas_samp: sampler;

struct InstanceSprite {
    @location(0) bounds: vec4<f32>,       // x, y, w, h (logical px)
    @location(1) uv: vec4<f32>,           // atlas uv: min.xy, max.xy (normalized)
    @location(2) color: vec4<f32>,        // linear premultiplied
    @location(3) mask_offset: vec2<f32>,  // sprite_center - mask_center (logical px)
    @location(4) mask_half: vec2<f32>,    // content-mask half-extent
    @location(5) mask_radii: vec4<f32>,   // content-mask corner radii (tl,tr,br,bl)
    @location(6) page: u32,               // atlas array layer
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) local: vec2<f32>,                          // frag offset from top-left (logical px)
    @location(1) uv: vec2<f32>,                             // interpolated atlas uv
    @location(2) @interpolate(flat) half: vec2<f32>,
    @location(3) @interpolate(flat) color: vec4<f32>,
    @location(4) @interpolate(flat) mask_offset: vec2<f32>,
    @location(5) @interpolate(flat) mask_half: vec2<f32>,
    @location(6) @interpolate(flat) mask_radii: vec4<f32>,
    @location(7) @interpolate(flat) page: u32,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, inst: InstanceSprite) -> VsOut {
    // Triangle-strip unit-quad corners: (0,0), (1,0), (0,1), (1,1).
    let corner = vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));
    let local = corner * inst.bounds.zw;
    let pos_px = (inst.bounds.xy + local) * globals.scale;
    let ndc = pos_px / globals.viewport * 2.0 - vec2<f32>(1.0, 1.0);

    var out: VsOut;
    out.pos = vec4<f32>(ndc.x, -ndc.y, 0.0, 1.0);
    out.local = local;
    out.uv = mix(inst.uv.xy, inst.uv.zw, corner);
    out.half = inst.bounds.zw * 0.5;
    out.color = inst.color;
    out.mask_offset = inst.mask_offset;
    out.mask_half = inst.mask_half;
    out.mask_radii = inst.mask_radii;
    out.page = inst.page;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Single-mip atlas: explicit LOD 0 with the linear sampler (decision Q4).
    let a = textureSampleLevel(atlas, atlas_samp, in.uv, in.page, 0.0).r;
    // Centered local coords for the content-mask clip (shared prelude).
    let p = in.local - in.half;
    let mask = mask_coverage(p, in.mask_offset, in.mask_half, in.mask_radii);
    // coverage * premultiplied color, clipped by the content mask.
    return in.color * (a * mask);
}
