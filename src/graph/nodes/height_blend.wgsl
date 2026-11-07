// @name Height Blend
// @category Blend
// @description Puts the top height over the bottom one only where it rises above it, with a soft edge: stones sunk in sand, pebbles buried in mud. The mask lowers the top where it's dark. Outputs the combined height or where the top shows.
// @output gray
// @input top: gray = 0.5
// @input bottom: gray = 0.5
// @input mask: gray = 1
// @param contrast: float(0.001, 0.5) = 0.05
// @param offset: float(-1, 1) = 0
// @param output: enum(Height, Mask) = Height

fn node(uv: vec2<f32>) -> vec4<f32> {
    let top = in_top(uv);
    let bottom = in_bottom(uv);
    let m = in_mask(uv);
    // As the layer stack's height blend: the mask sinks the top layer.
    let d = top - bottom + params.offset + (m - 1.0) * 0.5;
    let t = select(clamp(d / params.contrast * 0.5 + 0.5, 0.0, 1.0), 0.0, m <= 0.0);
    if (params.output == 1) {
        return vec4<f32>(t);
    }
    return vec4<f32>(mix(bottom, top, t));
}
