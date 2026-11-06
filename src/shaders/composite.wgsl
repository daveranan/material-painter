// Layer stack compositing. One pass per visible layer, ping-ponging between
// two accumulation sets: reads the stack below, blends this layer on top.
// Fill layers and smart masks are evaluated procedurally here from the baked
// mesh maps, so they cost no texture memory.

struct LayerU {
    fill_color: vec4<f32>,  // sRGB rgb
    fill_rmh: vec4<f32>,    // roughness, metallic, height
    ch: vec4<f32>,          // per-channel enable (and allocated, for paint layers)
    p0: vec4<f32>,          // opacity, blend mode, kind (0 paint, 1 fill), has paint mask
    var0: vec4<f32>,        // color variation, variation scale, roughness variation, height variation
    var1: vec4<f32>,        // variation seed, per-part variation, projection (engine::Projection order), image channel bits
    bmin: vec4<f32>,
    bsize: vec4<f32>,
    gw: array<vec4<f32>, 5>, // smart mask input weights (see engine::GEN_INPUTS)
    gc: vec4<f32>,          // custom map inputs: index among the baked custom maps (-1 none)
    gcw: vec4<f32>,         // custom map inputs: weights
    gp0: vec4<f32>,         // level, contrast, scale, noise
    gp1: vec4<f32>,         // invert, seed, has generator, edge width
    gid: vec4<f32>,         // ID color, tolerance
    gx: vec4<f32>,          // baked edge max distance, unused, tri-planar sharpness
    img: vec4<f32>,         // image tiling, rotation (degrees), offset x, offset y
    emis: vec4<f32>,        // emission: linear rgb times strength, strength
    ex: vec4<f32>,          // normal on, emission on, normal strength, normal image is DirectX
    dc: array<vec4<f32>, 4>, // decal box: center + depth, right/width + softness, up/height + facing cosine, normal
};

@group(0) @binding(0) var<uniform> u: LayerU;
@group(0) @binding(1) var prev_color: texture_2d<f32>;
@group(0) @binding(2) var prev_rough: texture_2d<f32>;
@group(0) @binding(3) var prev_metal: texture_2d<f32>;
@group(0) @binding(4) var prev_height: texture_2d<f32>;
@group(0) @binding(5) var l_color: texture_2d<f32>;
@group(0) @binding(6) var l_rough: texture_2d<f32>;
@group(0) @binding(7) var l_metal: texture_2d<f32>;
@group(0) @binding(8) var l_height: texture_2d<f32>;
@group(0) @binding(9) var l_mask: texture_2d<f32>;
@group(0) @binding(10) var pos_map: texture_2d<f32>;
@group(0) @binding(11) var nrm_map: texture_2d<f32>;
@group(0) @binding(12) var map_a: texture_2d<f32>;  // AO, curvature, cavity, thickness
@group(0) @binding(13) var map_b: texture_2d<f32>;  // sky, edge dist, crease dist, part random
@group(0) @binding(14) var map_c: texture_2d<f32>;  // bent normal, part gradient
@group(0) @binding(15) var custom_maps: texture_2d_array<f32>;  // custom map i: channel i % 4 of layer i / 4
@group(0) @binding(16) var map_e: texture_2d<f32>;  // ID color
@group(0) @binding(17) var samp: sampler;
@group(0) @binding(18) var rsamp: sampler;  // tiling, mipmapped: fill images
@group(0) @binding(19) var prev_normal: texture_2d<f32>;  // tangent-space xy at 0.5 + 0.5 n
@group(0) @binding(20) var prev_emis: texture_2d<f32>;    // linear HDR rgb
@group(0) @binding(21) var l_normal: texture_2d<f32>;     // fill: normal image; folder merge: stack below
@group(0) @binding(22) var l_emis: texture_2d<f32>;       // fill: emission image; folder merge: stack below
@group(0) @binding(23) var tan_map: texture_2d<f32>;      // MikkTSpace tangent, sign in alpha

fn blend3(mode: i32, a: vec3<f32>, b: vec3<f32>) -> vec3<f32> {
    switch mode {
        case 1: { return a * b; }
        case 2: { return 1.0 - (1.0 - a) * (1.0 - b); }
        case 3: { return select(1.0 - 2.0 * (1.0 - a) * (1.0 - b), 2.0 * a * b, a < vec3<f32>(0.5)); }
        case 4: { return a + b; }
        case 5: { return a - b; }
        case 6: { return min(a, b); }
        case 7: { return max(a, b); }
        default: { return b; }
    }
}

fn blend1(mode: i32, a: f32, b: f32) -> f32 {
    return blend3(mode, vec3<f32>(a), vec3<f32>(b)).x;
}

fn weight(i: i32) -> f32 {
    var w = u.gw;
    return w[i / 4][i % 4];
}

fn rot2(q: vec2<f32>, deg: f32) -> vec2<f32> {
    let a = radians(deg);
    let c = cos(a);
    let s = sin(a);
    return vec2<f32>(q.x * c - q.y * s, q.x * s + q.y * c);
}

