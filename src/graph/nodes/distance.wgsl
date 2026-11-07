// @name Distance
// @category Filters
// @description For every texel, the nearest white texel of the input: spreads its value (or the source's) from there (Voronoi regions, cracks between them) or gives the distance to it. Tileable.
// @passes log2
// @output gray
// @input input: gray = 0
// @input source: gray = 1
// @param mode: enum(Spread Source, Distance) = Spread Source
// @param max_distance: float(0.001, 1) = 0.2
// @param threshold: float(0, 1) = 0.5

// Passes: 0 seeds every white texel, then one jump-flooding pass per
// halving step (res/2 .. 1), then the output. A texel stores the offset
// (in uv) to its nearest seed and whether it has one; offsets stay small,
// which half floats keep precisely.
fn node(uv: vec2<f32>) -> vec4<f32> {
    let p = vec2<i32>(uv * res());
    let pi = pass_index();
    let last = pass_count() - 1;
    if (pi == 0) {
        if (in_input(uv) > params.threshold) {
            return vec4<f32>(0.0, 0.0, 1.0, 1.0);
        }
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    if (pi < last) {
        let jump = max(i32(res()) >> u32(pi), 1);
        var best = prev_at(p);
        var best_d = select(1e9, dot(best.xy, best.xy), best.z > 0.5);
        for (var k = 0; k < 9; k++) {
            if (k == 4) {
                continue;
            }
            let o = vec2<i32>(k % 3 - 1, k / 3 - 1) * jump;
            let c = prev_at(p + o);
            if (c.z > 0.5) {
                let off = vec2<f32>(o) / res() + c.xy;
                let d = dot(off, off);
                if (d < best_d) {
                    best_d = d;
                    best = vec4<f32>(off, 1.0, 1.0);
                }
            }
        }
        return best;
    }
    let s = prev_at(p);
    if (s.z < 0.5) {
        return vec4<f32>(0.0);
    }
    if (params.mode == 1) {
        return vec4<f32>(clamp(length(s.xy) / params.max_distance, 0.0, 1.0));
    }
    // Spread: the source at the seed, within the maximum distance.
    if (length(s.xy) > params.max_distance) {
        return vec4<f32>(0.0);
    }
    let at = fract(uv + s.xy);
    // Unconnected source: spread the input's own value.
    if ((u32(nu.info.y) & 2u) == 0u) {
        return vec4<f32>(in_input(at));
    }
    return vec4<f32>(in_source(at));
}
