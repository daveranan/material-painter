// @name Color to Mask
// @category Color
// @description White where the input is close to a color, for pulling one feature (grass, dirt, moss) out of a scan. Distance is perceptual (OKLab); lower the lightness weight to ignore shading.
// @output gray
// @input input: color = 0.5
// @param color: color = 0.35, 0.45, 0.2
// @param tolerance: float(0, 0.2) = 0.03
// @param softness: float(0, 0.2) = 0.02
// @param lightness_weight: float(0, 1) = 0.3
// @param invert: bool = false

fn node(uv: vec2<f32>) -> vec4<f32> {
    let p = rgb_to_oklab(in_input(uv).rgb);
    let k = rgb_to_oklab(params.color);
    let d = p - k;
    let dist = sqrt(params.lightness_weight * d.x * d.x + d.y * d.y + d.z * d.z);
    var m = 1.0 - smoothstep(params.tolerance, params.tolerance + max(params.softness, 1e-4), dist);
    if (params.invert) {
        m = 1.0 - m;
    }
    return vec4<f32>(m);
}
