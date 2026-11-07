// @name Tile Sampler
// @category Generators
// @description Scatters shapes over a grid with random position, size, rotation, brightness and gaps; up to four pattern inputs picked at random, a mask that hides tiles (sampled at each tile's center: no half tiles), and scale and rotation maps. Outputs the heights, or per instance a random grey (an ID for masks and color), a mask, or which pattern it is. Tileable.
// @output gray
// @input pattern: gray = 1
// @input pattern_2: gray = 1
// @input pattern_3: gray = 1
// @input pattern_4: gray = 1
// @input mask: gray = 1
// @input scale: gray = 1
// @input rotation: gray = 0
// @param shape: enum(Disc, Square, Diamond, Cone, Pattern Input, Paraboloid, Bell, Gaussian, Pyramid, Capsule) = Disc
// @param columns: int(1, 64) = 8
// @param rows: int(1, 64) = 8
// @param size: float(0.05, 2) = 0.8
// @param size_random: float(0, 1) = 0.3
// @param position_random: float(0, 1) = 0.5
// @param rotation_random: float(0, 1) = 0.5
// @param brightness_random: float(0, 1) = 0.4
// @param gaps: float(0, 1) = 0
// @param blend: enum(Max, Add) = Max
// @param seed: int(0, 1000) = 0
// @param pattern_count: int(1, 4) = 1
// @param mask_threshold: float(0, 1) = 0.5
// @param scale_map: float(0, 1) = 0
// @param rotation_map: float(0, 1) = 0
// @param output: enum(Height, Instance Random, Instance Mask, Pattern Index) = Height

fn pattern_at(k: i32, uv: vec2<f32>) -> f32 {
    switch k {
        case 1: { return in_pattern_2(uv); }
        case 2: { return in_pattern_3(uv); }
        case 3: { return in_pattern_4(uv); }
        default: { return in_pattern(uv); }
    }
}

fn shape_at(q: vec2<f32>, k: i32) -> f32 {
    // q spans -0.5..0.5 across one stamp.
    if (abs(q.x) > 0.5 || abs(q.y) > 0.5) {
        return 0.0;
    }
    let p = q * 2.0;
    let r2 = dot(p, p);
    switch params.shape {
        case 1: { return 1.0; }
        case 2: { return 1.0 - smoothstep(0.96, 1.0, abs(p.x) + abs(p.y)); }
        case 3: { return clamp(1.0 - length(p), 0.0, 1.0); }
        case 4: { return pattern_at(k, q + 0.5); }
        case 5: { return max(1.0 - r2, 0.0); }
        case 6: {
            let b = max(1.0 - r2, 0.0);
            return b * b * (3.0 - 2.0 * b);
        }
        case 7: { return select(0.0, exp(-r2 * 4.0), r2 < 1.0); }
        case 8: { return 1.0 - max(abs(p.x), abs(p.y)); }
        case 9: {
            // Capsule: a rounded bar twice as long as it is wide.
            let d = length(vec2<f32>(max(abs(p.x) - 0.5, 0.0), p.y * 2.0));
            return clamp(1.0 - d, 0.0, 1.0);
        }
        default: { return 1.0 - smoothstep(0.92, 1.0, length(p)); }
    }
}

fn node(uv: vec2<f32>) -> vec4<f32> {
    let n = vec2<f32>(f32(params.columns), f32(params.rows));
    let base = floor(uv * n);
    var v = 0.0;
    // The instance on top (largest stamp value) for the per-instance outputs.
    var top = 0.0;
    var top_id = 0.0;
    var top_pattern = 0.0;
    // Stamps can reach into the neighboring cells.
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            let c = base + vec2<f32>(f32(x), f32(y));
            let cell = c - n * floor(c / n);
            let h = hash2(cell, f32(params.seed));
            let h2 = hash2(cell, f32(params.seed) + 11.0);
            let h3 = hash2(cell, f32(params.seed) + 23.0);
            let h4 = hash2(cell, f32(params.seed) + 37.0);
            if (h3.x < params.gaps) {
                continue;
            }
            let center = (c + 0.5 + (h - 0.5) * params.position_random) / n;
            let at = fract(center);
            if (in_mask(at) < params.mask_threshold) {
                continue;
            }
            let map_scale = mix(1.0, in_scale(at), params.scale_map);
            let scale = params.size * (1.0 - params.size_random * h2.x) * map_scale / n;
            if (scale.x <= 0.0) {
                continue;
            }
            let turn = -(h2.y - 0.5) * TAU * params.rotation_random - in_rotation(at) * TAU * params.rotation_map;
            var d = uv - center;
            d -= round(d);
            let q = rotate2(d, turn) / scale;
            let k = min(i32(h4.x * f32(params.pattern_count)), params.pattern_count - 1);
            let s = shape_at(q, k) * (1.0 - params.brightness_random * h3.y);
            if (s > top) {
                top = s;
                top_id = 0.02 + 0.98 * h4.y;
                top_pattern = (f32(k) + 1.0) / f32(params.pattern_count);
            }
            if (params.blend == 1) {
                v += s;
            } else {
                v = max(v, s);
            }
        }
    }
    switch params.output {
        case 1: { return vec4<f32>(select(0.0, top_id, top > 0.001)); }
        case 2: { return vec4<f32>(smoothstep(0.0, 0.02, top)); }
        case 3: { return vec4<f32>(select(0.0, top_pattern, top > 0.001)); }
        default: { return vec4<f32>(clamp(v, 0.0, 1.0)); }
    }
}
