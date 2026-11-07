// @name Highpass
// @category Filters
// @description Keeps only detail smaller than the radius, around mid grey: evens out a height (no big slopes left) and gives peak and valley masks through a Histogram Scan.
// @output gray
// @input input: gray = 0.5
// @param radius: float(1, 64) = 8
// @param contrast: float(0, 4) = 1

fn node(uv: vec2<f32>) -> vec4<f32> {
    // The same soft disc as Blur, subtracted.
    let n = 48;
    var sum = 0.0;
    var wsum = 0.0;
    for (var i = 0; i < n; i++) {
        let r = sqrt((f32(i) + 0.5) / f32(n));
        let a = f32(i) * 2.39996323;
        let o = vec2<f32>(cos(a), sin(a)) * r * params.radius * px();
        let w = exp(-r * r * 2.0);
        sum += in_input(uv + o) * w;
        wsum += w;
    }
    return vec4<f32>((in_input(uv) - sum / wsum) * params.contrast + 0.5);
}
