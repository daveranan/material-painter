// Viewport rendering: environment backdrop, image-based-lit PBR mesh, 2D
// texture view and UV wireframe. The 3D view writes scene-linear HDR; post.wgsl
// tone maps it.

struct Globals {
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    cam_pos: vec4<f32>,
    light: vec4<f32>,    // env rotation (radians), exposure, env intensity, radiance max lod
    params: vec4<f32>,   // view mode, height strength, texel size, texture resolution
    view2d: vec4<f32>,   // zoom (px per uv), pan x, pan y (2D view); parallax depth (3D view)
    size: vec4<f32>,     // viewport width, height, baked AO strength, selection highlight strength
    extra: vec4<f32>,    // use baked normal, has bake, custom map shown (view mode 22), wireframe opacity
    bmin: vec4<f32>,
    bsize: vec4<f32>,
    bg: vec4<f32>,       // background color (linear), background intensity
    bg2: vec4<f32>,      // background blur, background mode (0 environment, 1 color, 2 transparent)
    light_vp: mat4x4<f32>, // key light shadow projection
    sun: vec4<f32>,      // direction toward the key light, intensity
    sun_c: vec4<f32>,    // key light color (linear), on (1) / off (0)
};

@group(0) @binding(0) var<uniform> g: Globals;
@group(0) @binding(1) var t_color: texture_2d<f32>;
@group(0) @binding(2) var t_rough: texture_2d<f32>;
@group(0) @binding(3) var t_metal: texture_2d<f32>;
@group(0) @binding(4) var t_height: texture_2d<f32>;
@group(0) @binding(5) var map_a: texture_2d<f32>;  // AO, curvature, cavity, thickness
@group(0) @binding(6) var map_b: texture_2d<f32>;  // sky, edge dist, crease dist, part random
@group(0) @binding(7) var map_c: texture_2d<f32>;  // bent normal, part gradient
@group(0) @binding(8) var map_d: texture_2d<f32>;  // baked normal xy, height, missed
@group(0) @binding(9) var map_e: texture_2d<f32>;  // ID
@group(0) @binding(19) var custom_maps: texture_2d_array<f32>;  // custom map i: channel i % 4 of layer i / 4
@group(0) @binding(20) var t_nrm: texture_2d<f32>;   // painted normal detail: tangent xy at 0.5 + 0.5 n
@group(0) @binding(21) var t_emis: texture_2d<f32>;  // emission, linear HDR
@group(0) @binding(22) var t_hl: texture_2d<f32>;    // the selected layer's coverage (strength in size.w)
@group(0) @binding(10) var samp: sampler;
@group(0) @binding(11) var t_wnrm: texture_2d<f32>;
@group(0) @binding(12) var t_wpos: texture_2d<f32>;
@group(0) @binding(13) var env_rad: texture_2d<f32>;  // radiance, mip per roughness
@group(0) @binding(14) var env_samp: sampler;
@group(0) @binding(16) var shadow_map: texture_depth_2d;
@group(0) @binding(17) var shadow_samp: sampler_comparison;
@group(0) @binding(15) var<uniform> env_sh: array<vec4<f32>, 9>;
// Parameters of a custom preview shader (a shading graph), 4 floats each.
@group(0) @binding(18) var<uniform> cp: array<vec4<f32>, 128>;

// Live detail (engine::LIVE_SLOTS): tri-planar fill layers re-sampled here at
// full resolution, on top of the texture set that holds them only down to one
// of its texels. Per slot: (tiling, rotation, offset), (sharpness, normal
// strength, normal is DirectX, on), (has color, roughness, normal).
struct LiveSlot {
    img: vec4<f32>,
    tri: vec4<f32>,
    has: vec4<f32>,
};
@group(0) @binding(23) var live_samp: sampler;
@group(0) @binding(24) var live_w: texture_2d<f32>;  // per slot: how much of the layer shows
@group(0) @binding(25) var<uniform> live: array<LiveSlot, 3>;
@group(0) @binding(26) var live0_c: texture_2d<f32>;
@group(0) @binding(27) var live0_r: texture_2d<f32>;
@group(0) @binding(28) var live0_n: texture_2d<f32>;
@group(0) @binding(29) var live1_c: texture_2d<f32>;
@group(0) @binding(30) var live1_r: texture_2d<f32>;
@group(0) @binding(31) var live1_n: texture_2d<f32>;
@group(0) @binding(32) var live2_c: texture_2d<f32>;
@group(0) @binding(33) var live2_r: texture_2d<f32>;
@group(0) @binding(34) var live2_n: texture_2d<f32>;