// A fill image sampled with the layer's projection. The mip level comes from
// how many image texels one output texel covers (`texel_world` is the world
// size of an output texel), since derivatives aren't available here.
fn fill_image(t: texture_2d<f32>, uv: vec2<f32>, p: vec3<f32>, n: vec3<f32>, texel_world: f32) -> vec4<f32> {
    let size = f32(textureDimensions(t).x);
    let tiling = max(u.img.x, 1e-3);
    let mode = i32(u.var1.z + 0.5);
    if (mode == 7) {
        // Decal: the image spans the box once, upright.
        let q = decal_coord(p);
        let lod = log2(max(texel_world * size * length(u.dc[1].xyz), 1.0));
        return textureSampleLevel(t, rsamp, vec2<f32>(q.x, 1.0 - q.y), lod);
    }
    if (mode == 0) {
        let q = rot2(uv - 0.5, u.img.y) * tiling + 0.5 + u.img.zw;
        let lod = log2(max(tiling * size / f32(textureDimensions(prev_color).x), 1.0));
        return textureSampleLevel(t, rsamp, q, lod);
    }
    if (mode >= 2) {
        // Planar, spherical and cylindrical projections span the mesh's
        // bounds once at tiling 1.
        let ext = max(max(u.bsize.x, u.bsize.y), max(u.bsize.z, 1e-4));
        let d = (p - (u.bmin.xyz + u.bsize.xyz * 0.5)) / ext;
        var c: vec2<f32>;
        switch mode {
            case 2: { c = vec2<f32>(-d.z, -d.y) + 0.5; }
            case 3: { c = vec2<f32>(d.x, d.z) + 0.5; }
            case 4: { c = vec2<f32>(d.x, -d.y) + 0.5; }
            case 5: {
                // Longitude around the vertical axis, latitude from the top.
                let r = max(length(d), 1e-6);
                c = vec2<f32>(atan2(d.x, d.z) / 6.2831853 + 0.5, acos(clamp(d.y / r, -1.0, 1.0)) / 3.1415927);
            }
            default: { c = vec2<f32>(atan2(d.x, d.z) / 6.2831853 + 0.5, 0.5 - d.y); }
        }
        let q = rot2(c - 0.5, u.img.y) * tiling + 0.5 + u.img.zw;
        let lod = log2(max(texel_world * tiling * size / ext, 1.0));
        return textureSampleLevel(t, rsamp, q, lod);
    }
    let lod = log2(max(texel_world * tiling * size, 1.0));
    var w = pow(abs(n), vec3<f32>(max(u.gx.z, 1.0)));
    w /= max(w.x + w.y + w.z, 1e-4);
    let q = p * tiling;
    return textureSampleLevel(t, rsamp, rot2(vec2<f32>(q.z, -q.y), u.img.y) + u.img.zw, lod) * w.x
        + textureSampleLevel(t, rsamp, rot2(q.xz, u.img.y) + u.img.zw, lod) * w.y
        + textureSampleLevel(t, rsamp, rot2(vec2<f32>(q.x, -q.y), u.img.y) + u.img.zw, lod) * w.z;
}

// Wood grain: rings and fibers running along the object's long axis, or
// vertically on faces that look down that axis (the ends of planks).
fn wood_grain(p: vec3<f32>, n: vec3<f32>, scale: f32) -> f32 {
    let size = u.bsize.xyz;
    var ax = 0;
    if (size.y > size.x && size.y >= size.z) {
        ax = 1;
    } else if (size.z > size.x) {
        ax = 2;
    }
    if (abs(n[ax]) > 0.7) {
        ax = select(1, 0, ax == 1);
    }
    let a = p[ax];
    let b = p[(ax + 1) % 3];
    let c = p[(ax + 2) % 3];
    let seed = vec3<f32>(u.gp1.y * 17.31);
    let warp = fbm(vec3<f32>(a * 0.35, b * 1.5, c * 1.5) * scale + seed);
    let r = (b + c) * scale * 2.5 + warp * 5.0;
    let ring = smoothstep(0.6, 1.0, abs(fract(r) * 2.0 - 1.0));
    let fiber = fbm(vec3<f32>(a * 0.5, b * 30.0, c * 30.0) * scale + seed + vec3<f32>(3.7));
    return clamp(ring * 0.7 + smoothstep(0.35, 0.75, fiber) * 0.45, 0.0, 1.0);
}

