// @name HCL Merge
// @category Color
// @description A color from OKLCH channels as HCL Split gives them: lightness, chroma (0.4 = white) and hue.
// @output color
// @input lightness: gray = 0.5
// @input chroma: gray = 0
// @input hue: gray = 0

fn node(uv: vec2<f32>) -> vec4<f32> {
    let lch = vec3<f32>(in_lightness(uv), in_chroma(uv) * 0.4, in_hue(uv));
    return vec4<f32>(oklch_to_rgb(lch), 1.0);
}
