// @name HCL Split
// @category Color
// @description One OKLCH channel as grey: lightness, chroma (0.4 = white) or hue (a turn around the color wheel).
// @output gray
// @input input: color = 0.5
// @param channel: enum(Lightness, Chroma, Hue) = Lightness

fn node(uv: vec2<f32>) -> vec4<f32> {
    let lch = rgb_to_oklch(in_input(uv).rgb);
    switch params.channel {
        case 1: { return vec4<f32>(clamp(lch.y / 0.4, 0.0, 1.0)); }
        case 2: { return vec4<f32>(lch.z); }
        default: { return vec4<f32>(lch.x); }
    }
}
