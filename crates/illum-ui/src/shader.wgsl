// SPDX-License-Identifier: GPL-3.0-only
// The viewer's one shader (architecture §5.4): a quad covering the picture's place in the
// window, sampled from the decoder's own planes and converted to RGB here, on the GPU. The
// same shader draws the cursor (mode 3) with its alpha.

struct Params {
    // Where the quad goes, in window pixels: x, y, width, height.
    rect: vec4<f32>,
    // Which part of the texture it shows: u0, v0, u1, v1.
    uv: vec4<f32>,
    viewport: vec2<f32>,
    // 0 = BGRA, 1 = I420 (three planes), 2 = NV12 (two planes), 3 = cursor (BGRA with alpha).
    mode: u32,
    // bit 0: BT.709 (else BT.601); bit 1: full range (else limited).
    flags: u32,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var tex0: texture_2d<f32>; // Y, or the whole BGRA picture
@group(0) @binding(3) var tex1: texture_2d<f32>; // U, or interleaved UV
@group(0) @binding(4) var tex2: texture_2d<f32>; // V

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> VsOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
    );
    let c = corners[i];
    let px = params.rect.xy + c * params.rect.zw;
    let ndc = vec2<f32>(px.x / params.viewport.x * 2.0 - 1.0, 1.0 - px.y / params.viewport.y * 2.0);
    var out: VsOut;
    out.pos = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = params.uv.xy + c * (params.uv.zw - params.uv.xy);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    if params.mode == 0u {
        return vec4<f32>(textureSample(tex0, samp, in.uv).rgb, 1.0);
    }
    if params.mode == 3u {
        return textureSample(tex0, samp, in.uv);
    }
    let y = textureSample(tex0, samp, in.uv).r;
    var u: f32;
    var v: f32;
    if params.mode == 1u {
        u = textureSample(tex1, samp, in.uv).r;
        v = textureSample(tex2, samp, in.uv).r;
    } else {
        let uv = textureSample(tex1, samp, in.uv).rg;
        u = uv.r;
        v = uv.g;
    }
    let bt709 = (params.flags & 1u) != 0u;
    let full = (params.flags & 2u) != 0u;
    let kr = select(0.299, 0.2126, bt709);
    let kb = select(0.114, 0.0722, bt709);
    let kg = 1.0 - kr - kb;
    let y_off = select(16.0 / 255.0, 0.0, full);
    let y_scale = select(255.0 / 219.0, 1.0, full);
    let c_scale = select(255.0 / 224.0, 1.0, full);
    let yy = (y - y_off) * y_scale;
    let cb = (u - 0.5) * c_scale;
    let cr = (v - 0.5) * c_scale;
    let r = yy + 2.0 * (1.0 - kr) * cr;
    let b = yy + 2.0 * (1.0 - kb) * cb;
    let g = yy - (2.0 * kb * (1.0 - kb) * cb + 2.0 * kr * (1.0 - kr) * cr) / kg;
    return vec4<f32>(clamp(vec3<f32>(r, g, b), vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
