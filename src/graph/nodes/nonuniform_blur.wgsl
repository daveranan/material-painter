// @name Non-Uniform Blur
// @category Filters
// @description Blurs more where the map is white and not at all where it's black: soften only where shapes meet, round pebbles, settle mud around stones.
// @output color
// @input input: color = 0.5
// @input map: gray = 1
// @param radius: float(0, 64) = 8

fn node(uv: vec2<f32>) -> vec4<f32> {
    let radius = params.radius * clamp(in_map(uv), 0.0, 1.0);
    if (radius < 0.5) {
        return in_input(uv);
    }
    let n = 48;
    var sum = vec4<f32>(0.0);
    var wsum = 0.0;
    for (var i = 0; i < n; i++) {
        let r = sqrt((f32(i) + 0.5) / f32(n));
        let a = f32(i) * 2.39996323;
        let o = vec2<f32>(cos(a), sin(a)) * r * radius * px();
        let w = exp(-r * r * 2.0);
        sum += in_input(uv + o) * w;
        wsum += w;
    }
    return sum / wsum;
}
