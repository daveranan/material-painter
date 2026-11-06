// Rasterizes the mesh in UV space to produce per-texel world position and
// normal maps, then grows them past island borders (dilation) so painting and
// bilinear sampling never leave seams.

// Conservative rasterization: a texel whose centre misses every triangle but
// whose square a triangle touches (thin slivers along creases, island
// borders) would otherwise get a dilated guess, and tri-planar layers sampled
// at that guess show as dashes along the seams. Each triangle is drawn grown
// by a texel; a fragment stores the closest point of the real triangle, with
// its distance outside as depth so texels inside a triangle always win.

// The mesh's vertex and index buffers, read as storage (mesh::Vertex: 12 floats).
@group(0) @binding(0) var<storage, read> verts: array<f32>;
@group(0) @binding(1) var<storage, read> indices: array<u32>;
// x: texture size in texels.
@group(0) @binding(2) var<uniform> uvb: vec4<f32>;

const VSTRIDE: u32 = 12u;
// Bilinear filtering reads the 2x2 texels around a sample, so every texel whose
// centre is within a texel diagonal of the triangle needs its own value.
const REACH: f32 = 1.5;
// Corners of very thin triangles move far when the edges are pushed out; cap it.
const MAX_GROW: f32 = 6.0;

fn v_pos(i: u32) -> vec3<f32> {
    let b = i * VSTRIDE;
    return vec3<f32>(verts[b], verts[b + 1u], verts[b + 2u]);
}
fn v_nrm(i: u32) -> vec3<f32> {
    let b = i * VSTRIDE + 3u;
    return vec3<f32>(verts[b], verts[b + 1u], verts[b + 2u]);
}
fn v_uv(i: u32) -> vec2<f32> {
    let b = i * VSTRIDE + 6u;
    return vec2<f32>(verts[b], verts[b + 1u]);
}
fn v_tan(i: u32) -> vec4<f32> {
    let b = i * VSTRIDE + 8u;
    return vec4<f32>(verts[b], verts[b + 1u], verts[b + 2u], verts[b + 3u]);
}

struct VOut {
    @builtin(position) clip: vec4<f32>,
    // First index of the triangle in `indices`.
    @location(0) @interpolate(flat) tri: u32,
};

@vertex
fn vs_uv(@builtin(vertex_index) vi: u32) -> VOut {
    let corner = vi % 3u;
    let first = vi - corner;
    let res = uvb.x;
    // Triangle in texel units, with the drawn corner first.
    let a = v_uv(indices[first + corner]) * res;
    let b = v_uv(indices[first + (corner + 1u) % 3u]) * res;
    let c = v_uv(indices[first + (corner + 2u) % 3u]) * res;
    // Push both edges at this corner out by REACH and move the corner to
    // where the pushed edges meet (winding-independent: "out" is away from
    // the opposite corner).
    var p = a;
    let e1 = b - a;
    let e2 = c - a;
    let l1 = length(e1);
    let l2 = length(e2);
    if (l1 > 1e-6 && l2 > 1e-6) {
        var n1 = vec2<f32>(-e1.y, e1.x) / l1;
        n1 = select(n1, -n1, dot(n1, e2) > 0.0);
        var n2 = vec2<f32>(-e2.y, e2.x) / l2;
        n2 = select(n2, -n2, dot(n2, e1) > 0.0);
        let s = n1 + n2;
        let d = 1.0 + dot(n1, n2);
        var off = (n1 + n2) * REACH;
        if (d > 1e-4) {
            off = s * (REACH / d);
        }
        let len = length(off);
        p = a + select(off, off * (MAX_GROW / len), len > MAX_GROW);
    } else {
        // Degenerate in UV: a small square around the corner still reaches its texels.
        p = a + vec2<f32>(select(-REACH, REACH, corner == 1u), select(-REACH, REACH, corner == 2u));
    }
    let uv = p / res;
    var out: VOut;
    out.clip = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.5, 1.0);
    out.tri = first;
    return out;
}