const PI: f32 = 3.14159265;

fn rot_y(d: vec3<f32>, a: f32) -> vec3<f32> {
    let c = cos(a);
    let s = sin(a);
    return vec3<f32>(c * d.x + s * d.z, d.y, -s * d.x + c * d.z);
}

// Prefiltered environment radiance for a reflection of the given roughness.
fn env(d: vec3<f32>, rough: f32) -> vec3<f32> {
    let uv = env_dir_to_uv(rot_y(d, -g.light.x));
    return textureSampleLevel(env_rad, env_samp, uv, clamp(rough, 0.0, 1.0) * g.light.w).rgb * g.light.z;
}

// Diffuse irradiance arriving at a surface facing n.
fn irradiance(n: vec3<f32>) -> vec3<f32> {
    return sh_irradiance(env_sh, rot_y(n, -g.light.x)) * g.light.z;
}

fn aces(x: vec3<f32>) -> vec3<f32> {
    let a = 2.51;
    let b = 0.03;
    let c = 2.43;
    let d = 0.59;
    let e = 0.14;
    return clamp((x * (a * x + b)) / (x * (c * x + d) + e), vec3<f32>(0.0), vec3<f32>(1.0));
}

// Karis' analytic approximation of the split-sum environment BRDF.
// Scale and bias to F0 of the split-sum environment BRDF (Karis's fit).
fn env_ab(rough: f32, nv: f32) -> vec2<f32> {
    let c0 = vec4<f32>(-1.0, -0.0275, -0.572, 0.022);
    let c1 = vec4<f32>(1.0, 0.0425, 1.04, -0.04);
    let r = rough * c0 + c1;
    let a004 = min(r.x * r.x, exp2(-9.28 * nv)) * r.x + r.y;
    return vec2<f32>(-1.04, 1.04) * a004 + r.zw;
}

fn env_brdf(f0: vec3<f32>, rough: f32, nv: f32) -> vec3<f32> {
    let ab = env_ab(rough, nv);
    return f0 * ab.x + ab.y;
}

struct Surface {
    albedo: vec3<f32>,
    rough: f32,
    metal: f32,
    height: f32,
    // Final tangent-space normal (OpenGL convention): baked normal + painted height detail.
    ts: vec3<f32>,
    a: vec4<f32>,
    b: vec4<f32>,
    c: vec4<f32>,
    d: vec4<f32>,
    e: vec4<f32>,
    f: vec4<f32>,
    wnrm: vec3<f32>,
    wpos: vec3<f32>,
    // Emitted light (linear), added on top of the shading.
    emis: vec3<f32>,
};

// Deepest mip level of the painted channels (`VIEW_MIPS` - 1 in engine.rs).
const VIEW_MAX_LOD: f32 = 4.0;
// Footprint scale for the painted channels: below 1 is a negative mip bias
// (0.6 = -0.74 levels), keeping panel lines crisp at a distance without the
// shimmer of sampling level 0.
const VIEW_LOD_SCALE: f32 = 0.6;

