// Terrain blend preview: up to four feature materials (exported
// ColorHeight + NormalMasks, or the open document's) on a heightmap, laid
// out by the game's splat pattern and blended by the game's height rule,
// with anti-tiling. A preview of how the materials meet, not the painter's
// PBR view: a sun, a sky, fog and a tonemap.

struct U {
    view_proj: mat4x4<f32>,
    eye: vec4<f32>,
    // xyz: direction to the sun; w: its intensity.
    sun: vec4<f32>,
    // footprint (m), amplitude (m), grid cells per side, splat scale.
    ground: vec4<f32>,
    // blend depth (the game's look.y), anti-tiling on, show weights, exposure.
    blend: vec4<f32>,
    // Metres per repeat of each slot's material.
    tiles: vec4<f32>,
    // 1 where a slot holds a material.
    present: vec4<f32>,
    // Heightmap texels per side, then unused.
    info: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var height_tex: texture_2d<f32>;
@group(0) @binding(2) var color_tex: texture_2d_array<f32>;
@group(0) @binding(3) var data_tex: texture_2d_array<f32>;
@group(0) @binding(4) var clamp_samp: sampler;
@group(0) @binding(5) var repeat_samp: sampler;

// ---------------------------------------------------------------- ground

fn ground_uv(xz: vec2<f32>) -> vec2<f32> {
    return xz / u.ground.x + 0.5;
}

fn ground_h(xz: vec2<f32>) -> f32 {
    return textureSampleLevel(height_tex, clamp_samp, ground_uv(xz), 0.0).r * u.ground.y;
}

// Height gradient (rise over run) by central differences, one texel apart.
fn ground_grad(xz: vec2<f32>) -> vec2<f32> {
    let e = u.ground.x / u.info.x;
    let dx = ground_h(xz + vec2<f32>(e, 0.0)) - ground_h(xz - vec2<f32>(e, 0.0));
    let dz = ground_h(xz + vec2<f32>(0.0, e)) - ground_h(xz - vec2<f32>(0.0, e));
    return vec2<f32>(dx, dz) / (2.0 * e);
}

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) world: vec3<f32>,
};

// A grid of `ground.z` cells per side from the vertex index alone (six
// vertices per cell), lifted by the heightmap.
@vertex
fn vs(@builtin(vertex_index) vi: u32) -> VOut {
    let n = u32(u.ground.z);
    let cell = vi / 6u;
    let corner = array<vec2<u32>, 6>(vec2<u32>(0u, 0u), vec2<u32>(0u, 1u), vec2<u32>(1u, 0u), vec2<u32>(1u, 0u), vec2<u32>(0u, 1u), vec2<u32>(1u, 1u))[vi % 6u];
    let g = vec2<u32>(cell % n, cell / n) + corner;
    let xz = (vec2<f32>(g) / f32(n) - 0.5) * u.ground.x;
    let world = vec3<f32>(xz.x, ground_h(xz), xz.y);
    var o: VOut;
    o.pos = u.view_proj * vec4<f32>(world, 1.0);
    o.world = world;
    return o;
}

// ---------------------------------------------------------------- the game's splat

// The game's value noise (sim::terrain::value_noise), bit for bit in i32.
fn game_hash(a: i32, b: i32, seed: i32) -> f32 {
    var q = (a * 374761393) ^ (b * 668265263) ^ (seed * 1274126177);
    q = (q ^ i32(u32(q) >> 13u)) * 1103515245;
    return f32(u32(q ^ i32(u32(q) >> 16u))) / 4294967296.0;
}

fn game_value_noise(x: f32, z: f32, seed: i32) -> f32 {
    let i = i32(floor(x));
    let j = i32(floor(z));
    let fx = x - floor(x);
    let fz = z - floor(z);
    let uu = fx * fx * (3.0 - 2.0 * fx);
    let vv = fz * fz * (3.0 - 2.0 * fz);
    let a = game_hash(i, j, seed);
    let b = game_hash(i + 1, j, seed);
    let c = game_hash(i, j + 1, seed);
    let d = game_hash(i + 1, j + 1, seed);
    return a + (b - a) * uu + (c - a) * vv + (a - b - c + d) * uu * vv;
}

