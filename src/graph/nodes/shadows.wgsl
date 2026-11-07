// @name Shadows
// @category Filters
// @description Where a height map shadows itself under a low light: white lit, black in shadow. Use as a mask so small stones settle in the lee of big ones, or dust gathers on one side.
// @output gray
// @input height: gray = 0.5
// @param angle: float(-180, 180) = 45
// @param elevation: float(1, 89) = 25
// @param depth: float(0.001, 0.5) = 0.05
// @param distance: float(0.01, 0.5) = 0.1
// @param softness: float(0, 1) = 0.5

fn node(uv: vec2<f32>) -> vec4<f32> {
    // March toward the light; the height is `depth` uv units tall.
    let a = radians(params.angle);
    let dir = vec2<f32>(cos(a), sin(a));
    let rise = tan(radians(params.elevation));
    let h0 = in_height(uv) * params.depth;
    let steps = 48;
    var lit = 1.0;
    for (var i = 1; i <= steps; i++) {
        let t = params.distance * f32(i) / f32(steps);
        let ray = h0 + t * rise;
        let ground = in_height(fract(uv + dir * t)) * params.depth;
        let blocked = (ground - ray) / max(params.depth * 0.1, 1e-5);
        lit = min(lit, clamp(1.0 - blocked * (1.0 - params.softness * 0.9), 0.0, 1.0));
    }
    return vec4<f32>(lit);
}
