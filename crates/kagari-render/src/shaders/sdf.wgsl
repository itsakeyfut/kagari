// Shared SDF / anti-aliasing prelude (wgsl.md §1). Composed ahead of each
// primitive shader on the Rust side (quad.rs `QUAD_SHADER_SRC`), so the same math
// is never copy-pasted per shader.
//
// CONVENTIONS (fixed project-wide, wgsl.md §5/§7):
//   - SDF local space is CENTERED on the shape, in LOGICAL px, with y pointing DOWN
//     (top-left origin, matching the framebuffer). So p.y < 0 is the top half,
//     p.y > 0 the bottom half; p.x < 0 left, p.x > 0 right.
//   - Colors are LINEAR, PREMULTIPLIED alpha. No sRGB encode here (the output pass
//     owns the linear->sRGB encode).

// Signed distance to a rounded box (Inigo Quilez). `half` is the half-extent, `r`
// the corner radius for this fragment's quadrant. Negative inside, 0 on the edge.
// q = |p| - half + r folds the problem into the first quadrant; the length() term
// rounds the corner and the min(max(q.x,q.y),0) term keeps the straight edges flat.
fn sd_rounded_box(p: vec2<f32>, half: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - half + vec2<f32>(r);
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

// Per-quadrant corner radius from `radii = (tl, tr, br, bl)`. Branch-free via
// select() so callers stay in uniform control flow for fwidth (wgsl.md §6).
fn corner_radius(p: vec2<f32>, radii: vec4<f32>) -> f32 {
    // Row pairs ordered (left, right): top = (tl, tr), bottom = (bl, br).
    let pair = select(radii.xy, radii.wz, p.y > 0.0);
    return select(pair.x, pair.y, p.x > 0.0);
}

// Coverage from a signed distance (wgsl.md §6): ~1px analytic AA. `aa = fwidth(d)`
// is the per-physical-pixel change of d, so the smoothstep spans ~1 physical px
// regardless of the logical-px units of d. MUST be called in uniform control flow.
fn coverage(d: f32) -> f32 {
    let aa = fwidth(d);
    return 1.0 - smoothstep(-aa, aa, d);
}

// Content-mask clip coverage, shared by every primitive (quad/sprite/...). `p` is the
// fragment in the shape's centered local space; `mask_offset = shape_center - mask_center`
// maps it into the mask's frame. Returns ~1 inside the rounded-rect mask, 0 outside,
// AA on the edge. A no-op (huge) mask yields ~1. Branch-free.
fn mask_coverage(p: vec2<f32>, mask_offset: vec2<f32>, mask_half: vec2<f32>, mask_radii: vec4<f32>) -> f32 {
    let mp = p + mask_offset;
    let r = min(corner_radius(mp, mask_radii), min(mask_half.x, mask_half.y));
    return coverage(sd_rounded_box(mp, mask_half, r));
}