fn game_fbm(x: f32, z: f32, seed: i32) -> f32 {
    var sum = 0.0;
    var amp = 0.5;
    var freq = 1.0;
    for (var o = 0; o < 4; o++) {
        sum += amp * game_value_noise(x * freq, z * freq, seed + o * 31);
        amp *= 0.5;
        freq *= 2.03;
    }
    return sum / 0.9375;
}

// The game's temperate pattern (view::splat::pattern): soil, dry drifts,
// mud patches, rock on slopes and outcrops, as slots 0-3.
fn splat(xz: vec2<f32>, slope: f32) -> vec4<f32> {
    let p = xz / u.ground.w;
    let dry = smoothstep(0.44, 0.58, game_fbm(p.x / 22.0, p.y / 22.0, 11));
    let mud = smoothstep(0.6, 0.7, game_fbm(p.x / 11.0 + 40.0, p.y / 11.0, 23));
    let outcrop = smoothstep(0.68, 0.76, game_fbm(p.x / 16.0 - 70.0, p.y / 16.0 + 9.0, 37));
    let rock = max(outcrop, smoothstep(0.3, 0.55, slope));
    let rest = 1.0 - rock;
    let soil = rest * (1.0 - mud);
    var w = vec4<f32>(soil * (1.0 - dry), soil * dry, rest * mud, rock);
    // An empty slot's share goes to slot 0 (or the first filled one).
    var spare = 0.0;
    for (var k = 1; k < 4; k++) {
        if (u.present[k] < 0.5) {
            spare += w[k];
            w[k] = 0.0;
        }
    }
    w.x += spare;
    if (u.present.x < 0.5) {
        let moved = w.x;
        w.x = 0.0;
        for (var k = 1; k < 4; k++) {
            if (u.present[k] > 0.5) {
                w[k] += moved;
                break;
            }
        }
    }
    return w;
}

// ---------------------------------------------------------------- materials

struct Mat {
    color: vec3<f32>,
    height: f32,
    // Tangent normal xy (DirectX: +y along +v, which is +z here).
    nxy: vec2<f32>,
};

fn read_mat(k: i32, uv: vec2<f32>, dx: vec2<f32>, dy: vec2<f32>) -> Mat {
    let c = textureSampleGrad(color_tex, repeat_samp, uv, k, dx, dy);
    let d = textureSampleGrad(data_tex, repeat_samp, uv, k, dx, dy);
    return Mat(c.rgb, c.a, d.rg * 2.0 - 1.0);
}

// One slot's material here. With anti-tiling, a second read turned and
// shifted takes over where a broad noise field says so, and the two meet
// along their own heights (stones stay whole) rather than fading.
fn sample_mat(k: i32, xz: vec2<f32>, gx: vec2<f32>, gy: vec2<f32>) -> Mat {
    let t = u.tiles[k];
    let uv = xz / t;
    let a = read_mat(k, uv, gx / t, gy / t);
    if (u.blend.y < 0.5) {
        return a;
    }
    let ang = 2.3 + f32(k) * 0.9;
    let cs = cos(ang);
    let sn = sin(ang);
    let r = mat2x2<f32>(cs, sn, -sn, cs);
    let uv2 = r * uv + vec2<f32>(0.37, 0.71);
    var b = read_mat(k, uv2, r * (gx / t), r * (gy / t));
    // The normal turns back with the read.
    b.nxy = transpose(r) * b.nxy;
    let field = game_fbm(xz.x / (t * 2.7) + f32(k) * 13.0, xz.y / (t * 2.7), 71 + k);
    let s = clamp((b.height - a.height) * 2.5 + (field - 0.5) * 7.0 + 0.5, 0.0, 1.0);
    return Mat(mix(a.color, b.color, s), mix(a.height, b.height, s), mix(a.nxy, b.nxy, s));
}