// Called from uniform control flow only (it takes screen-space derivatives).
fn sample_surface(uv: vec2<f32>) -> Surface {
    var s: Surface;
    // The pixel's footprint in UV: the painted channels are mipmapped and
    // sampled anisotropically over it, so fine detail doesn't shimmer.
    let ddx = dpdx(uv) * VIEW_LOD_SCALE;
    let ddy = dpdy(uv) * VIEW_LOD_SCALE;
    s.albedo = textureSampleGrad(t_color, samp, uv, ddx, ddy).rgb;
    s.rough = textureSampleGrad(t_rough, samp, uv, ddx, ddy).r;
    s.metal = textureSampleGrad(t_metal, samp, uv, ddx, ddy).r;
    s.height = textureSampleGrad(t_height, samp, uv, ddx, ddy).r;
    s.emis = textureSampleGrad(t_emis, samp, uv, ddx, ddy).rgb;
    let pxy = textureSampleGrad(t_nrm, samp, uv, ddx, ddy).rg * 2.0 - 1.0;
    let painted = vec3<f32>(pxy, sqrt(max(1.0 - dot(pxy, pxy), 0.0)));
    s.a = textureSampleLevel(map_a, samp, uv, 0.0);
    s.b = textureSampleLevel(map_b, samp, uv, 0.0);
    s.c = textureSampleLevel(map_c, samp, uv, 0.0);
    s.d = textureSampleLevel(map_d, samp, uv, 0.0);
    s.e = textureSampleLevel(map_e, samp, uv, 0.0);
    let ci = max(i32(g.extra.z), 0);
    s.f = vec4<f32>(textureSampleLevel(custom_maps, samp, uv, ci / 4, 0.0)[ci % 4]);
    s.wnrm = textureSampleLevel(t_wnrm, samp, uv, 0.0).xyz * 2.0 - 1.0;
    s.wpos = textureSampleLevel(t_wpos, samp, uv, 0.0).xyz;
    // Height slope over the pixel's footprint (at least a texel), from the
    // matching mip level: one-texel differences under a pixel that covers
    // many texels turn thin grooves into flickering bright and dark lines.
    let texels = max(length(ddx), length(ddy)) * g.params.w;
    let lod = clamp(log2(max(texels, 1.0)), 0.0, VIEW_MAX_LOD);
    let step = exp2(lod);
    let tx = g.params.z * step;
    let hl = textureSampleLevel(t_height, samp, uv - vec2<f32>(tx, 0.0), lod).r;
    let hr = textureSampleLevel(t_height, samp, uv + vec2<f32>(tx, 0.0), lod).r;
    let hd = textureSampleLevel(t_height, samp, uv - vec2<f32>(0.0, tx), lod).r;
    let hb = textureSampleLevel(t_height, samp, uv + vec2<f32>(0.0, tx), lod).r;
    // Height slope per texel, scaled so the look is resolution-independent.
    let k = g.params.y * g.params.w / 256.0 / step;
    let hu = (hr - hl) * 0.5 * k;
    let ht = (hb - hd) * 0.5 * k;
    var base = vec3<f32>(0.0, 0.0, 1.0);
    if (g.extra.x > 0.5) {
        let bx = s.d.x;
        let by = s.d.y;
        base = vec3<f32>(bx, by, sqrt(max(1.0 - bx * bx - by * by, 0.0)));
    }
    // Whiteout blend of baked normal, painted normal detail and height detail.
    s.ts = normalize(vec3<f32>(base.xy + painted.xy + vec2<f32>(-hu, ht), base.z * painted.z));
    return s;
}

fn live_rot(q: vec2<f32>, deg: f32) -> vec2<f32> {
    let a = radians(deg);
    let c = cos(a);
    let s = sin(a);
    return vec2<f32>(q.x * c - q.y * s, q.x * s + q.y * c);
}

fn live_blend_w(n: vec3<f32>, sharp: f32) -> vec3<f32> {
    let w = pow(abs(n), vec3<f32>(max(sharp, 1.0)));
    return w / max(w.x + w.y + w.z, 1e-4);
}

// Tri-planar image sample, matching composite.wgsl's fill_image.
fn live_tri(t: texture_2d<f32>, p: vec3<f32>, n: vec3<f32>, s: LiveSlot, lod: f32) -> vec4<f32> {
    let w = live_blend_w(n, s.tri.x);
    let q = p * max(s.img.x, 1e-3);
    return textureSampleLevel(t, live_samp, live_rot(vec2<f32>(q.z, -q.y), s.img.y) + s.img.zw, lod) * w.x
        + textureSampleLevel(t, live_samp, live_rot(q.xz, s.img.y) + s.img.zw, lod) * w.y
        + textureSampleLevel(t, live_samp, live_rot(vec2<f32>(q.x, -q.y), s.img.y) + s.img.zw, lod) * w.z;
}

fn live_plane_n(t: texture_2d<f32>, q: vec2<f32>, lod: f32, s: LiveSlot, r: vec3<f32>, up: vec3<f32>, f: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
    var m = textureSampleLevel(t, live_samp, live_rot(q, s.img.y) + s.img.zw, lod).xyz * 2.0 - 1.0;
    if (s.tri.z > 0.5) {
        m.y = -m.y;
    }
    let xy = live_rot(m.xy * s.tri.y, s.img.y);
    // Whiteout onto the surface normal, as composite.wgsl's planar_normal.
    let ln = vec3<f32>(dot(n, r), dot(n, up), abs(dot(n, f)));
    let o = vec3<f32>(xy + ln.xy, max(m.z, 1e-3) * ln.z);
    return r * o.x + up * o.y + f * o.z;
}