// Scratches on a plane: some grid cells hold one stroke, slightly bent,
// tapered at both ends, each with its own width and depth. `aa` is the
// texel footprint in q units: strokes thinner than a texel fade instead
// of breaking into stair-steps.
fn scratch_plane(q: vec2<f32>, seed: f32, aa: f32) -> f32 {
    let cell = floor(q);
    var m = 0.0;
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            let c = cell + vec2<f32>(f32(x), f32(y));
            let h = hash33(vec3<f32>(c, seed));
            if (h.z < 0.55) {
                continue;
            }
            let h2 = hash33(vec3<f32>(c + vec2<f32>(17.0, 31.0), seed + 5.0));
            let ang = h2.x * 3.14159265;
            let dir = vec2<f32>(cos(ang), sin(ang));
            let side = vec2<f32>(-dir.y, dir.x);
            let half_len = 0.35 + 1.25 * h.x * h.x;
            let rel = q - (c + h.xy);
            let t = dot(rel, dir);
            if (abs(t) > half_len) {
                continue;
            }
            let s = t / half_len;
            // A gentle arc, so strokes aren't ruler-straight.
            let bend = (h2.y - 0.5) * 0.25 * half_len;
            let d = abs(dot(rel, side) - bend * s * s);
            let width = 0.006 + 0.03 * h2.z * h2.z;
            let taper = 1.0 - smoothstep(0.3, 1.0, abs(s));
            let w = width * taper;
            // The falloff spans at least a texel, so a thin diagonal stroke
            // always reaches a texel center (no dashes); thin strokes are
            // fainter instead of thinner than a texel.
            let edge = max(aa, 1e-4);
            let line = clamp(1.0 - d / (w + edge), 0.0, 1.0);
            let strength = 0.3 + 0.7 * fract(h2.x * 13.7 + h.y * 7.1);
            m = max(m, line * line * strength * min(w / edge + 0.35, 1.0));
        }
    }
    return m;
}

// Scratches projected from the three axes and blended by the normal, so
// every face gets clean strokes: a set of bold ones and finer hairlines.
fn scratches(p: vec3<f32>, n: vec3<f32>, scale: f32, texel_world: f32) -> f32 {
    var w = pow(abs(n), vec3<f32>(4.0));
    w /= max(w.x + w.y + w.z, 1e-4);
    let k = scale * 5.0;
    let q = p * k;
    let seed = u.gp1.y * 3.7;
    let aa = texel_world * k;
    var m = 0.0;
    for (var i = 0; i < 3; i++) {
        if (w[i] < 0.01) {
            continue;
        }
        let a = select(select(q.xy, q.xz, i == 1), q.yz, i == 0);
        let bold = scratch_plane(a, seed + f32(i), aa);
        let fine = scratch_plane(a * 2.7 + vec2<f32>(3.1, 7.7), seed + f32(i) + 11.0, aa * 2.7) * 0.55;
        m += w[i] * max(bold, fine);
    }
    return m;
}

// Grunge for breaking up mask edges: broad blotches with crisp, gritty
// detail, so worn and dirty areas end in irregular borders instead of
// soft gradients.
fn grunge(q: vec3<f32>) -> f32 {
    let broad = fbm(q);
    let grit = fbm(q * 5.3 + vec3<f32>(4.1, 1.3, 7.9));
    let speck = vnoise(q * 23.0 + vec3<f32>(9.4, 2.2, 5.6));
    return broad * 0.6 + grit * 0.3 + speck * 0.1;
}

// Draft composites run at a fraction of the size: an output texel `c` reads
// the full-size inputs (position, normal and tangent maps, painted channels,
// masks) at `full(c)`. `u.gx.y` is the stride (0 or 1: full size).
fn stride() -> i32 {
    return i32(max(u.gx.y, 1.0));
}

fn full(c: vec2<i32>) -> vec2<i32> {
    return c * stride();
}

// World size of one output texel at `c` (on the surface point `p`).
fn texel_size(c: vec2<i32>, p: vec3<f32>) -> f32 {
    let right = textureLoad(pos_map, full(c) + vec2<i32>(stride(), 0), 0);
    return select(2.0 / f32(textureDimensions(pos_map).x) * f32(stride()), distance(right.xyz, p), right.a > 0.5);
}

