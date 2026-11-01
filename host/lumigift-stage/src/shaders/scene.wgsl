// Lumigift stage: scene pass (boxes, mirrored reflections, floor).
//
// World space is in metres, y up. Box shading happens in "box units"
// (body width = 1.0, see mesh.rs); BOX converts between the two.

const BOX: f32 = 0.06;
const MAX_POOLS: u32 = 64u;

struct Gift {
    pos_yaw: vec4f,   // x, z (metres), yaw, emission gain
    led0: vec4f,      // linear HDR rgb
    led1: vec4f,
    led2: vec4f,
    led3: vec4f,
    flags: vec4f,     // online, selected, hovered, dragging
};

struct Globals {
    view_proj: mat4x4f,
    eye: vec4f,          // xyz camera position (m), w = time (s)
    focus: vec4f,        // xyz orbit target, w = gift count
    grid: vec4f,         // x strength, y fade radius (m), z reflection strength, w pool strength
    accent: vec4f,       // linear accent colour, w = fog start distance (m)
    fog: vec4f,          // rgb fog colour (linear), w density (per metre)
    gifts: array<Gift, MAX_POOLS>,
};

@group(0) @binding(0) var<uniform> g: Globals;

// ---------------------------------------------------------------------------
// Material constants (docs/UI_DESIGN.md §5.3)
// ---------------------------------------------------------------------------

const ALBEDO = vec3f(0.78, 0.78, 0.80);
const KEY_DIR = vec3f(-0.4706, 0.8367, 0.4183);   // normalize(-0.45, 0.80, 0.40)
const KEY = vec3f(0.115, 0.118, 0.135);
const RIM = vec3f(0.070, 0.070, 0.090);
const AMBIENT = vec3f(0.022, 0.022, 0.028);
const BODY_H: f32 = 0.86;

fn rot_y(v: vec3f, s: f32, c: f32) -> vec3f {
    return vec3f(c * v.x + s * v.z, v.y, -s * v.x + c * v.z);
}

fn apply_fog(col: vec3f, world: vec3f) -> vec3f {
    let d = max(distance(world, g.eye.xyz) - g.accent.w, 0.0);
    return mix(col, g.fog.rgb, 1.0 - exp(-g.fog.w * d));
}

// ---------------------------------------------------------------------------
// Boxes
// ---------------------------------------------------------------------------

struct VsIn {
    @location(0) pos: vec3f,
    @location(1) part: f32,
    @location(2) normal: vec3f,
    @location(3) pos_yaw: vec4f,
    @location(4) led0: vec4f,
    @location(5) led1: vec4f,
    @location(6) led2: vec4f,
    @location(7) led3: vec4f,
    @location(8) flags: vec4f,
};

struct VsOut {
    @builtin(position) clip: vec4f,
    @location(0) local: vec3f,
    @location(1) n_local: vec3f,
    @location(2) world: vec3f,
    @location(3) v_local: vec3f,
    @location(4) @interpolate(flat) part: f32,
    @location(5) @interpolate(flat) led0: vec4f,
    @location(6) @interpolate(flat) led1: vec4f,
    @location(7) @interpolate(flat) led2: vec4f,
    @location(8) @interpolate(flat) led3: vec4f,
    @location(9) @interpolate(flat) flags: vec4f,
    @location(10) @interpolate(flat) yaw: f32,
};

fn box_vs(in: VsIn, mirror: f32) -> VsOut {
    let s = sin(in.pos_yaw.z);
    let c = cos(in.pos_yaw.z);
    var lp = in.pos;
    // Lift the dragged box slightly so it reads as "picked up".
    let lift = in.flags.w * 0.12;
    let wp = rot_y(lp * BOX, s, c) + vec3f(in.pos_yaw.x, (lift * BOX), in.pos_yaw.y);
    let world = vec3f(wp.x, wp.y * mirror, wp.z);
    var out: VsOut;
    out.clip = g.view_proj * vec4f(world, 1.0);
    out.local = lp;
    out.n_local = in.normal;
    out.world = world;
    // View vector in box-local space (for fresnel and rims). For the mirror
    // image, reflect the eye instead of the geometry.
    let eye = vec3f(g.eye.x, g.eye.y * mirror, g.eye.z);
    let rel = eye - vec3f(in.pos_yaw.x, lift * BOX, in.pos_yaw.y);
    out.v_local = rot_y(rel, -s, c) / BOX - lp;
    out.part = in.part;
    out.led0 = in.led0 * in.pos_yaw.w;
    out.led1 = in.led1 * in.pos_yaw.w;
    out.led2 = in.led2 * in.pos_yaw.w;
    out.led3 = in.led3 * in.pos_yaw.w;
    out.flags = in.flags;
    out.yaw = in.pos_yaw.z;
    return out;
}

@vertex
fn vs_box(in: VsIn) -> VsOut {
    return box_vs(in, 1.0);
}

@vertex
fn vs_reflect(in: VsIn) -> VsOut {
    return box_vs(in, -1.0);
}

const LED_Y: f32 = 0.50;
const LED_XZ: f32 = 0.24;

