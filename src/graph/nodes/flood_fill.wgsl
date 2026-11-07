// @name Flood Fill
// @category Filters
// @description Finds each separate white shape of a mask (stones, cells, tiles) and its bounding box, for Flood Fill To: a random value, tilt or size per shape. Shapes must be separated by black. Raise the iterations for shapes wider than that many texels.
// @passes iterations
// @output color
// @input input: gray = 0
// @param threshold: float(0, 1) = 0.5
// @param iterations: int(8, 512) = 128

// Each texel of a shape stores its distances (uv) to the shape's bounding
// box edges: left, top, right, bottom. Every pass takes the farthest edges
// its 8 neighbours in the same shape know, one texel at a time, so it never
// jumps across a gap into another shape. Outside shapes: -1.
fn node(uv: vec2<f32>) -> vec4<f32> {
    let p = vec2<i32>(uv * res());
    let inside = in_input(uv) > params.threshold;
    if (!inside) {
        return vec4<f32>(-1.0);
    }
    if (pass_index() == 0) {
        return vec4<f32>(0.0);
    }
    var b = prev_at(p);
    let t = 1.0 / res();
    for (var k = 0; k < 9; k++) {
        if (k == 4) {
            continue;
        }
        let o = vec2<i32>(k % 3 - 1, k / 3 - 1);
        let n = prev_at(p + o);
        if (n.x < 0.0) {
            continue;
        }
        // The neighbour's edges, seen from here: it sits `d` away, so the
        // left and top edges are `d` closer to it than to us.
        let d = vec2<f32>(o) * t;
        b = max(b, vec4<f32>(n.x - d.x, n.y - d.y, n.z + d.x, n.w + d.y));
    }
    return b;
}