// Smart mask: weighted sum of mesh-map inputs, noise breakup, then levels.
fn generator(p: vec3<f32>, n: vec3<f32>, uv: vec2<f32>, texel_world: f32) -> f32 {
    let A = textureSampleLevel(map_a, samp, uv, 0.0);
    let B = textureSampleLevel(map_b, samp, uv, 0.0);
    let C = textureSampleLevel(map_c, samp, uv, 0.0);
    let E = textureSampleLevel(map_e, samp, uv, 0.0);
    let scale = u.gp0.z;
    let q = p * scale + vec3<f32>(u.gp1.y * 17.31);
    let curv = (A.g - 0.5) * 2.0;
    let width = max(u.gp1.w, 1e-4);
    let emax = u.gx.x;

    // Thin parts (rods, rims, belts) are rounded all the way around, so edge
    // and convexity inputs would wear them completely; damp them there.
    let thick = mix(0.15, 1.0, smoothstep(0.15, 0.75, A.a));
    var inp: array<f32, 19>;
    inp[0] = (1.0 - smoothstep(0.0, width, B.g * emax)) * thick;
    inp[1] = max(curv, 0.0) * thick;
    inp[2] = max(-curv, 0.0);
    inp[3] = 1.0 - A.r;
    inp[4] = 1.0 - A.b;
    inp[5] = B.r;
    inp[6] = max(n.y, 0.0);
    inp[7] = 1.0 - A.a;
    inp[8] = C.a;
    inp[9] = (p.y - u.bmin.y) / max(u.bsize.y, 1e-4);
    inp[10] = B.a;
    inp[11] = 1.0 - smoothstep(0.0, width, B.b * emax);
    inp[12] = 0.0;
    if (weight(12) != 0.0) {
        let s = fbm(vec3<f32>(p.x * scale * 7.0, p.y * scale * 0.45, p.z * scale * 7.0) + vec3<f32>(u.gp1.y));
        inp[12] = smoothstep(0.45, 0.8, s);
    }
    inp[13] = 0.0;
    if (weight(13) != 0.0) {
        inp[13] = fbm(q);
    }
    let tol = max(u.gid.w, 1e-3);
    inp[14] = 1.0 - smoothstep(tol * 0.5, tol, distance(E.rgb, u.gid.rgb));
    inp[15] = max(-n.y, 0.0);
    inp[16] = 0.0;
    if (weight(16) != 0.0) {
        inp[16] = wood_grain(p, n, scale);
    }
    inp[17] = 0.0;
    if (weight(17) != 0.0) {
        inp[17] = cellular(q * 4.0);
    }
    inp[18] = 0.0;
    if (weight(18) != 0.0) {
        inp[18] = scratches(p, n, scale, texel_world);
    }

    var v = 0.0;
    for (var i = 0; i < 19; i++) {
        v += weight(i) * inp[i];
    }
    for (var k = 0; k < 4; k++) {
        let ci = i32(u.gc[k]);
        if (ci >= 0) {
            let t = textureSampleLevel(custom_maps, samp, uv, ci / 4, 0.0);
            v += u.gcw[k] * t[ci % 4];
        }
    }
    let noise_amt = u.gp0.w;
    if (noise_amt != 0.0) {
        // Fine grit breaks the mask's border up; a broad, slow variation on
        // top moves it in and out along long edges, so wear and grime don't
        // follow the mesh maps' contours as an even band.
        let broad = fbm(q * 0.35 + vec3<f32>(11.7, 3.1, 5.9)) - 0.5;
        v += ((grunge(q * 2.0) - 0.5) * 1.3 + broad * 2.6) * noise_amt;
    }
    let level = u.gp0.x;
    let contrast = max(u.gp0.y, 0.001);
    var m = smoothstep(level - contrast, level + contrast, v);
    if (u.gp1.x > 0.5) {
        m = 1.0 - m;
    }
    return m;
}

struct AccumOut {
    @location(0) color: vec4<f32>,
    @location(1) rough: vec4<f32>,
    @location(2) metal: vec4<f32>,
    @location(3) height: vec4<f32>,
    @location(4) normal: vec4<f32>,
    @location(5) emis: vec4<f32>,
};

// A normal image texel as a tangent-space vector (OpenGL: green up), with the
// layer's strength on its slopes.
fn decode_normal(t: vec4<f32>) -> vec3<f32> {
    var n = t.xyz * 2.0 - 1.0;
    if (u.ex.w > 0.5) {
        n.y = -n.y;
    }
    return vec3<f32>(n.xy * u.ex.z, max(n.z, 1e-3));
}

// One planar projection of the normal image as a world-space normal: `r` and
// `up` are the world directions of the image's right and up, `f` the face
// direction. Rotating the image coordinates by the layer's rotation rotates
// the slopes by the same angle.
fn planar_normal(q: vec2<f32>, lod: f32, r: vec3<f32>, up: vec3<f32>, f: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
    let t = decode_normal(textureSampleLevel(l_normal, rsamp, rot2(q, u.img.y) + u.img.zw, lod));
    let xy = rot2(t.xy, u.img.y);
    // Whiteout onto the surface normal, in this plane's frame: a flat image
    // returns the surface normal itself. (Built around the plane's axis
    // instead, every face not square to an axis came out tilted toward it.)
    let ln = vec3<f32>(dot(n, r), dot(n, up), abs(dot(n, f)));
    let o = vec3<f32>(xy + ln.xy, t.z * ln.z);
    return r * o.x + up * o.y + f * o.z;
}

