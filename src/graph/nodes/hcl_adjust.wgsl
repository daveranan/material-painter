// @name HCL Adjust
// @category Color
// @description Hue, chroma and lightness in OKLCH: turning the hue keeps each pixel's brightness (HSL doesn't). Contrast spreads lightness around mid grey.
// @output color
// @input input: color = 0.5
// @param hue: float(-0.5, 0.5) = 0
// @param chroma: float(0, 2) = 1
// @param lightness: float(-0.5, 0.5) = 0
// @param contrast: float(0, 2) = 1

fn node(uv: vec2<f32>) -> vec4<f32> {
    let c = in_input(uv);
    var lch = rgb_to_oklch(c.rgb);
    lch.x = (lch.x - 0.5) * params.contrast + 0.5 + params.lightness;
    lch.y = lch.y * params.chroma;
    lch.z = fract(lch.z + params.hue + 1.0);
    return vec4<f32>(oklch_to_rgb(lch), c.a);
}
