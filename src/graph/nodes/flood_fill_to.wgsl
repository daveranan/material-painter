// @name Flood Fill To
// @category Filters
// @description Turns a Flood Fill into one value per shape: a random grey, a gradient across the shape at a (random) angle for per-stone tilt, the shape's size, or its position. Black outside shapes.
// @output gray
// @input flood: color = -1
// @param mode: enum(Random, Gradient, Size, Position X, Position Y) = Random
// @param angle: float(-180, 180) = 0
// @param angle_random: float(0, 1) = 1
// @param random_min: float(0, 1) = 0
// @param random_max: float(0, 1) = 1
// @param size_scale: float(1, 64) = 8
// @param seed: int(0, 1000) = 0

fn node(uv: vec2<f32>) -> vec4<f32> {
    let b = in_flood(uv);
    if (b.x < 0.0) {
        return vec4<f32>(0.0);
    }
    // Bounding box in uv; the corner identifies the shape (rounded to the
    // texel, as every texel of the shape computes the same corner).
    let lo = uv - b.xy;
    let hi = uv + b.zw;
    // Wrapped, so a shape crossing the tile edge is one shape on both sides.
    let id = wrap_cell(floor(lo * res() + 0.5), res());
    let r = hash2(id, f32(params.seed) * 7.0 + 3.0);
    let size = hi - lo;
    switch params.mode {
        case 1: {
            let a = radians(params.angle) + (r.y - 0.5) * TAU * params.angle_random;
            let dir = vec2<f32>(cos(a), sin(a));
            let extent = abs(size.x * dir.x) + abs(size.y * dir.y);
            let g = dot(uv - (lo + hi) * 0.5, dir) / max(extent, 1e-5) + 0.5;
            return vec4<f32>(clamp(g, 0.0, 1.0));
        }
        case 2: {
            return vec4<f32>(clamp(max(size.x, size.y) * params.size_scale, 0.0, 1.0));
        }
        case 3: {
            return vec4<f32>(fract((lo.x + hi.x) * 0.5));
        }
        case 4: {
            return vec4<f32>(fract((lo.y + hi.y) * 0.5));
        }
        default: {
            return vec4<f32>(mix(params.random_min, params.random_max, r.x));
        }
    }
}