// The fill's normal image in this texel's tangent space (the frame the
// viewport and exports decode: MikkTSpace, bitangent "up" in the texture).
fn fill_normal(c: vec2<i32>, uv: vec2<f32>, p: vec3<f32>, n: vec3<f32>, texel_world: f32) -> vec3<f32> {
    let size = f32(textureDimensions(l_normal).x);
    let tiling = max(u.img.x, 1e-3);
    let mode = i32(u.var1.z + 0.5);
    if (mode == 0) {
        let q = rot2(uv - 0.5, u.img.y) * tiling + 0.5 + u.img.zw;
        let lod = log2(max(tiling * size / f32(textureDimensions(prev_color).x), 1.0));
        let t = decode_normal(textureSampleLevel(l_normal, rsamp, q, lod));
        return normalize(vec3<f32>(rot2(t.xy, u.img.y), t.z));
    }
    var nw = n;
    if (mode == 1) {
        let lod = log2(max(texel_world * tiling * size, 1.0));
        var w = pow(abs(n), vec3<f32>(max(u.gx.z, 1.0)));
        w /= max(w.x + w.y + w.z, 1e-4);
        let q = p * tiling;
        let s = sign(n + vec3<f32>(1e-6));
        // Image axes per projection match fill_image(): x (z, -y), y (x, z), z (x, -y).
        nw = planar_normal(vec2<f32>(q.z, -q.y), lod, vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(s.x, 0.0, 0.0), n) * w.x
            + planar_normal(q.xz, lod, vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 0.0, -1.0), vec3<f32>(0.0, s.y, 0.0), n) * w.y
            + planar_normal(vec2<f32>(q.x, -q.y), lod, vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(0.0, 0.0, s.z), n) * w.z;
    } else if (mode <= 4) {
        let ext = max(max(u.bsize.x, u.bsize.y), max(u.bsize.z, 1e-4));
        let d = (p - (u.bmin.xyz + u.bsize.xyz * 0.5)) / ext;
        let lod = log2(max(texel_world * tiling * size / ext, 1.0));
        let s = sign(n + vec3<f32>(1e-6));
        switch mode {
            case 2: { nw = planar_normal(vec2<f32>(-d.z, -d.y) * tiling, lod, vec3<f32>(0.0, 0.0, -1.0), vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(s.x, 0.0, 0.0), n); }
            case 3: { nw = planar_normal(vec2<f32>(d.x, d.z) * tiling, lod, vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 0.0, -1.0), vec3<f32>(0.0, s.y, 0.0), n); }
            default: { nw = planar_normal(vec2<f32>(d.x, -d.y) * tiling, lod, vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(0.0, 0.0, s.z), n); }
        }
    } else {
        // Spherical and cylindrical: no normal detail.
        return vec3<f32>(0.0, 0.0, 1.0);
    }
    // World to tangent space.
    let ts = textureLoad(tan_map, full(c), 0);
    let t0 = ts.xyz * 2.0 - 1.0;
    let t = normalize(t0 - n * dot(n, t0) + vec3<f32>(1e-6, 0.0, 0.0));
    let b = -cross(n, t) * select(-1.0, 1.0, ts.w > 0.5);
    let v = normalize(nw);
    return normalize(vec3<f32>(dot(v, t), dot(v, b), max(dot(v, n), 1e-3)));
}

fn unpack_normal(t: vec2<f32>) -> vec3<f32> {
    let xy = t * 2.0 - 1.0;
    return vec3<f32>(xy, sqrt(max(1.0 - dot(xy, xy), 0.0)));
}

// Decal box coordinates of a point: x and y 0..1 across the box (y up), z
// -1..1 through its depth.
fn decal_coord(p: vec3<f32>) -> vec3<f32> {
    let d = p - u.dc[0].xyz;
    return vec3<f32>(dot(d, u.dc[1].xyz) + 0.5, dot(d, u.dc[2].xyz) + 0.5, dot(d, u.dc[3].xyz) / u.dc[0].w);
}

// How much of a point the decal box covers: inside it across the surface
// (soft edges), within its depth, on faces turned toward it.
fn decal_cover(p: vec3<f32>, n: vec3<f32>) -> f32 {
    let q = decal_coord(p);
    let s = max(u.dc[1].w, 1e-4);
    let across = smoothstep(0.0, s, q.x) * smoothstep(0.0, s, 1.0 - q.x) * smoothstep(0.0, s, q.y) * smoothstep(0.0, s, 1.0 - q.y);
    let through = 1.0 - smoothstep(0.85, 1.0, abs(q.z));
    // Facing 180 (cosine -1) wraps around every face in the box.
    let facing = select(smoothstep(u.dc[2].w - 0.05, u.dc[2].w + 0.02, dot(n, u.dc[3].xyz)), 1.0, u.dc[2].w < -0.999);
    return across * through * facing;
}

// A decal at a point: (image color, coverage). `flip` reads the image
// mirrored left to right (the mirrored copy, so text stays readable).
fn decal_eval(p: vec3<f32>, n: vec3<f32>, texel_world: f32, flip: bool) -> vec4<f32> {
    let cov = decal_cover(p, n);
    if (cov <= 0.0) {
        return vec4<f32>(1.0, 1.0, 1.0, 0.0);
    }
    let bits = u32(u.var1.w);
    if ((bits & 1u) == 0u) {
        return vec4<f32>(1.0, 1.0, 1.0, cov);
    }
    let q = decal_coord(p);
    let x = select(q.x, 1.0 - q.x, flip);
    let size = f32(textureDimensions(l_color).x);
    let lod = log2(max(texel_world * size * length(u.dc[1].xyz), 1.0));
    let t = textureSampleLevel(l_color, rsamp, vec2<f32>(x, 1.0 - q.y), lod);
    if ((bits & 16u) != 0u) {
        return vec4<f32>(1.0, 1.0, 1.0, cov * t.r);
    }
    return vec4<f32>(t.rgb, cov * t.a);
}