// World-space tri-planar normal, matching composite.wgsl's fill_normal.
fn live_tri_n(t: texture_2d<f32>, p: vec3<f32>, n: vec3<f32>, s: LiveSlot, lod: f32) -> vec3<f32> {
    let w = live_blend_w(n, s.tri.x);
    let q = p * max(s.img.x, 1e-3);
    let sg = sign(n + vec3<f32>(1e-6));
    return live_plane_n(t, vec2<f32>(q.z, -q.y), lod, s, vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(sg.x, 0.0, 0.0), n) * w.x
        + live_plane_n(t, q.xz, lod, s, vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 0.0, -1.0), vec3<f32>(0.0, sg.y, 0.0), n) * w.y
        + live_plane_n(t, vec2<f32>(q.x, -q.y), lod, s, vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(0.0, 0.0, sg.z), n) * w.z;
}

// Mip levels of an image for this pixel (x) and for one texel of the texture
// set (y, what the composite sampled): the detail between them is missing.
fn live_lods(t: texture_2d<f32>, s: LiveSlot, texel_world: f32, pixel_world: f32) -> vec2<f32> {
    let k = max(s.img.x, 1e-3) * f32(textureDimensions(t).x);
    let lo = log2(max(texel_world * k, 1.0));
    return vec2<f32>(clamp(log2(max(pixel_world * k, 1e-4)), 0.0, lo), lo);
}

// What one live slot adds: color factor - 1 (linear), roughness and world normal offsets.
struct LiveAdd {
    col: vec3<f32>,
    rough: f32,
    nrm: vec3<f32>,
};

fn live_add(tc: texture_2d<f32>, tr: texture_2d<f32>, tn: texture_2d<f32>, s: LiveSlot, w: f32, p: vec3<f32>, n: vec3<f32>, tw: f32, pw: f32) -> LiveAdd {
    var a: LiveAdd;
    a.col = vec3<f32>(0.0);
    a.rough = 0.0;
    a.nrm = vec3<f32>(0.0);
    if (s.tri.w < 0.5 || w < 0.002) {
        return a;
    }
    let lc = live_lods(tc, s, tw, pw);
    if (lc.y - lc.x > 0.05) {
        let hi = pow(live_tri(tc, p, n, s, lc.x).rgb, vec3<f32>(2.2));
        let lo = pow(live_tri(tc, p, n, s, lc.y).rgb, vec3<f32>(2.2));
        a.col = w * (clamp(hi / max(lo, vec3<f32>(0.01)), vec3<f32>(0.0), vec3<f32>(4.0)) - 1.0);
    }
    if (s.has.y > 0.5) {
        let lr = live_lods(tr, s, tw, pw);
        if (lr.y - lr.x > 0.05) {
            a.rough = w * (live_tri(tr, p, n, s, lr.x).r - live_tri(tr, p, n, s, lr.y).r);
        }
    }
    if (s.has.z > 0.5) {
        let ln = live_lods(tn, s, tw, pw);
        if (ln.y - ln.x > 0.05) {
            a.nrm = w * (live_tri_n(tn, p, n, s, ln.x) - live_tri_n(tn, p, n, s, ln.y));
        }
    }
    return a;
}

fn hue(h: f32) -> vec3<f32> {
    let k = vec3<f32>(1.0, 2.0 / 3.0, 1.0 / 3.0);
    let p = abs(fract(vec3<f32>(h) + k) * 6.0 - 3.0);
    return clamp(p - 1.0, vec3<f32>(0.0), vec3<f32>(1.0));
}

fn lin(v: f32) -> vec3<f32> {
    return vec3<f32>(v * v);
}

// How much of the key light reaches world point p (geometric normal gn):
// 3x3 filtered lookups in the shadow map, offset along the normal so
// surfaces don't shadow themselves.
fn key_shadow(p: vec3<f32>, gn: vec3<f32>) -> f32 {
    let size = f32(textureDimensions(shadow_map).x);
    let l = normalize(g.sun.xyz);
    let slope = 1.0 - clamp(dot(gn, l), 0.0, 1.0);
    let q = g.light_vp * vec4<f32>(p + gn * (0.004 + 0.01 * slope), 1.0);
    let ndc = q.xyz / q.w;
    let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    if (any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0)) || ndc.z > 1.0) {
        return 1.0;
    }
    var lit = 0.0;
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            let o = vec2<f32>(f32(x), f32(y)) * 1.5 / size;
            lit += textureSampleCompareLevel(shadow_map, shadow_samp, uv + o, ndc.z);
        }
    }
    return lit / 9.0;
}

