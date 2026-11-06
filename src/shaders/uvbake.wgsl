// Rasterizes the mesh in UV space to produce per-texel world position and
// normal maps, then grows them past island borders (dilation) so painting and
// bilinear sampling never leave seams.

struct VIn {
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) tangent: vec4<f32>,
};

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) wpos: vec3<f32>,
    @location(1) n: vec3<f32>,
    @location(2) t: vec4<f32>,
};

@vertex
fn vs_uv(v: VIn) -> VOut {
    var out: VOut;
    out.clip = vec4<f32>(v.uv.x * 2.0 - 1.0, 1.0 - v.uv.y * 2.0, 0.5, 1.0);
    out.wpos = v.pos;
    out.n = v.normal;
    out.t = v.tangent;
    return out;
}

struct MapsOut {
    @location(0) pos: vec4<f32>,
    @location(1) nrm: vec4<f32>,
    // MikkTSpace tangent, bitangent sign in alpha (1: +1, 0: -1).
    @location(2) tan: vec4<f32>,
};

@fragment
fn fs_uv(in: VOut) -> MapsOut {
    var out: MapsOut;
    out.pos = vec4<f32>(in.wpos, 1.0);
    out.nrm = vec4<f32>(normalize(in.n) * 0.5 + 0.5, 1.0);
    out.tan = vec4<f32>(normalize(in.t.xyz) * 0.5 + 0.5, select(0.0, 1.0, in.t.w >= 0.0));
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
    let dims = vec2<i32>(textureDimensions(src_pos));
    var acc_p = vec3<f32>(0.0);
    var acc_n = vec3<f32>(0.0);
    var acc_t = vec4<f32>(0.0);
    var cnt = 0.0;
    for (var dy = -1; dy <= 1; dy++) {
        for (var dx = -1; dx <= 1; dx++) {
            let q = clamp(c + vec2<i32>(dx, dy), vec2<i32>(0), dims - 1);
            let s = textureLoad(src_pos, q, 0);
            if (s.a > 0.5) {
                acc_p += s.xyz;
                acc_n += textureLoad(src_nrm, q, 0).xyz * 2.0 - 1.0;
                let t = textureLoad(src_tan, q, 0);
                acc_t += vec4<f32>(t.xyz * 2.0 - 1.0, t.w);
                cnt += 1.0;
            }
        }
    }
    if (cnt == 0.0) {
        out.pos = vec4<f32>(0.0);
        out.nrm = vec4<f32>(0.0);
        out.tan = vec4<f32>(0.0);
        return out;
    }
    out.pos = vec4<f32>(acc_p / cnt, 1.0);
    out.nrm = vec4<f32>(normalize(acc_n) * 0.5 + 0.5, 1.0);
    out.tan = vec4<f32>(normalize(acc_t.xyz + vec3<f32>(1e-6)) * 0.5 + 0.5, select(0.0, 1.0, acc_t.w / cnt >= 0.5));
    return out;
}