fn led_glow(p: vec3f, n: vec3f, led: array<vec3f, 4>) -> vec3f {
    let lp = array<vec3f, 4>(
        vec3f(-LED_XZ, LED_Y, -LED_XZ),
        vec3f(LED_XZ, LED_Y, -LED_XZ),
        vec3f(LED_XZ, LED_Y, LED_XZ),
        vec3f(-LED_XZ, LED_Y, LED_XZ),
    );
    var direct = vec3f(0.0);
    for (var i = 0u; i < 4u; i++) {
        let d = p - lp[i];
        // Distance of the shell from the LED along the face normal: only the
        // faces next to an LED get its hot-spot.
        let nd = dot(d, n);
        let near = smoothstep(0.70, 0.30, nd) * step(0.0, nd);
        // Lateral offset on the face, squashed vertically (the spots in the
        // photo are a bit wider than tall).
        let lat = (d - n * nd) * vec3f(1.0, 1.25, 1.0);
        let spot = 0.75 / (1.0 + 70.0 * dot(lat, lat)) * near;
        let broad = 0.09 / (1.0 + 6.0 * dot(d, d));
        direct += led[i] * (spot + broad);
    }
    return direct;
}

fn shade_box(in: VsOut, reflection: bool) -> vec3f {
    let p = in.local;
    let n = normalize(in.n_local);
    let v = normalize(in.v_local);
    let s = sin(in.yaw);
    let c = cos(in.yaw);
    let n_world = rot_y(n, s, c);
    let ndv = saturate(dot(n, v));
    let fres = pow(1.0 - ndv, 3.0);

    let online = in.flags.x;
    let selected = in.flags.y;
    let hovered = in.flags.z;

    let led = array<vec3f, 4>(in.led0.rgb, in.led1.rgb, in.led2.rgb, in.led3.rgb);
    let mean = (led[0] + led[1] + led[2] + led[3]) * 0.25;

    var albedo = ALBEDO;
    var emission = vec3f(0.0);
    let part = u32(in.part + 0.5);
    if (part == 0u) {
        // Body: frosted shell lit from inside.
        let direct = led_glow(p, n, led);
        let scatter = mean * 0.20;
        let h = p.y / BODY_H;
        // PCB + battery shadow the band under the lid; the base is a bit dimmer.
        let profile = mix(1.0, 0.14, smoothstep(0.64, 0.88, h)) * mix(0.78, 1.0, smoothstep(0.0, 0.3, h));
        // Faces glow more where the shell is seen edge-on (longer path through plastic).
        emission = (direct + scatter) * profile * (1.0 + 0.6 * fres);
        // The top face (hidden by the lid) and bottom never matter, but keep them dim.
        emission *= mix(1.0, 0.3, saturate(abs(n.y)));
    } else if (part == 1u) {
        albedo = vec3f(0.86, 0.86, 0.88);
        // Lid: denser, mostly lit through its underside and rim.
        let under = saturate(-n.y);
        let side = 1.0 - abs(n.y);
        emission = mean * (0.035 + 0.07 * side + 0.25 * under) + mean * fres * 0.08;
        // Slight hot-spots near the LEDs bleed into the lid's lower edge.
        emission += led_glow(p, n, led) * 0.06 * side;
    } else if (part == 2u) {
        albedo = vec3f(0.85, 0.85, 0.87);
        emission = mean * 0.06;
    } else {
        albedo = vec3f(0.0);
        emission = vec3f(0.0);
    }

    if (online < 0.5) {
        emission = vec3f(0.0);
        albedo *= 0.55;
    }

    let key = KEY * max(dot(n_world, KEY_DIR), 0.0);
    // Wrapped "sky" fill so every face of an unlit box separates from the dark.
    let fill = AMBIENT * (0.6 + 0.4 * n_world.y);
    var col = albedo * (key + fill) + RIM * pow(1.0 - ndv, 4.0) * select(0.5, 1.0, part != 3u);

    if (reflection) {
        emission *= 0.5;
    }
    col += emission;

    if (!reflection) {
        let accent = g.accent.rgb;
        let edge = pow(1.0 - ndv, 2.5);
        col += accent * edge * (selected * 0.9 + hovered * 0.35);
        col += RIM * hovered * pow(1.0 - ndv, 4.0) * 1.5;
    }
    return col;
}

@fragment
fn fs_box(in: VsOut) -> @location(0) vec4f {
    let col = shade_box(in, false);
    return vec4f(apply_fog(col, in.world), 1.0);
}

@fragment
fn fs_reflect(in: VsOut) -> @location(0) vec4f {
    var col = shade_box(in, true);
    // Fade with depth below the floor (box units).
    let depth = -in.world.y / BOX;
    col *= exp(-2.6 * depth) * g.grid.z;
    return vec4f(apply_fog(col, in.world), 1.0);
}

// ---------------------------------------------------------------------------
// Floor
// ---------------------------------------------------------------------------

struct FloorOut {
    @builtin(position) clip: vec4f,
    @location(0) world: vec3f,
};

