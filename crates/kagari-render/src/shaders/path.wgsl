// Path primitive (#58): a tessellated fill/stroke mesh with feathered per-vertex AA. The shared
// `sdf.wgsl` prelude (`mask_coverage`) is composed ahead of this file on the Rust side.
//
// Non-instanced: real vertices in absolute logical px. Color is linear premultiplied; no sRGB encode
// here (the output pass owns it).
//
// Feather AA: coverage = clamp(cov_a - abs(cov_b), 0, 1), unifying fill and stroke —
//   - fill:   cov_a = 1 interior, ramps 1→0 across the 1px outline fringe; cov_b = 0.
//   - stroke: cov_a = half_width + 0.5 (flat), cov_b = signed distance from the centerline; the band is
//     solid in the core and ramps to 0 over the outer ~1px, centered on the nominal edge.

struct Globals {
    viewport: vec2<f32>,   // physical px
    scale: f32,            // logical px -> physical px
    _pad: f32,
};
@group(0) @binding(0) var<uniform> globals: Globals;

struct VsIn {
    @location(0) position: vec2<f32>,     // absolute logical px
    @location(1) cov_a: f32,
    @location(2) cov_b: f32,
    @location(3) color: vec4<f32>,        // linear premultiplied (flat)
    @location(4) mask_offset: vec2<f32>,  // -mask_center (flat)
    @location(5) mask_half: vec2<f32>,
    @location(6) mask_radii: vec4<f32>,
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) world: vec2<f32>,                          // absolute logical px (for the mask)
    @location(1) cov_a: f32,
    @location(2) cov_b: f32,
    @location(3) @interpolate(flat) color: vec4<f32>,
    @location(4) @interpolate(flat) mask_offset: vec2<f32>,
    @location(5) @interpolate(flat) mask_half: vec2<f32>,
    @location(6) @interpolate(flat) mask_radii: vec4<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    let pos_px = in.position * globals.scale;
    let ndc = pos_px / globals.viewport * 2.0 - vec2<f32>(1.0, 1.0);

    var out: VsOut;
    out.pos = vec4<f32>(ndc.x, -ndc.y, 0.0, 1.0);
    out.world = in.position;
    out.cov_a = in.cov_a;
    out.cov_b = in.cov_b;
    out.color = in.color;
    out.mask_offset = in.mask_offset;
    out.mask_half = in.mask_half;
    out.mask_radii = in.mask_radii;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Feather coverage from the interpolated AA pair (see the header).
    let cov = clamp(in.cov_a - abs(in.cov_b), 0.0, 1.0);
    // Content-mask clip: `world` is absolute px, `mask_offset = -mask_center`, so the prelude's
    // `mp = world + mask_offset = world - mask_center` is mask-relative.
    let mask = mask_coverage(in.world, in.mask_offset, in.mask_half, in.mask_radii);
    return in.color * (cov * mask);
}
