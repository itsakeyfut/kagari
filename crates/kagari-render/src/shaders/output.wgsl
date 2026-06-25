// Output-transform pass: a fullscreen triangle that samples the offscreen linear
// target and writes it to the swapchain. The swapchain is an sRGB format, so the
// hardware performs the linear->sRGB encode on store — the shader does no encode
// here (this is the extension point for tone mapping / HDR output, post-MVP).

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VsOut {
    // One oversized triangle covering the screen: clip-space corners
    // (-1,-1), (-1,3), (3,-1). uv maps clip -> texture space with the v axis
    // flipped (clip y is up, texture v is down), so screen-top samples row 0.
    let x = f32(i32(idx) / 2) * 4.0 - 1.0;
    let y = f32(i32(idx) % 2) * 4.0 - 1.0;
    var out: VsOut;
    out.pos = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>(x * 0.5 + 0.5, 1.0 - (y * 0.5 + 0.5));
    return out;
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(src, src_sampler, in.uv);
}