const FLOOR_HALF: f32 = 12.0;

@vertex
fn vs_floor(@builtin(vertex_index) vi: u32) -> FloorOut {
    var corners = array<vec2f, 6>(
        vec2f(-1.0, -1.0), vec2f(-1.0, 1.0), vec2f(1.0, 1.0),
        vec2f(-1.0, -1.0), vec2f(1.0, 1.0), vec2f(1.0, -1.0),
    );
    let c = corners[vi] * FLOOR_HALF;
    let world = vec3f(c.x + g.focus.x, 0.0, c.y + g.focus.z);
    var out: FloorOut;
    out.clip = g.view_proj * vec4f(world, 1.0);
    out.world = world;
    return out;
}

const FLOOR_ALBEDO = vec3f(0.0040, 0.0040, 0.0052);

fn grid_line(coord: vec2f, spacing: f32, width_px: f32) -> f32 {
    let q = coord / spacing;
    let fw = max(fwidth(q), vec2f(1e-5));
    let d = abs(fract(q - 0.5) - 0.5) / fw;
    let line = 1.0 - min(min(d.x, d.y) / width_px, 1.0);
    // Fade lines out where they would alias (sub-pixel spacing).
    let anti_alias = 1.0 - smoothstep(0.25, 0.6, max(fw.x, fw.y));
    return line * anti_alias;
}

@fragment
fn fs_floor(in: FloorOut) -> @location(0) vec4f {
    let w = in.world;
    let q = w.xz / BOX; // box units
    var col = FLOOR_ALBEDO * (KEY * KEY_DIR.y + AMBIENT);

    var pool = vec3f(0.0);
    var ring = 0.0;
    var occl = 1.0;
    let count = min(u32(g.focus.w), MAX_POOLS);
    for (var i = 0u; i < count; i++) {
        let gi = g.gifts[i];
        let s = sin(gi.pos_yaw.z);
        let c = cos(gi.pos_yaw.z);
        let rel = q - gi.pos_yaw.xy / BOX;
        // Into box-local coordinates.
        let l = vec2f(c * rel.x - s * rel.y, s * rel.x + c * rel.y);
        let ad = max(abs(l) - vec2f(0.5), vec2f(0.0));
        let d2 = dot(ad, ad);
        let dist = sqrt(d2);
        if (dist > 7.0) {
            continue;
        }
        // Rounded-square ring around the footprint for selection / hover.
        let rd = abs(dist - 0.16);
        let ring_w = 0.035;
        ring += (gi.flags.y * 1.0 + gi.flags.z * 0.45) * (1.0 - smoothstep(ring_w * 0.5, ring_w, rd));

        if (gi.flags.x < 0.5) {
            occl *= 1.0 - 0.5 * exp(-dist * 9.0);
            continue;
        }
        // Each quadrant leans towards its nearest LED; far away the pool is the mean.
        let dir = l / max(length(l), 1e-4);
        let w0 = pow(max(0.0, dot(dir, vec2f(-0.7071, -0.7071)) + 0.6), 2.0);
        let w1 = pow(max(0.0, dot(dir, vec2f(0.7071, -0.7071)) + 0.6), 2.0);
        let w2 = pow(max(0.0, dot(dir, vec2f(0.7071, 0.7071)) + 0.6), 2.0);
        let w3 = pow(max(0.0, dot(dir, vec2f(-0.7071, 0.7071)) + 0.6), 2.0);
        let gain = gi.pos_yaw.w;
        let directional = (gi.led0.rgb * w0 + gi.led1.rgb * w1 + gi.led2.rgb * w2 + gi.led3.rgb * w3) / max(w0 + w1 + w2 + w3, 1e-4);
        let mean = (gi.led0.rgb + gi.led1.rgb + gi.led2.rgb + gi.led3.rgb) * 0.25;
        let tint = mix(directional, mean, smoothstep(0.0, 2.5, dist)) * gain;
        // Tight bright core + broad soft halo.
        let falloff = 0.45 / (1.0 + 8.0 * d2) + 0.035 / (1.0 + 0.8 * d2);
        let contact = 1.0 - 0.55 * exp(-dist * 16.0);
        pool += tint * falloff * smoothstep(5.0, 0.0, dist) * contact;
        occl *= 1.0 - 0.35 * exp(-dist * 9.0);
    }
    col = col * occl + pool * g.grid.w;

    // Grid: 10 cm minor, 50 cm major lines, fading away from the orbit target.
    let gfade = 1.0 - smoothstep(g.grid.y * 0.4, g.grid.y, distance(w.xz, g.focus.xz));
    let grid_col = vec3f(0.026, 0.026, 0.033);
    let minor = grid_line(w.xz, 0.1, 1.0);
    let major = grid_line(w.xz, 0.5, 1.4);
    col += grid_col * (minor * 0.22 + major * 0.35) * g.grid.x * gfade;

    col += g.accent.rgb * ring * 1.4;

    // Glossy floor: the mirrored boxes drawn before show through.
    let alpha = 0.86;
    return vec4f(apply_fog(col, w), alpha);
}