// Key light: GGX specular and Lambert diffuse from a directional light.
fn key_light(s: Surface, n: vec3<f32>, v: vec3<f32>, shadow: f32) -> vec3<f32> {
    if (g.sun_c.w < 0.5 || shadow <= 0.0) {
        return vec3<f32>(0.0);
    }
    let l = normalize(g.sun.xyz);
    let nl = dot(n, l);
    if (nl <= 0.0) {
        return vec3<f32>(0.0);
    }
    let nv = max(dot(n, v), 1e-4);
    let h = normalize(l + v);
    let nh = max(dot(n, h), 0.0);
    let vh = max(dot(v, h), 0.0);
    let rough = clamp(s.rough, 0.03, 1.0);
    let a = rough * rough;
    let a2 = a * a;
    let dd = nh * nh * (a2 - 1.0) + 1.0;
    let d = a2 / (PI * dd * dd);
    let k = a * 0.5;
    let vis = 1.0 / ((nv * (1.0 - k) + k) * (nl * (1.0 - k) + k) * 4.0);
    let f0 = mix(vec3<f32>(0.04), s.albedo, s.metal);
    let f = f0 + (1.0 - f0) * pow(1.0 - vh, 5.0);
    let diffuse = s.albedo * (1.0 - s.metal) * (1.0 - f) / PI;
    return (diffuse + d * vis * f) * g.sun_c.rgb * g.sun.w * nl * shadow;
}

fn shade(s: Surface, n: vec3<f32>, v: vec3<f32>, shadow: f32) -> vec3<f32> {
    let mode = i32(g.params.x);
    switch mode {
        case 1: { return s.albedo; }
        case 2: { return lin(s.rough); }
        case 3: { return lin(s.metal); }
        case 4: { return lin(clamp(s.height * 0.5 + 0.5, 0.0, 1.0)); }
        case 5: { let c = s.ts * 0.5 + 0.5; return c * c; }
        case 6: { return lin(s.a.r); }
        case 7: { return lin(s.a.g); }
        case 8: { return lin(s.a.b); }
        case 9: { return lin(s.a.a); }
        case 10: { return lin(s.b.r); }
        case 11: { return lin(s.b.g); }
        case 12: { return lin(s.b.b); }
        case 13: { let c = hue(s.b.a) * 0.8 + 0.1; return c * c; }
        case 14: { return lin(s.c.a); }
        case 15: { return s.c.rgb * s.c.rgb; }
        case 16: {
            let c = vec3<f32>(s.d.x, s.d.y, sqrt(max(1.0 - s.d.x * s.d.x - s.d.y * s.d.y, 0.0))) * 0.5 + 0.5;
            return c * c;
        }
        case 17: { return lin(s.d.z); }
        case 18: { return s.e.rgb * s.e.rgb; }
        case 19: { let c = s.wnrm * 0.5 + 0.5; return c * c; }
        case 20: { let c = (s.wpos - g.bmin.xyz) / max(g.bsize.xyz, vec3<f32>(1e-4)); return c * c; }
        case 21: {
            let base = lin(0.25 + 0.5 * s.a.r);
            return mix(base, vec3<f32>(1.0, 0.02, 0.02), s.d.w);
        }
        case 22: { return lin(s.f.r); }
        case 23: { return s.emis; }
        default: {}
    }
    return shade_pbr(s, n, v, shadow);
}