// ---------------------------------------------------------------- shading

fn aces(x: vec3<f32>) -> vec3<f32> {
    return clamp((x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14), vec3<f32>(0.0), vec3<f32>(1.0));
}

fn to_srgb(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(c, vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3<f32>(0.0031308));
}

const SKY: vec3<f32> = vec3<f32>(0.52, 0.62, 0.78);

@fragment
fn fs(in: VOut) -> @location(0) vec4<f32> {
    let xz = in.world.xz;
    let gx = dpdx(xz);
    let gy = dpdy(xz);
    let grad = ground_grad(xz);
    let n_geo = normalize(vec3<f32>(-grad.x, 1.0, -grad.y));
    let w = splat(xz, length(grad));

    // The game's height blend (terrain.wgsl blendWeights): each surface lifts
    // by height * 0.5 + weight; every surface within `depth` of the highest
    // shares the pixel by how far above that line it stands.
    var mats: array<Mat, 4>;
    var lift = vec4<f32>(-1.0);
    for (var k = 0; k < 4; k++) {
        mats[k] = sample_mat(k, xz, gx, gy);
        if (w[k] > 0.004) {
            lift[k] = mats[k].height * 0.5 + w[k];
        }
    }
    let top = max(max(lift.x, lift.y), max(lift.z, lift.w));
    let b = max(lift - vec4<f32>(top - u.blend.x), vec4<f32>(0.0));
    let bw = b / max(dot(b, vec4<f32>(1.0)), 1e-4);

    var albedo = vec3<f32>(0.0);
    var nxy = vec2<f32>(0.0);
    var h = 0.0;
    for (var k = 0; k < 4; k++) {
        albedo += mats[k].color * bw[k];
        nxy += mats[k].nxy * bw[k];
        h += mats[k].height * bw[k];
    }
    if (u.blend.z > 0.5) {
        albedo = bw.x * vec3<f32>(0.75, 0.25, 0.2) + bw.y * vec3<f32>(0.3, 0.65, 0.25) + bw.z * vec3<f32>(0.25, 0.4, 0.8) + bw.w * vec3<f32>(0.8, 0.75, 0.3);
        nxy = vec2<f32>(0.0);
    }

    // Detail normal on the ground's slope: +u along +x, +v along +z.
    let tx = normalize(vec3<f32>(1.0, 0.0, 0.0) - n_geo * n_geo.x);
    let tz = normalize(vec3<f32>(0.0, 0.0, 1.0) - n_geo * n_geo.z);
    let nz = sqrt(max(1.0 - dot(nxy, nxy), 0.0));
    let n = normalize(tx * nxy.x + tz * nxy.y + n_geo * nz);

    let l = normalize(u.sun.xyz);
    let sun = max(dot(n, l), 0.0) * u.sun.w * vec3<f32>(1.0, 0.95, 0.86);
    let sky = mix(vec3<f32>(0.22, 0.2, 0.17), SKY, n.y * 0.5 + 0.5) * 0.55;
    // Crevices (low material height) see less sky.
    let ao = mix(0.55, 1.0, smoothstep(0.0, 0.7, h));
    var col = albedo * (sun + sky * ao);

    let dist = length(in.world - u.eye.xyz);
    col = mix(col, SKY * 0.9, 1.0 - exp(-dist * 0.0015));
    col = aces(col * u.blend.w);
    return vec4<f32>(to_srgb(col), 1.0);
}

// The sky behind the terrain: a full-screen triangle drawn first.
@vertex
fn vs_sky(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    let p = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u)) * 2.0 - 1.0;
    return vec4<f32>(p, 0.0, 1.0);
}

@fragment
fn fs_sky(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    return vec4<f32>(to_srgb(aces(SKY * 0.9 * u.blend.w)), 1.0);
}
