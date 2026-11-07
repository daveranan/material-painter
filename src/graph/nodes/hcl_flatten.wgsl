// @name HCL Flatten
// @category Color
// @description Pulls lightness, chroma and hue toward the image's average, narrowing the range of each: the quick way to make a scan bland enough to grade.
// @mean
// @output color
// @input input: color = 0.5
// @param lightness: float(0, 1) = 0.3
// @param chroma: float(0, 1) = 0.5
// @param hue: float(0, 1) = 0.5

fn node(uv: vec2<f32>) -> vec4<f32> {
    let c = in_input(uv);
    let lab = rgb_to_oklab(c.rgb);
    let mean = rgb_to_oklab(mean_input().rgb);
    let l = mix(lab.x, mean.x, params.lightness);
    let ch = mix(length(lab.yz), length(mean.yz), params.chroma);
    // Hue: turn each pixel's (a, b) direction toward the mean's.
    let dir = normalize(mix(normalize(lab.yz + 1e-6), normalize(mean.yz + 1e-6), params.hue) + 1e-6);
    return vec4<f32>(oklab_to_rgb(vec3<f32>(l, dir * ch)), c.a);
}