// The built-in physically based look: image-based light plus the key light.
fn shade_pbr(s: Surface, n: vec3<f32>, v: vec3<f32>, shadow: f32) -> vec3<f32> {
    let rough = clamp(s.rough, 0.03, 1.0);
    let a = rough * rough;
    let f0 = mix(vec3<f32>(0.04), s.albedo, s.metal);
    let diffuse_c = s.albedo * (1.0 - s.metal);
    let nv = max(dot(n, v), 1e-4);

    // All light comes from the environment: SH irradiance for diffuse, the
    // roughness-prefiltered radiance for specular. The baked AO reaches far
    // (it's sky occlusion), so at full strength it darkens whole crevices,
    // not just contact lines; the view applies it at the user's strength.
    let ao = mix(1.0, s.a.r, clamp(g.size.z, 0.0, 1.0));
    let ab = env_ab(rough, nv);
    let spec = f0 * ab.x + ab.y;
    // Multiple scattering (Fdez-Aguera 2019): single-scatter GGX loses the
    // light that bounces more than once inside rough metal; Cycles, Unreal
    // and Substance keep it. Without this, rough steel shades ~8% too dark.
    // The cap keeps the analytic fit (which underestimates A + B when very
    // rough) from overshooting: checked against Cycles on a sphere chart
    // (tools/parity/chart.py).
    let ms = min(1.0 + f0 * (1.0 / max(ab.x + ab.y, 1e-3) - 1.0), 1.0 + 0.5 * f0);
    var col = diffuse_c * (1.0 - spec) * irradiance(n) / PI * ao;
    let r = reflect(-v, n);
    let spec_ao = clamp(pow(nv + ao, exp2(-16.0 * a - 1.0)) - 1.0 + ao, 0.0, 1.0);
    col += env(r, rough) * spec * ms * spec_ao;
    // Baked AO darkens the key light only a little: it's mostly sky occlusion.
    col += key_light(s, n, v, shadow) * mix(1.0, ao, 0.35);
    return col + s.emis;
}

// ---------------------------------------------------------------- backdrop

@fragment
fn fs_sky(in: FsOut) -> @location(0) vec4<f32> {
    let ndc = vec2<f32>(in.uv.x * 2.0 - 1.0, 1.0 - in.uv.y * 2.0);
    let far = g.inv_view_proj * vec4<f32>(ndc, 1.0, 1.0);
    let d = normalize(far.xyz / far.w - g.cam_pos.xyz);
    if (g.bg2.y > 0.5) {
        return vec4<f32>(g.bg.rgb, 1.0);
    }
    return vec4<f32>(env(d, g.bg2.x) * g.bg.w, 1.0);
}

// ---------------------------------------------------------------- mesh

struct VIn {
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) tangent: vec4<f32>,
};

struct MeshOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) wpos: vec3<f32>,
    @location(1) n: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) t: vec4<f32>,
};

@vertex
fn vs_mesh(v: VIn) -> MeshOut {
    var out: MeshOut;
    out.clip = g.view_proj * vec4<f32>(v.pos, 1.0);
    out.wpos = v.pos;
    out.n = v.normal;
    out.uv = v.uv;
    out.t = v.tangent;
    return out;
}

// Everything a shader sees at one point of the mesh; custom preview shaders
// (shading graphs) read their inputs from it.
struct Shade {
    s: Surface,
    // Shading normal (baked normal and painted height applied), the
    // geometric normal, and the direction toward the camera.
    n: vec3<f32>,
    gn: vec3<f32>,
    v: vec3<f32>,
    // Toward the key light, and its color times intensity (zero when off).
    l: vec3<f32>,
    light: vec3<f32>,
    shadow: f32,
    uv: vec2<f32>,
    wpos: vec3<f32>,
};

