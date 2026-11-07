// @name Match Mean
// @category Color
// @description Moves the input's average color onto the reference's average (in OKLab), keeping the input's variation: makes scans that should be one feature the same color.
// @mean
// @output color
// @input input: color = 0.5
// @input reference: color = 0.5
// @param lightness: float(0, 1) = 1
// @param color: float(0, 1) = 1

fn node(uv: vec2<f32>) -> vec4<f32> {
    let c = in_input(uv);
    let shift = rgb_to_oklab(mean_reference().rgb) - rgb_to_oklab(mean_input().rgb);
    let lab = rgb_to_oklab(c.rgb) + shift * vec3<f32>(params.lightness, params.color, params.color);
    return vec4<f32>(oklab_to_rgb(lab), c.a);
}