// Closest point of segment ab to q, as (distance², t).
fn seg_closest(q: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> vec2<f32> {
    let ab = b - a;
    let t = clamp(dot(q - a, ab) / max(dot(ab, ab), 1e-12), 0.0, 1.0);
    let d = q - (a + ab * t);
    return vec2<f32>(dot(d, d), t);
}

struct MapsOut {
    @location(0) pos: vec4<f32>,
    @location(1) nrm: vec4<f32>,
    // MikkTSpace tangent, bitangent sign in alpha (1: +1, 0: -1).
    @location(2) tan: vec4<f32>,
};

struct UvOut {
    @location(0) pos: vec4<f32>,
    @location(1) nrm: vec4<f32>,
    @location(2) tan: vec4<f32>,
    @builtin(frag_depth) depth: f32,
};

@fragment
fn fs_uv(in: VOut) -> UvOut {
    let res = uvb.x;
    let i0 = indices[in.tri];
    let i1 = indices[in.tri + 1u];
    let i2 = indices[in.tri + 2u];
    let a = v_uv(i0) * res;
    let b = v_uv(i1) * res;
    let c = v_uv(i2) * res;
    let q = in.clip.xy;
    // Barycentrics of the texel centre; inside the triangle when all >= 0.
    let area = (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
    var w = vec3<f32>(1.0 / 3.0);
    var dist = 0.0;
    var inside = false;
    if (abs(area) > 1e-9) {
        let w1 = ((q.x - a.x) * (c.y - a.y) - (q.y - a.y) * (c.x - a.x)) / area;
        let w2 = ((b.x - a.x) * (q.y - a.y) - (b.y - a.y) * (q.x - a.x)) / area;
        w = vec3<f32>(1.0 - w1 - w2, w1, w2);
        inside = min(w.x, min(w.y, w.z)) >= 0.0;
    }
    if (!inside) {
        // Nearest point on the border.
        let ab = seg_closest(q, a, b);
        let bc = seg_closest(q, b, c);
        let ca = seg_closest(q, c, a);
        var best = ab.x;
        w = vec3<f32>(1.0 - ab.y, ab.y, 0.0);
        if (bc.x < best) {
            best = bc.x;
            w = vec3<f32>(0.0, 1.0 - bc.y, bc.y);
        }
        if (ca.x < best) {
            best = ca.x;
            w = vec3<f32>(ca.y, 0.0, 1.0 - ca.y);
        }
        dist = sqrt(best);
        if (dist > REACH) {
            discard;
        }
    }
    let t = v_tan(i0) * w.x + v_tan(i1) * w.y + v_tan(i2) * w.z;
    var out: UvOut;
    out.pos = vec4<f32>(v_pos(i0) * w.x + v_pos(i1) * w.y + v_pos(i2) * w.z, 1.0);
    out.nrm = vec4<f32>(normalize(v_nrm(i0) * w.x + v_nrm(i1) * w.y + v_nrm(i2) * w.z) * 0.5 + 0.5, 1.0);
    out.tan = vec4<f32>(normalize(t.xyz + vec3<f32>(1e-6)) * 0.5 + 0.5, select(0.0, 1.0, t.w >= 0.0));
    out.depth = dist / (REACH * 2.0);
    return out;
}

@group(0) @binding(0) var src_pos: texture_2d<f32>;
@group(0) @binding(1) var src_nrm: texture_2d<f32>;
@group(0) @binding(2) var src_tan: texture_2d<f32>;

@fragment
fn fs_dilate(in: FsOut) -> MapsOut {
    let c = vec2<i32>(in.pos.xy);
    var out: MapsOut;
    let p = textureLoad(src_pos, c, 0);
    if (p.a > 0.5) {
        out.pos = p;
        out.nrm = textureLoad(src_nrm, c, 0);
        out.tan = textureLoad(src_tan, c, 0);
        return out;
    }
    // Copy one neighbour instead of averaging: a padding texel between two
    // islands would otherwise get the midpoint of two unrelated surface points,
    // and tri-planar layers evaluated there bleed into the view's mip levels
    // as dots along the seams. Edge neighbours win over corner neighbours.
    let dims = vec2<i32>(textureDimensions(src_pos));
    let offsets = array<vec2<i32>, 8>(
        vec2<i32>(1, 0), vec2<i32>(-1, 0), vec2<i32>(0, 1), vec2<i32>(0, -1),
        vec2<i32>(1, 1), vec2<i32>(-1, 1), vec2<i32>(1, -1), vec2<i32>(-1, -1),
    );
    for (var i = 0; i < 8; i++) {
        let q = clamp(c + offsets[i], vec2<i32>(0), dims - 1);
        let s = textureLoad(src_pos, q, 0);
        if (s.a > 0.5) {
            out.pos = s;
            out.nrm = textureLoad(src_nrm, q, 0);
            out.tan = textureLoad(src_tan, q, 0);
            return out;
        }
    }
    out.pos = vec4<f32>(0.0);
    out.nrm = vec4<f32>(0.0);
    out.tan = vec4<f32>(0.0);
    return out;
}