// Parallax occlusion (`g.view2d.w`: world height of painted height 1): the
// surface looks displaced without moving the mesh. The view ray steps down
// through the height field; the first step below the surface gives the UV
// to shade. UV and world are related through screen derivatives, so no
// tangent convention is assumed.
fn parallax_uv(in: MeshOut, front: bool) -> vec2<f32> {
    let depth = g.view2d.w;
    if (depth <= 0.0) {
        return in.uv;
    }
    var n = normalize(in.n);
    if (!front) {
        n = -n;
    }
    // World directions of +u and +v (per UV unit) from the derivatives.
    let duv_x = dpdx(in.uv);
    let duv_y = dpdy(in.uv);
    let dp_x = dpdx(in.wpos);
    let dp_y = dpdy(in.wpos);
    let det = duv_x.x * duv_y.y - duv_x.y * duv_y.x;
    if (abs(det) < 1e-12) {
        return in.uv;
    }
    let tu = (dp_x * duv_y.y - dp_y * duv_x.y) / det;
    let tv = (dp_y * duv_x.x - dp_x * duv_y.x) / det;
    let v = normalize(g.cam_pos.xyz - in.wpos);
    let vn = max(dot(v, n), 0.15);
    // World step along the surface per unit of height, toward the camera,
    // turned into UV through the dual of (tu, tv).
    let w = (v - n * dot(v, n)) / vn;
    let a = dot(tu, tu);
    let b = dot(tu, tv);
    let d = dot(tv, tv);
    let gdet = max(a * d - b * b, 1e-12);
    let wu = dot(w, tu);
    let wv = dot(w, tv);
    let per_h = vec2<f32>(d * wu - b * wv, a * wv - b * wu) / gdet;
    // Mip that fits the pixel, so the march doesn't shimmer when far away.
    let texels = max(length(duv_x), length(duv_y)) * g.params.w;
    let lod = clamp(log2(max(texels, 1.0)), 0.0, VIEW_MAX_LOD);
    const STEPS: i32 = 32;
    var hl = depth;
    var prev_gap = 0.0;
    var prev_h = depth;
    for (var i = 0; i <= STEPS; i++) {
        hl = depth - 2.0 * depth * f32(i) / f32(STEPS);
        let uv = in.uv + per_h * hl;
        let surf = textureSampleLevel(t_height, samp, uv, lod).r * depth;
        let gap = hl - surf;
        if (gap <= 0.0) {
            // Between the last step above and this one below the surface.
            let t = select(prev_gap / (prev_gap - gap), 0.0, i == 0);
            return in.uv + per_h * mix(prev_h, hl, t);
        }
        prev_gap = gap;
        prev_h = hl;
    }
    return in.uv + per_h * hl;
}

fn mesh_shade_inputs(in: MeshOut, front: bool) -> Shade {
    var c: Shade;
    let suv = parallax_uv(in, front);
    c.s = sample_surface(suv);
    // Live detail. World size of one texture-set texel and of this pixel
    // (with the painted channels' sharpening bias), from screen derivatives.
    let dpx = dpdx(in.wpos);
    let dpy = dpdy(in.wpos);
    let duv = (length(dpdx(in.uv)) + length(dpdy(in.uv))) * g.params.w;
    let tw = (length(dpx) + length(dpy)) / max(duv, 1e-6);
    let pw = max(length(dpx), length(dpy)) * VIEW_LOD_SCALE;
    let lw = textureSampleLevel(live_w, samp, in.uv, 0.0).rgb;
    let mn = normalize(in.n);
    let l0 = live_add(live0_c, live0_r, live0_n, live[0], lw.x, in.wpos, mn, tw, pw);
    let l1 = live_add(live1_c, live1_r, live1_n, live[1], lw.y, in.wpos, mn, tw, pw);
    let l2 = live_add(live2_c, live2_r, live2_n, live[2], lw.z, in.wpos, mn, tw, pw);
    c.s.albedo = clamp(c.s.albedo * (1.0 + l0.col + l1.col + l2.col), vec3<f32>(0.0), vec3<f32>(1.0));
    c.s.rough = clamp(c.s.rough + l0.rough + l1.rough + l2.rough, 0.0, 1.0);
    let live_dn = l0.nrm + l1.nrm + l2.nrm;
    var rn = in.n;
    if (!front) {
        rn = -rn;
    }
    c.gn = normalize(rn);
    // MikkTSpace decoding: the interpolated normal and tangent as they are
    // (not normalized or made orthogonal), the bitangent from their cross
    // product, normalized only after combining. The baker inverts exactly
    // this, and so do Unity, Unreal, Godot and Blender.
    let b_up = -cross(rn, in.t.xyz) * in.t.w;
    c.n = normalize(in.t.xyz * c.s.ts.x + b_up * c.s.ts.y + rn * c.s.ts.z);
    c.n = normalize(c.n + select(live_dn, -live_dn, !front));
    c.v = normalize(g.cam_pos.xyz - in.wpos);
    c.l = normalize(g.sun.xyz);
    c.light = g.sun_c.rgb * g.sun.w * g.sun_c.w;
    c.shadow = 1.0;
    if (g.sun_c.w > 0.5) {
        c.shadow = key_shadow(in.wpos, c.gn);
    }
    c.uv = suv;
    c.wpos = in.wpos;
    return c;
}

