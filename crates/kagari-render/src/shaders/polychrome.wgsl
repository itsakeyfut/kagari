// PolychromeSprite primitive (#55): a colored-image tile from the RGBA atlas
// (`texture_2d_array`) multiplied by a tint, with the shared content-mask clip. Used by
// images/icons. The `sdf.wgsl` prelude (`mask_coverage`) is composed ahead of this file
// on the Rust side. Mirrors `sprite.wgsl` (vs identical); the fragment differs.
//
// Color: the atlas is `Rgba8UnormSrgb`, so the sample HW-decodes sRGB->linear, with
// straight (non-premultiplied) alpha. We premultiply in linear here (premultiplication
// must happen in linear space), then multiply by the premultiplied-linear `tint`
// (`Color::WHITE` = identity) and the content-mask coverage. The output is linear
// premultiplied; the output pass owns the sRGB encode.

struct Globals {
    viewport: vec2<f32>,   // physical px
    scale: f32,            // logical px -> physical px
    _pad: f32,
};
@group(0) @binding(0) var<uniform> globals: Globals;

// The RGBA image atlas (one array texture, sampled linearly).
@group(1) @binding(0) var atlas: texture_2d_array<f32>;
@group(1) @binding(1) var atlas_samp: sampler;

struct InstancePolychrome {
    @location(0) bounds: vec4<f32>,       // x, y, w, h (logical px)
    @location(1) uv: vec4<f32>,           // atlas uv: min.xy, max.xy (normalized)
    @location(2) tint: vec4<f32>,         // linear premultiplied (WHITE = unmodified)
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
    @location(3) @interpolate(flat) tint: vec4<f32>,
    @location(4) @interpolate(flat) mask_offset: vec2<f32>,
    @location(5) @interpolate(flat) mask_half: vec2<f32>,
    @location(6) @interpolate(flat) mask_radii: vec4<f32>,
    @location(7) @interpolate(flat) page: u32,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, inst: InstancePolychrome) -> VsOut {
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
    out.tint = inst.tint;
    out.mask_offset = inst.mask_offset;
    out.mask_half = inst.mask_half;
    out.mask_radii = inst.mask_radii;
    out.page = inst.page;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Single-mip atlas: explicit LOD 0 with the linear sampler. The sRGB format decodes
    // RGB to linear on sample; alpha is straight.
    let texel = textureSampleLevel(atlas, atlas_samp, in.uv, in.page, 0.0);
    // Premultiply in linear (the blend state is premultiplied "over").
    let premult = vec4<f32>(texel.rgb * texel.a, texel.a);
    // Centered local coords for the content-mask clip (shared prelude).
    let p = in.local - in.half;
    let mask = mask_coverage(p, in.mask_offset, in.mask_half, in.mask_radii);
    // image premultiplied * premultiplied tint (WHITE = identity), clipped by the mask.
    return premult * in.tint * mask;
}
