// @name Auto Levels
// @category Adjust
// @description Stretches a grey map so its darkest texel is black and its brightest white: keeps every layer of a height using the full range before the next blend.
// @minmax
// @output gray
// @input input: gray = 0.5

fn node(uv: vec2<f32>) -> vec4<f32> {
    let lo = min_input();
    let hi = max_input();
    return vec4<f32>((in_input(uv) - lo) / max(hi - lo, 1e-5));
}
