// @name Facing
// @category Filters
// @description How much each slope of a height map faces a direction: bright toward it, dark away, mid grey on flat ground. Edge highlights, wind-side dust, sun-bleached faces.
// @output gray
// @input height: gray = 0.5
// @param angle: float(-180, 180) = 45
// @param intensity: float(0, 64) = 8

fn node(uv: vec2<f32>) -> vec4<f32> {
    let e = px();
    let gx = in_height(uv + vec2<f32>(e, 0.0)) - in_height(uv - vec2<f32>(e, 0.0));
    let gy = in_height(uv + vec2<f32>(0.0, e)) - in_height(uv - vec2<f32>(0.0, e));
    let a = radians(params.angle);
    // Uphill points against the gradient: a slope faces where it falls away.
    let facing = -(gx * cos(a) + gy * sin(a)) * params.intensity * res() / 512.0;
    return vec4<f32>(clamp(0.5 + facing, 0.0, 1.0));
}
