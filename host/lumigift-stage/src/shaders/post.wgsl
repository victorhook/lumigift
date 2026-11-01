// Lumigift stage: bloom (dual-filter / CoD:AW style) and final composite.

struct Post {
    uv_rect: vec4f,     // composite: uv offset (xy) and scale (zw) of the visible part
    params: vec4f,      // x exposure, y bloom strength, z bloom threshold, w time
    params2: vec4f,     // x output is sRGB target (1) or gamma (0), y vignette, z grain, w bloom mips
};

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var<uniform> post: Post;
// Composite only: bloom result.
@group(0) @binding(3) var bloom_tex: texture_2d<f32>;

struct FsIn {
    @builtin(position) pos: vec4f,
    @location(0) uv: vec2f,
};

@vertex
fn vs_fullscreen(@builtin(vertex_index) vi: u32) -> FsIn {
    // One oversized triangle covering the viewport.
    let x = f32((vi << 1u) & 2u);
    let y = f32(vi & 2u);
    var out: FsIn;
    out.pos = vec4f(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    out.uv = vec2f(x, y);
    return out;
}

fn luma(c: vec3f) -> f32 {
    return dot(c, vec3f(0.2126, 0.7152, 0.0722));
}

fn karis(c: vec3f) -> f32 {
    return 1.0 / (1.0 + luma(c));
}

fn s(uv: vec2f) -> vec3f {
    return textureSampleLevel(src, samp, uv, 0.0).rgb;
}

// 13-tap downsample (Jimenez 2014). `first` applies the soft threshold and a
// Karis average per 2×2 group to suppress fireflies.
fn downsample(uv: vec2f, first: bool) -> vec3f {
    let t = 1.0 / vec2f(textureDimensions(src));
    let a = s(uv + t * vec2f(-2.0, -2.0));
    let b = s(uv + t * vec2f(0.0, -2.0));
    let c = s(uv + t * vec2f(2.0, -2.0));
    let d = s(uv + t * vec2f(-2.0, 0.0));
    let e = s(uv);
    let f = s(uv + t * vec2f(2.0, 0.0));
    let g = s(uv + t * vec2f(-2.0, 2.0));
    let h = s(uv + t * vec2f(0.0, 2.0));
    let i = s(uv + t * vec2f(2.0, 2.0));
    let j = s(uv + t * vec2f(-1.0, -1.0));
    let k = s(uv + t * vec2f(1.0, -1.0));
    let l = s(uv + t * vec2f(-1.0, 1.0));
    let m = s(uv + t * vec2f(1.0, 1.0));
    if (first) {
        let g0 = (a + b + d + e) * 0.25;
        let g1 = (b + c + e + f) * 0.25;
        let g2 = (d + e + g + h) * 0.25;
        let g3 = (e + f + h + i) * 0.25;
        let g4 = (j + k + l + m) * 0.25;
        let w0 = karis(g0) * 0.125;
        let w1 = karis(g1) * 0.125;
        let w2 = karis(g2) * 0.125;
        let w3 = karis(g3) * 0.125;
        let w4 = karis(g4) * 0.5;
        var col = (g0 * w0 + g1 * w1 + g2 * w2 + g3 * w3 + g4 * w4) / (w0 + w1 + w2 + w3 + w4);
        // Soft-knee threshold on brightness.
        let thr = post.params.z;
        let knee = thr * 0.5 + 1e-4;
        let br = max(max(col.r, col.g), col.b);
        var rq = clamp(br - thr + knee, 0.0, 2.0 * knee);
        rq = rq * rq / (4.0 * knee);
        let contrib = max(rq, br - thr) / max(br, 1e-4);
        col *= contrib;
        return col;
    }
    var col = e * 0.125;
    col += (a + c + g + i) * 0.03125;
    col += (b + d + f + h) * 0.0625;
    col += (j + k + l + m) * 0.125;
    return col;
}

@fragment
fn fs_down_first(in: FsIn) -> @location(0) vec4f {
    return vec4f(downsample(in.uv, true), 1.0);
}

@fragment
fn fs_down(in: FsIn) -> @location(0) vec4f {
    return vec4f(downsample(in.uv, false), 1.0);
}

// 3×3 tent upsample, additively blended onto the next larger mip.
@fragment
fn fs_up(in: FsIn) -> @location(0) vec4f {
    let t = 1.0 / vec2f(textureDimensions(src));
    let uv = in.uv;
    var col = s(uv) * 4.0;
    col += (s(uv + vec2f(-t.x, 0.0)) + s(uv + vec2f(t.x, 0.0)) + s(uv + vec2f(0.0, -t.y)) + s(uv + vec2f(0.0, t.y))) * 2.0;
    col += s(uv + vec2f(-t.x, -t.y)) + s(uv + vec2f(t.x, -t.y)) + s(uv + vec2f(-t.x, t.y)) + s(uv + vec2f(t.x, t.y));
    return vec4f(col / 16.0, 1.0);
}

// ---------------------------------------------------------------------------
// Composite (runs inside egui's render pass)
// ---------------------------------------------------------------------------

// Hue-preserving tonemap with white-hot cores (docs/UI_DESIGN.md §5.5),
// with an AgX-like smooth shoulder.
fn tonemap(c: vec3f) -> vec3f {
    let m = max(max(c.r, c.g), c.b);
    let mapped = c / (1.0 + m);
    // Very bright, saturated light desaturates towards white like a camera sensor.
    let w = smoothstep(1.5, 10.0, m) * 0.5;
    return mix(mapped, vec3f(m / (1.0 + m)), w);
}

fn linear_to_srgb(c: vec3f) -> vec3f {
    let lo = c * 12.92;
    let hi = 1.055 * pow(max(c, vec3f(0.0)), vec3f(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3f(0.0031308));
}

fn srgb_to_linear(c: vec3f) -> vec3f {
    let lo = c / 12.92;
    let hi = pow((c + 0.055) / 1.055, vec3f(2.4));
    return select(hi, lo, c <= vec3f(0.04045));
}

fn hash12(p: vec2f) -> f32 {
    var p3 = fract(vec3f(p.xyx) * 0.1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

@fragment
fn fs_composite(in: FsIn) -> @location(0) vec4f {
    let uv = post.uv_rect.xy + in.uv * post.uv_rect.zw;
    let scene = textureSampleLevel(src, samp, uv, 0.0).rgb;
    let bloom = textureSampleLevel(bloom_tex, samp, uv, 0.0).rgb / max(post.params2.w, 1.0);
    let hdr = (scene + bloom * post.params.y) * post.params.x;

    var col = tonemap(hdr);

    // Vignette.
    let vd = length(uv - 0.5) * 1.35;
    col *= mix(1.0, 1.0 - post.params2.y, smoothstep(0.45, 1.05, vd));

    // Dither in gamma space: ±1 LSB triangular noise, plus optional visible grain.
    col = linear_to_srgb(col);
    let px = in.pos.xy;
    let t = fract(post.params.w * 0.61803);
    let n1 = hash12(px + t * 913.0);
    let n2 = hash12(px.yx + t * 577.0 + 17.0);
    col += (n1 + n2 - 1.0) * (1.0 / 255.0) + (n1 - 0.5) * post.params2.z;
    col = max(col, vec3f(0.0));
    if (post.params2.x > 0.5) {
        // The target encodes sRGB itself.
        col = srgb_to_linear(col);
    }
    return vec4f(col, 1.0);
}
