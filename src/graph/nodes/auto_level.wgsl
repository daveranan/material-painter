// @name Auto Level Balance
// @category Adjust
// @description Shifts a grey map so its average sits at mid grey: about as much lighter as darker. For noises before blending and scan roughness maps; not for height (it moves the profile).
// @mean
// @output gray
// @input input: gray = 0.5
// @param mid: float(0, 1) = 0.5
// @param amount: float(0, 1) = 1

fn node(uv: vec2<f32>) -> vec4<f32> {
    let v = in_input(uv) + (params.mid - mean_input()) * params.amount;
    return vec4<f32>(clamp(v, 0.0, 1.0));
}