@fragment
fn fs_mesh(in: MeshOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    let c = mesh_shade_inputs(in, front);
    var col = shade(c.s, c.n, c.v, c.shadow);
    if (g.size.w > 0.0) {
        // Where the selected layer shows: tinted, with the rest dimmed.
        let m = textureSampleLevel(t_hl, samp, in.uv, 0.0).r;
        let tint = vec3<f32>(1.0, 0.55, 0.1);
        col = mix(col, mix(col * 0.35, tint * (0.25 + dot(col, vec3<f32>(0.3, 0.6, 0.1))), m), g.size.w);
    }
    return vec4<f32>(col, 1.0);
}

// ---------------------------------------------------------------- 2D view

struct QuadOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

fn uv_to_clip(uv: vec2<f32>) -> vec4<f32> {
    let screen = g.size.xy * 0.5 + g.view2d.yz + (uv - 0.5) * g.view2d.x;
    let ndc = screen / g.size.xy * 2.0 - 1.0;
    return vec4<f32>(ndc.x, -ndc.y, 0.0, 1.0);
}

@vertex
fn vs_quad(@builtin(vertex_index) vi: u32) -> QuadOut {
    let corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0), vec2<f32>(0.0, 1.0)
    );
    var out: QuadOut;
    out.uv = corners[vi];
    out.clip = uv_to_clip(out.uv);
    return out;
}

@fragment
fn fs_quad(in: QuadOut) -> @location(0) vec4<f32> {
    let s = sample_surface(in.uv);
    let pn = s.ts;
    // Lit by the environment with the camera looking straight at the texture.
    // The 2D view has no post pass, so it tone maps here.
    // The 2D view's normals are in texture space: no key light there.
    var lit = shade(s, pn, vec3<f32>(0.0, 0.0, 1.0), 0.0);
    if (i32(g.params.x) == 0) {
        lit = aces(lit * g.light.y);
    }
    return vec4<f32>(lit, 1.0);
}

@vertex
fn vs_wire(v: VIn) -> QuadOut {
    var out: QuadOut;
    out.uv = v.uv;
    out.clip = uv_to_clip(v.uv);
    return out;
}

// UV wireframe (opacity in extra.w).
@fragment
fn fs_wire(in: QuadOut) -> @location(0) vec4<f32> {
    return vec4<f32>(0.92, 0.95, 1.0, g.extra.w);
}

// UV seams: island borders, bright orange.
@fragment
fn fs_seam(in: QuadOut) -> @location(0) vec4<f32> {
    return vec4<f32>(1.0, 0.62, 0.15, 0.95);
}

// UV seams over the shaded 3D view, a little nearer the eye than the wireframe.
@vertex
fn vs_seam3d(v: VIn) -> OverlayOut {
    var out: OverlayOut;
    out.clip = toward_eye(v.pos, 0.002);
    out.color = vec4<f32>(1.0, 0.62, 0.15, 0.95);
    return out;
}

// ------------------------------------------------------------------ shadow map

@vertex
fn vs_shadow(v: VIn) -> @builtin(position) vec4<f32> {
    return g.light_vp * vec4<f32>(v.pos, 1.0);
}

// ------------------------------------------------------------------ 3D overlays

struct OverlayOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
};

// Pulls a surface point a little toward the camera so lines and highlights
// lying on the mesh win the depth test against it.
fn toward_eye(p: vec3<f32>, k: f32) -> vec4<f32> {
    return g.view_proj * vec4<f32>(p + (g.cam_pos.xyz - p) * k, 1.0);
}

// Mesh wireframe over the shaded 3D view (opacity in extra.w).
@vertex
fn vs_wire3d(v: VIn) -> OverlayOut {
    var out: OverlayOut;
    out.clip = toward_eye(v.pos, 0.0015);
    out.color = vec4<f32>(0.75, 0.85, 1.0, g.extra.w);
    return out;
}

struct OverlayIn {
    @location(0) pos: vec3<f32>,
    // sRGB color, straight alpha.
    @location(1) color: vec4<f32>,
};

// Colored triangles and lines: bake cage, polygon fill selection.
@vertex
fn vs_overlay(v: OverlayIn) -> OverlayOut {
    var out: OverlayOut;
    out.clip = toward_eye(v.pos, 0.001);
    out.color = vec4<f32>(pow(v.color.rgb, vec3<f32>(2.2)), v.color.a);
    return out;
}

@fragment
fn fs_overlay(in: OverlayOut) -> @location(0) vec4<f32> {
    return in.color;
}
