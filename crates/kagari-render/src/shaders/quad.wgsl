// Quad primitive. Phase #12 renders a solid fill; the SDF rounded rect, per-edge
// border, gradient, and content-mask clip arrive in #13–#15. The InstanceQuad
// struct below is byte-matched to the Rust `InstanceQuad` (160 bytes); all 10
// instance attributes are wired now so later phases only add shader logic.

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
    @location(5) border_widths: vec4<f32>, // top, right, bottom, left (px)
    @location(6) border_color: vec4<f32>,  // linear premultiplied
    @location(7) mask_bounds: vec4<f32>,   // content-mask rect x, y, w, h
    @location(8) mask_radii: vec4<f32>,    // content-mask corner radii
    @location(9) flags: u32,               // bit0: gradient bg (else solid)
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, inst: InstanceQuad) -> VsOut {
    // Triangle-strip unit-quad corners: (0,0), (1,0), (0,1), (1,1).
    let corner = vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));
    // logical px -> physical px -> NDC. Framebuffer y is down, clip y is up: flip.
    let pos_px = (inst.bounds.xy + corner * inst.bounds.zw) * globals.scale;
    let ndc = pos_px / globals.viewport * 2.0 - vec2<f32>(1.0, 1.0);
    var out: VsOut;
    out.pos = vec4<f32>(ndc.x, -ndc.y, 0.0, 1.0);
    out.color = inst.bg_color; // #12: solid fill (premultiplied linear)
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return in.color;
}
