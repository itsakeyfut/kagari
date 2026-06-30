// Shadow primitive (#155, specs §2.5): an analytic drop shadow = a Gaussian-blurred
// rounded rectangle, drawn behind the casting box. The closed form is the
// Evan-Wallace / gpui approach: an erf along x integrated over a few Gaussian-weighted
// y samples, so corners blur correctly without a prepass. Outer shadows only (inset is
// post-MVP). Shared SDF/AA math + conventions come from the `sdf.wgsl` prelude, composed
// ahead of this file on the Rust side. All color is linear premultiplied; no sRGB here.

struct Globals {
    viewport: vec2<f32>,   // physical px
    scale: f32,            // logical px -> physical px
    _pad: f32,
};
@group(0) @binding(0) var<uniform> globals: Globals;

struct InstanceShadow {
    @location(0) bounds: vec4<f32>,        // casting box x, y, w, h (logical px)
    @location(1) corner_radii: vec4<f32>,  // tl, tr, br, bl
    @location(2) shadow: vec4<f32>,        // offset.x, offset.y, blur, spread (logical px)
    @location(3) color: vec4<f32>,         // linear premultiplied
    @location(4) mask_bounds: vec4<f32>,   // content-mask rect x, y, w, h
    @location(5) mask_radii: vec4<f32>,    // content-mask corner radii
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    // Fragment position relative to the shadow rect center (logical px, y-down).
    @location(0) p: vec2<f32>,
    @location(1) @interpolate(flat) half: vec2<f32>,    // shadow rect half-extent (box + spread)
    @location(2) @interpolate(flat) corner: f32,        // uniform corner radius (MVP)
    @location(3) @interpolate(flat) sigma: f32,         // Gaussian sigma (blur/2)
    @location(4) @interpolate(flat) color: vec4<f32>,
    @location(5) @interpolate(flat) mask_offset: vec2<f32>,
    @location(6) @interpolate(flat) mask_half: vec2<f32>,
    @location(7) @interpolate(flat) mask_radii: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, inst: InstanceShadow) -> VsOut {
    let unit = vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));

    let offset = inst.shadow.xy;
    let blur = max(inst.shadow.z, 0.0);
    let spread = inst.shadow.w;
    // Shadow rect = casting box translated by `offset`, inflated by `spread`.
    let shadow_half = max(inst.bounds.zw * 0.5 + vec2<f32>(spread), vec2<f32>(0.0));
    let shadow_center = inst.bounds.xy + inst.bounds.zw * 0.5 + offset;
    // The draw quad must cover the blur extent (~3 sigma = 1.5*blur) beyond the rect.
    let margin = blur * 1.5 + 1.0;
    let draw_half = shadow_half + vec2<f32>(margin);
    let frag = shadow_center - draw_half + unit * draw_half * 2.0;

    // logical px -> physical px -> NDC. Framebuffer y is down, clip y is up: flip.
    let pos_px = frag * globals.scale;
    let ndc = pos_px / globals.viewport * 2.0 - vec2<f32>(1.0, 1.0);

    let uniform_r = min(inst.corner_radii.x, min(shadow_half.x, shadow_half.y));

    var out: VsOut;
    out.pos = vec4<f32>(ndc.x, -ndc.y, 0.0, 1.0);
    out.p = frag - shadow_center;
    out.half = shadow_half;
    out.corner = max(uniform_r, 0.0);
    out.sigma = blur * 0.5;
    out.color = inst.color;
    out.mask_offset = shadow_center - (inst.mask_bounds.xy + inst.mask_bounds.zw * 0.5);
    out.mask_half = inst.mask_bounds.zw * 0.5;
    out.mask_radii = inst.mask_radii;
    return out;
}

// erf approximation (Abramowitz & Stegun 7.1.26), max abs error ~1.5e-7.
fn erf(x: f32) -> f32 {
    let s = sign(x);
    let a = abs(x);
    let t = 1.0 / (1.0 + 0.3275911 * a);
    let y = 1.0
        - (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t
            + 0.254829592)
            * t
            * exp(-a * a);
    return s * y;
}

fn gaussian(x: f32, sigma: f32) -> f32 {
    return exp(-(x * x) / (2.0 * sigma * sigma)) / (2.5066283 * sigma);
}

// Coverage of the blurred box along x at height `y` (relative to the rect center),
// accounting for the rounded corner so the blur follows the curve.
fn blur_along_x(x: f32, y: f32, sigma: f32, corner: f32, half: vec2<f32>) -> f32 {
    let delta = min(half.y - corner - abs(y), 0.0);
    let curved = half.x - corner + sqrt(max(corner * corner - delta * delta, 0.0));
    let integral = vec2<f32>(
        0.5 + 0.5 * erf((x - curved) * (0.7071068 / sigma)),
        0.5 + 0.5 * erf((x + curved) * (0.7071068 / sigma)),
    );
    return integral.y - integral.x;
}

// Gaussian-blurred rounded-rect coverage at `p` (centered on the rect). Integrates the
// 1D x-blur over a few Gaussian-weighted samples along y (Evan Wallace / gpui).
fn shadow_coverage(p: vec2<f32>, half: vec2<f32>, corner: f32, sigma: f32) -> f32 {
    let low = p.y - half.y;
    let high = p.y + half.y;
    let start = clamp(-3.0 * sigma, low, high);
    let end = clamp(3.0 * sigma, low, high);
    let step = (end - start) / 4.0;
    var y = start + step * 0.5;
    var value = 0.0;
    for (var i = 0; i < 4; i = i + 1) {
        value = value + blur_along_x(p.x, p.y - y, sigma, corner, half) * gaussian(y, sigma) * step;
        y = y + step;
    }
    return clamp(value, 0.0, 1.0);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Sharp (blur ~ 0) falls back to the plain rounded-box coverage; otherwise the
    // Gaussian integral. `sigma` is per-instance (flat), so the branch is uniform.
    var cov: f32;
    if (in.sigma > 0.001) {
        cov = shadow_coverage(in.p, in.half, in.corner, in.sigma);
    } else {
        cov = coverage(sd_rounded_box(in.p, in.half, in.corner));
    }
    let mask_cov = mask_coverage(in.p, in.mask_offset, in.mask_half, in.mask_radii);
    return in.color * cov * mask_cov;
}
