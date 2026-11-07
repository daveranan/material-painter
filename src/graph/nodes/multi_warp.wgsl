// @name Multi-Directional Warp
// @category Filters
// @description Warps the input by a map in several directions in turn (evenly spread around the circle), so it bends and breaks up without drifting one way: organic noise, eroded stone edges.
// @output color
// @input input: color = 0.5
// @input warp: gray = 0.5
// @param intensity: float(0, 1) = 0.05
// @param directions: int(2, 8) = 4
// @param angle: float(-180, 180) = 0

fn node(uv: vec2<f32>) -> vec4<f32> {
    var p = uv;
    let count = params.directions;
    for (var k = 0; k < 8; k++) {
        if (k >= count) {
            break;
        }
        let a = radians(params.angle) + f32(k) * TAU / f32(count);
        p += vec2<f32>(cos(a), sin(a)) * in_warp(p) * params.intensity / f32(count);
    }
    return in_input(fract(p));
}
