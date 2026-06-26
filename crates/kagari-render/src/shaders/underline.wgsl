// Underline primitive (#23): a filled band (Solid) or periodic rectangular
// segments along the long axis (Dotted), for text underlines and IME preedit.
// The `sdf.wgsl` prelude (`coverage`, `sd_rounded_box`, `mask_coverage`) is composed
// ahead of this file on the Rust side.
//
// All color is linear premultiplied; no sRGB encode here (the output pass owns it).

struct Globals {
    viewport: vec2<f32>,   // physical px
    scale: f32,            // logical px -> physical px
    _pad: f32,
};
@group(0) @binding(0) var<uniform> globals: Globals;

struct InstanceUnderline {
    @location(0) bounds: vec4<f32>,       // x, y, w, h (logical px) — the band
    @location(1) color: vec4<f32>,        // linear premultiplied
    @location(2) mask_offset: vec2<f32>,  // band_center - mask_center (logical px)
    @location(3) mask_half: vec2<f32>,    // content-mask half-extent
    @location(4) mask_radii: vec4<f32>,   // content-mask corner radii (tl,tr,br,bl)
    @location(5) flags: u32,              // 0 = Solid, 1 = Dotted
    @location(6) period: f32,             // dotted segment period (logical px); 0 if Solid
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) local: vec2<f32>,                          // frag offset from top-left (logical px)
    @location(1) @interpolate(flat) half: vec2<f32>,
    @location(2) @interpolate(flat) color: vec4<f32>,
    @location(3) @interpolate(flat) mask_offset: vec2<f32>,
    @location(4) @interpolate(flat) mask_half: vec2<f32>,
    @location(5) @interpolate(flat) mask_radii: vec4<f32>,
    @location(6) @interpolate(flat) flags: u32,
    @location(7) @interpolate(flat) period: f32,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, inst: InstanceUnderline) -> VsOut {
    // Triangle-strip unit-quad corners: (0,0), (1,0), (0,1), (1,1).
    let corner = vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));
    let local = corner * inst.bounds.zw;
    let pos_px = (inst.bounds.xy + local) * globals.scale;
    let ndc = pos_px / globals.viewport * 2.0 - vec2<f32>(1.0, 1.0);

    var out: VsOut;
    out.pos = vec4<f32>(ndc.x, -ndc.y, 0.0, 1.0);
    out.local = local;
    out.half = inst.bounds.zw * 0.5;
    out.color = inst.color;
    out.mask_offset = inst.mask_offset;
    out.mask_half = inst.mask_half;
    out.mask_radii = inst.mask_radii;
    out.flags = inst.flags;
    out.period = inst.period;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Centered local coords; the band itself is the rect (no corner rounding).
    let p = in.local - in.half;
    let band = coverage(sd_rounded_box(p, in.half, 0.0));
    let mask = mask_coverage(p, in.mask_offset, in.mask_half, in.mask_radii);
    // Dotted: ON in the first half of each `period` cell along x (threshold, Q5);
    // Solid: always ON. `max(period, 1.0)` avoids div-by-zero when period is 0.
    let phase = fract(in.local.x / max(in.period, 1.0));
    let dotted_on = select(0.0, 1.0, phase < 0.5);
    let on = select(1.0, dotted_on, in.flags == 1u);
    return in.color * (band * on * mask);
}
