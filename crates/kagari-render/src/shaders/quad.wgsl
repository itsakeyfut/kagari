// Quad primitive. Renders an SDF rounded rectangle with per-corner radii, a
// per-edge border band, and analytic ~1px AA (#13). Gradient bg (#14) and
// content-mask clip (#15) only add logic later; the InstanceQuad struct below is
// byte-matched to the Rust `InstanceQuad` (160 bytes) and stays fixed.
//
// Shared SDF/AA math (`sd_rounded_box`, `corner_radius`, `coverage`) and the
// coordinate/color conventions come from the `sdf.wgsl` prelude, composed ahead of
// this file on the Rust side. All color is linear premultiplied; no sRGB here.

struct Globals {
    viewport: vec2<f32>,   // physical px
    scale: f32,            // logical px -> physical px
    _pad: f32,
};
@group(0) @binding(0) var<uniform> globals: Globals;

struct InstanceQuad {
    @location(0) bounds: vec4<f32>,        // x, y, w, h (logical px)
    @location(1) corner_radii: vec4<f32>,  // tl, tr, br, bl
    @location(2) bg_color: vec4<f32>,      // linear premultiplied; solid / grad stop 0
    @location(3) bg_grad_color: vec4<f32>, // grad stop 1
    @location(4) bg_grad_dir: vec4<f32>,   // start.xy, end.xy in [0,1] quad space
    @location(5) border_widths: vec4<f32>, // top, right, bottom, left (logical px)
    @location(6) border_color: vec4<f32>,  // linear premultiplied
    @location(7) mask_bounds: vec4<f32>,   // content-mask rect x, y, w, h
    @location(8) mask_radii: vec4<f32>,    // content-mask corner radii
    @location(9) flags: u32,               // bit0: gradient bg (else solid)
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    // Fragment offset from the quad's top-left in logical px (smooth); the only
    // per-fragment-varying input — everything else is a per-instance constant.
    @location(0) local: vec2<f32>,
    @location(1) @interpolate(flat) half: vec2<f32>,
    @location(2) @interpolate(flat) corner_radii: vec4<f32>,
    @location(3) @interpolate(flat) border_widths: vec4<f32>,
    @location(4) @interpolate(flat) border_color: vec4<f32>,
    @location(5) @interpolate(flat) bg: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, inst: InstanceQuad) -> VsOut {
    // Triangle-strip unit-quad corners: (0,0), (1,0), (0,1), (1,1).
    let corner = vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));
    let local = corner * inst.bounds.zw;
    // logical px -> physical px -> NDC. Framebuffer y is down, clip y is up: flip.
    let pos_px = (inst.bounds.xy + local) * globals.scale;
    let ndc = pos_px / globals.viewport * 2.0 - vec2<f32>(1.0, 1.0);

    var out: VsOut;
    out.pos = vec4<f32>(ndc.x, -ndc.y, 0.0, 1.0);
    out.local = local;
    out.half = inst.bounds.zw * 0.5;
    out.corner_radii = inst.corner_radii;
    out.border_widths = inst.border_widths;
    out.border_color = inst.border_color;
    out.bg = inst.bg_color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Centered local coords (y-down), in logical px — see sdf.wgsl conventions.
    let p = in.local - in.half;

    // Outer rounded-box edge. Clamp the radius so it never exceeds the half-extent.
    let rad = min(corner_radius(p, in.corner_radii), min(in.half.x, in.half.y));
    let d_outer = sd_rounded_box(p, in.half, rad);

    // Inner edge: inset the box per-edge by the border widths (top, right, bottom,
    // left). The inset shifts the inner center and shrinks the half-extent; the
    // inner corner radius is reduced by the thicker adjacent border (an
    // approximation — per-edge borders + rounded corners have no exact closed form).
    let bw = in.border_widths;
    let top = bw.x;
    let right = bw.y;
    let bottom = bw.z;
    let left = bw.w;
    let inner_center = vec2<f32>((left - right) * 0.5, (top - bottom) * 0.5);
    let inner_half = max(in.half - vec2<f32>((left + right) * 0.5, (top + bottom) * 0.5), vec2<f32>(0.0));
    // Classify the quadrant in the inner box's own frame so the adjacent-border
    // pick stays consistent in the thin band between the two centers.
    let pi = p - inner_center;
    let adj = max(select(top, bottom, pi.y > 0.0), select(left, right, pi.x > 0.0));
    // Clamp the inner radius to its own half-extent, mirroring the outer `rad`, so
    // the iq SDF stays valid (r <= min(half)) under thick/asymmetric borders.
    let r_inner = clamp(rad - adj, 0.0, min(inner_half.x, inner_half.y));
    let d_inner = sd_rounded_box(pi, inner_half, r_inner);

    // Border where outside the inner edge, background where inside; both blended by
    // SDF coverage so every edge stays anti-aliased. Premultiplied, so scaling the
    // whole RGBA by the outer coverage is the correct shape mask.
    let body = mix(in.border_color, in.bg, coverage(d_inner));
    return body * coverage(d_outer);
}