// A decal at a point, and its mirror image across x = 0 when it has one.
fn decal_both(p: vec3<f32>, n: vec3<f32>, texel_world: f32) -> vec4<f32> {
    var d = decal_eval(p, n, texel_world, false);
    if (u.dc[3].w > 0.5) {
        let m = vec3<f32>(-1.0, 1.0, 1.0);
        let e = decal_eval(p * m, n * m, texel_world, true);
        if (e.a > d.a) {
            d = e;
        }
    }
    return d;
}

// The layer's mask before opacity: the painted mask (or, with mask filters,
// the filtered mask) times the smart mask.
fn layer_mask(c: vec2<i32>, p: vec3<f32>, n: vec3<f32>, uv: vec2<f32>) -> f32 {
    var mask = 1.0;
    if (u.p0.w > 0.5) {
        mask *= textureLoad(l_mask, full(c), 0).r;
    }
    if (u.gp1.z > 0.5) {
        mask *= generator(p, n, uv, texel_size(c, p));
    }
    return mask;
}

// The layer's mask alone, as the first step of its mask filters.
@fragment
fn fs_mask_source(in: FsOut) -> @location(0) vec4<f32> {
    let c = vec2<i32>(in.pos.xy);
    let uv = in.pos.xy / vec2<f32>(textureDimensions(pos_map));
    let p = textureLoad(pos_map, full(c), 0).xyz;
    let n = normalize(textureLoad(nrm_map, full(c), 0).xyz * 2.0 - 1.0);
    return vec4<f32>(layer_mask(c, p, n, uv), 0.0, 0.0, 1.0);
}

// Where a layer shows (its mask, a decal's box and text), for the viewport's
// selection highlight.
@fragment
fn fs_cover(in: FsOut) -> @location(0) vec4<f32> {
    let c = vec2<i32>(in.pos.xy);
    let uv = in.pos.xy / vec2<f32>(textureDimensions(pos_map));
    let p = textureLoad(pos_map, full(c), 0).xyz;
    let n = normalize(textureLoad(nrm_map, full(c), 0).xyz * 2.0 - 1.0);
    var m = layer_mask(c, p, n, uv);
    if (u.p0.z > 0.5 && u.p0.z < 1.5 && i32(u.var1.z + 0.5) == 7) {
        m *= decal_both(p, n, texel_size(c, p)).a;
    }
    return vec4<f32>(m, 0.0, 0.0, 1.0);
}

// Live detail (engine::encode_live_weights): carries, per texel, how much of
// each live layer's color reaches the final result (one slot per channel).
// The stack-below color slot holds the weights so far; `u.gx.w` is this
// layer's slot + 1 (0: not live).
@fragment
fn fs_live_weight(in: FsOut) -> @location(0) vec4<f32> {
    let c = vec2<i32>(in.pos.xy);
    let uv = in.pos.xy / vec2<f32>(textureDimensions(pos_map));
    var w = textureLoad(prev_color, c, 0);
    let p = textureLoad(pos_map, full(c), 0).xyz;
    let n = normalize(textureLoad(nrm_map, full(c), 0).xyz * 2.0 - 1.0);
    var a = u.p0.x * layer_mask(c, p, n, uv);
    if (u.p0.z > 0.5 && u.p0.z < 1.5 && i32(u.var1.z + 0.5) == 7) {
        a *= decal_both(p, n, texel_size(c, p)).a;
    }
    // Color coverage: a fill's color channel switch, a paint layer's alpha.
    var ac = select(0.0, 1.0, u.ch.x > 0.5);
    if (u.p0.z < 0.5 && u.ch.x > 0.5) {
        ac = textureLoad(l_color, full(c), 0).a;
    }
    a = clamp(a * ac, 0.0, 1.0);
    if (i32(u.p0.y) == 0) {
        w *= 1.0 - a;
    }
    let slot = i32(u.gx.w + 0.5) - 1;
    w.x = select(w.x, a, slot == 0);
    w.y = select(w.y, a, slot == 1);
    w.z = select(w.z, a, slot == 2);
    return vec4<f32>(w.xyz, 1.0);
}

// The four paint channels alone: documents without normal or emission layers
// composite through these, skipping those channels' reads and writes.
struct AccumOut4 {
    @location(0) color: vec4<f32>,
    @location(1) rough: vec4<f32>,
    @location(2) metal: vec4<f32>,
    @location(3) height: vec4<f32>,
};

@fragment
fn fs_layer(in: FsOut) -> AccumOut {
    return layer_pass(in, true);
}

@fragment
fn fs_layer4(in: FsOut) -> AccumOut4 {
    let o = layer_pass(in, false);
    return AccumOut4(o.color, o.rough, o.metal, o.height);
}

fn layer_pass(in: FsOut, extra: bool) -> AccumOut {
    let c = vec2<i32>(in.pos.xy);
    let dims = vec2<f32>(textureDimensions(prev_color));
    let uv = in.pos.xy / dims;

    let pc = textureLoad(prev_color, c, 0).rgb;
    let pr = textureLoad(prev_rough, c, 0).r;
    let pm = textureLoad(prev_metal, c, 0).r;
    let ph = textureLoad(prev_height, c, 0).r;
    var pn = vec3<f32>(0.0, 0.0, 1.0);
    var pe = vec3<f32>(0.0);
    if (extra) {
        pn = unpack_normal(textureLoad(prev_normal, c, 0).rg);
        pe = textureLoad(prev_emis, c, 0).rgb;
    }

    let p = textureLoad(pos_map, full(c), 0).xyz;
    let n = normalize(textureLoad(nrm_map, full(c), 0).xyz * 2.0 - 1.0);

    // The mask first: where it hides the layer, the stack below passes
    // through and the layer's content (noise, images) isn't evaluated.
    var mask = u.p0.x * layer_mask(c, p, n, uv);
    // Decal fills show only inside their box, cut out by the image's alpha
    // (a grayscale image's value).
    let decal = u.p0.z > 0.5 && u.p0.z < 1.5 && i32(u.var1.z + 0.5) == 7;
    var dimg = vec4<f32>(1.0);
    if (decal && mask > 0.0) {
        let d = decal_both(p, n, texel_size(c, p));
        mask *= d.a;
        dimg = vec4<f32>(d.rgb, 1.0);
    }
    if (mask <= 0.0) {
        var keep: AccumOut;
        keep.color = vec4<f32>(pc, 1.0);
        keep.rough = vec4<f32>(pr);
        keep.metal = vec4<f32>(pm);
        keep.height = vec4<f32>(ph);
        if (extra) {
            keep.normal = vec4<f32>(pn.xy * 0.5 + 0.5, 0.0, 1.0);
            keep.emis = vec4<f32>(pe, 1.0);
        }
        return keep;
    }

    var sc = u.fill_color.rgb;
    var sr = u.fill_rmh.x;
    var sm = u.fill_rmh.y;
    var sh = u.fill_rmh.z;
    var ac = u.ch.x;
    var ar = u.ch.y;
    var am = u.ch.z;
    var ah = u.ch.w;
    // Normal and emission: fill layers only.
    var sn = vec3<f32>(0.0, 0.0, 1.0);
    var se = u.emis.rgb;
    var an = 0.0;
    var ae = 0.0;

    if (u.p0.z < 0.5) {
        // Paint layer: per-channel (value, alpha).
        if (u.ch.x > 0.5) {
            let t = textureLoad(l_color, full(c), 0);
            sc = t.rgb;
            ac = t.a;
        }
        if (u.ch.y > 0.5) {
            let t = textureLoad(l_rough, full(c), 0).rg;
            sr = t.x;
            ar = t.y;
        }
        if (u.ch.z > 0.5) {
            let t = textureLoad(l_metal, full(c), 0).rg;
            sm = t.x;
            am = t.y;
        }
        if (u.ch.w > 0.5) {
            let t = textureLoad(l_height, full(c), 0).rg;
            sh = t.x;
            ah = t.y;
        }
    } else {
        // Fill layer: images replace their channels' values, then procedural
        // and per-part variation apply on top.
        let bits = u32(u.var1.w);
        if (bits != 0u) {
            let texel_world = texel_size(c, p);
            if (decal) {
                // A decal's image tints the fill color; its alpha is the mask.
                sc = sc * dimg.rgb;
            } else if ((bits & 1u) != 0u) {
                let img = fill_image(l_color, uv, p, n, texel_world);
                sc = select(img.rgb, img.rrr, (bits & 16u) != 0u);
            }
            if ((bits & 2u) != 0u) {
                sr = fill_image(l_rough, uv, p, n, texel_world).r;
            }
            if ((bits & 4u) != 0u) {
                sm = fill_image(l_metal, uv, p, n, texel_world).r;
            }
            if ((bits & 8u) != 0u) {
                // Mid grey is the surface; lighter is raised. The layer's
                // height value scales it (0.5: as is; negative inverts).
                sh = (fill_image(l_height, uv, p, n, texel_world).r - 0.5) * 2.0 * u.fill_rmh.z;
            }
            if (extra && (bits & 32u) != 0u && u.ex.x > 0.5) {
                sn = fill_normal(c, uv, p, n, texel_world);
            }
            if (extra && (bits & 64u) != 0u && u.ex.y > 0.5) {
                // sRGB image to linear light, times the strength.
                se = pow(fill_image(l_emis, uv, p, n, texel_world).rgb, vec3<f32>(2.2)) * u.emis.w;
            }
        }
        // Per-part variation tilts each part's normal a little (plates of a
        // hull catching the light at slightly different angles).
        if (extra && u.ex.x > 0.5 && u.var1.y != 0.0) {
            let part = textureSampleLevel(map_b, samp, uv, 0.0).a;
            let tilt = vec2<f32>(fract(part * 7.31), fract(part * 13.17)) - 0.5;
            sn = normalize(vec3<f32>(sn.xy + tilt * u.var1.y * 0.25, sn.z));
        }
        an = u.ex.x;
        ae = u.ex.y;
        let vs = u.var0.y;
        let seed = vec3<f32>(u.var1.x * 13.7);
        if (u.var0.x != 0.0) {
            let n1 = fbm(p * vs + seed);
            sc = sc * (1.0 + (n1 - 0.5) * 2.0 * u.var0.x);
        }
        if (u.var0.z != 0.0) {
            let n2 = fbm(p * vs * 1.7 + seed + vec3<f32>(5.2));
            sr = sr + (n2 - 0.5) * u.var0.z;
        }
        if (u.var0.w != 0.0) {
            let n3 = fbm(p * vs * 3.1 + seed + vec3<f32>(11.9));
            sh = sh + (n3 - 0.5) * u.var0.w;
        }
        if (u.var1.y != 0.0) {
            let pr_rand = textureSampleLevel(map_b, samp, uv, 0.0).a;
            sc = sc * (1.0 + (pr_rand - 0.5) * 2.0 * u.var1.y);
            sr = sr + (fract(pr_rand * 7.31) - 0.5) * u.var1.y * 0.6;
        }
        sc = clamp(sc, vec3<f32>(0.0), vec3<f32>(1.0));
        sr = clamp(sr, 0.0, 1.0);
    }

    let mode = i32(u.p0.y);
    var out: AccumOut;
    out.color = vec4<f32>(clamp(mix(pc, blend3(mode, pc, sc), ac * mask), vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
    out.rough = vec4<f32>(clamp(mix(pr, blend1(mode, pr, sr), ar * mask), 0.0, 1.0));
    out.metal = vec4<f32>(clamp(mix(pm, blend1(mode, pm, sm), am * mask), 0.0, 1.0));
    out.height = vec4<f32>(mix(ph, blend1(mode, ph, sh), ah * mask));
    if (extra) {
        // Normals combine with the ones below (whiteout), so layered detail adds up.
        let comb = normalize(vec3<f32>(pn.xy + sn.xy, pn.z * sn.z));
        out.normal = vec4<f32>(mix(pn.xy, comb.xy, an * mask) * 0.5 + 0.5, 0.0, 1.0);
        out.emis = vec4<f32>(max(mix(pe, blend3(mode, pe, se), ae * mask), vec3<f32>(0.0)), 1.0);
    }
    return out;
}

// Ends a faded or masked folder: the stack with the folder's layers on it
// (prev_*) is faded over the stack below the folder (in the layer slots, l_*)
// by the folder's opacity and mask, like an isolated group.
@fragment
fn fs_folder_merge(in: FsOut) -> AccumOut {
    return folder_merge(in, true);
}

@fragment
fn fs_folder_merge4(in: FsOut) -> AccumOut4 {
    let o = folder_merge(in, false);
    return AccumOut4(o.color, o.rough, o.metal, o.height);
}

fn folder_merge(in: FsOut, extra: bool) -> AccumOut {
    let c = vec2<i32>(in.pos.xy);
    let uv = in.pos.xy / vec2<f32>(textureDimensions(prev_color));
    var m = u.p0.x;
    if (u.p0.w > 0.5) {
        m *= textureLoad(l_mask, full(c), 0).r;
    }
    if (u.gp1.z > 0.5) {
        let p = textureLoad(pos_map, full(c), 0).xyz;
        let n = normalize(textureLoad(nrm_map, full(c), 0).xyz * 2.0 - 1.0);
        m *= generator(p, n, uv, texel_size(c, p));
    }
    var out: AccumOut;
    out.color = vec4<f32>(mix(textureLoad(l_color, c, 0).rgb, textureLoad(prev_color, c, 0).rgb, m), 1.0);
    out.rough = vec4<f32>(mix(textureLoad(l_rough, c, 0).r, textureLoad(prev_rough, c, 0).r, m));
    out.metal = vec4<f32>(mix(textureLoad(l_metal, c, 0).r, textureLoad(prev_metal, c, 0).r, m));
    out.height = vec4<f32>(mix(textureLoad(l_height, c, 0).r, textureLoad(prev_height, c, 0).r, m));
    if (extra) {
        out.normal = vec4<f32>(mix(textureLoad(l_normal, c, 0).rg, textureLoad(prev_normal, c, 0).rg, m), 0.0, 1.0);
        out.emis = vec4<f32>(mix(textureLoad(l_emis, c, 0).rgb, textureLoad(prev_emis, c, 0).rgb, m), 1.0);
    }
    return out;
}
