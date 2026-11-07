# Recipes: Substance techniques in Material Painter

Material Designer graphs live in the project JSON under `"graphs"`
(`{"id": 1, "graph": {"name", "res", "nodes": [{"id", "def", "params",
"links": {"input": node_id}}]}}`); a fill uses one with `"graph": 1` and a
mask with `"mask": {"graph": 1}`. The graph's `output` nodes say what each
output is (Base Color, Roughness, Metallic, Height, Mask). Edit the JSON and
the running app reloads it. Custom nodes are WGSL files in
`%APPDATA%/MaterialPainter/nodes/` (header format: any file in
`src/graph/nodes/`); add `// @mean` to read `mean_<input>()`.

## Substance node → MP

| Substance | MP | Notes |
|---|---|---|
| Height Blend | layer stack `height_blend` on a fill | Not a graph node yet. In a graph, approximate with `math` Max (the taller wins) or `blend` Lighten with a mask. |
| Tile Sampler | `tile_sampler` | Grid scatter, random position/size/rotation/brightness, gaps (density), Max/Add. Grey only, one pattern, no mask or scale/vector map inputs. Run it twice with the same seed: once for height, once with brightness random for per-instance values. |
| Atlas Scatter, Shape Splatter | none | Cut atlas pieces into separate images and stack fills; or scatter grey shapes and color by gradient map. |
| Distance + Edge Detect (cell layout) | `cells` (Distance, Edges) | Edges mode is the cell-boundary mask. |
| Flood Fill to Random Grayscale | `cells` Random | Per cell, not per arbitrary shape. |
| Flood Fill to Gradient (per-stone slope) | `cells` Distance + `gradient` + `math` | Approximate: cells Random as angle source is not available; use two `gradient`s at different angles masked by `cells` Random through `histogram_scan`, combined with Darken. |
| Highpass Grayscale | `math` Subtract (input, `blur` of input) + 0.5 | Use for peak/valley masks and evening a height. |
| Slope Blur | `slope_blur` | Small intensities (0.02-0.08); it softens hard edges fast. |
| Directional Warp / Warp | `directional_warp` / `warp` | |
| Non-Uniform Blur | `blur` blended by a mask | Approximation. |
| Clouds, Perlin, Fractal Sum | `noise` (Perlin, Value, Turbulence, Ridged) | Scale = cells across the tile. |
| BnW Spots, Grunge maps | `noise` Turbulence/Ridged through `histogram_scan`; `scratches` | |
| Gradient Map | `gradient_map` (3 colors) | Chain two with a mask for more keys. |
| Gradient Dynamic | `gradient_map` from `cells` Random | Per-instance color. |
| HSL | `hcl_adjust` | OKLCH: hue moves keep lightness. Keep `hsl` only for old graphs. |
| Color Match | `match_mean` | Matches averages, keeps the hue range. |
| Color To Mask | `color_to_mask` | OKLab distance; lower `lightness_weight` to ignore shading. |
| Histogram Scan | `histogram_scan` | |
| Levels / Auto Levels | `levels` / `auto_level` | `auto_level` centres the average; not for height. |
| Ambient Occlusion (HBAO) | `ambient_occlusion` | |
| Curvature, Facing Normal edges | `curvature` + `histogram_scan` | |
| Quantize Grayscale | `posterize` | Terraced shapes. |
| Transformation 2D / Safe Transform | `transform` | Wraps, so whole repeats stay tileable. |
| Normal to Height HQ, Shadow, Vector Warp | none | |

## Pebbles that sit in mud (riverbed, sand + pebbles)

Layer stack on a Plane (each a fill; masks from small graphs):
1. **Mud/sand base** (main): fill with a height graph: `noise` Perlin scale 3
   (large undulation) + `noise` Perlin scale 12 at 0.3 + `noise` Turbulence
   scale 48 at 0.1 (`math` Add, then `levels`). Color from `gradient_map` of
   the same height, narrow range.
2. **Pebbles** (mask 2): fill whose height graph is `tile_sampler` Disc,
   columns/rows 12-20, size 0.6-0.9, size random 0.4, position random 0.8,
   rotation random 1, gaps 0.3, then `blur` 2 for roundness and a weak
   `noise` on top. `height_blend` 0.04-0.06: the mud's undulation buries them
   unevenly. Second, smaller pebble layer with another seed and size 0.5.
3. **Dirt in lows** (mask 1): flat fill, `height_blend` 0.03, mask from
   `histogram_scan` of (height minus `blur` 12 of height), inverted.
Feature tags: sand (main), dirt (mask 1), pebbles (mask 2).

## Stones on soil (rocky ground)

1. Soil base as above (main).
2. Large stones: `cells` Distance scale 4-6 → `levels` (round tops) → `math`
   Multiply with `cells` Edges inverted (separation) → `slope_blur` 0.03 with
   a `noise` → add weak `noise`. Fill with it as height, `height_blend` 0.05.
   Mask the stones to 40-60% coverage with `cells` Random through
   `histogram_scan` so not every cell is a stone.
3. Medium stones: the same at scale 10-14, masked to the gaps (inverted
   large-stone height through `histogram_scan`).
4. Gravel/pebbles: `tile_sampler` at 24-40 columns, `height_blend` 0.03.
Tag stones rock (main or mask 2 by brief), gravel/pebbles as their feature.

## Grass or clover only on the ground, never on stones

Scatter mask = the stone layer's height through `histogram_scan`, inverted
(the "ground mask"). Use it as the mask of the grass fill, multiplied by a
`noise` patch mask. Grass height kept low (it's a different scale), blurred
1-2 texels so blades bend into the ground.

## Splitting a pre-blended scan (grass and rubble, moss on rock)

Graph from the scan's base color (`bitmap`):
1. Measure the feature's average color first (sample the greenest/most
   typical tenth of pixels); key `color_to_mask` on that, tolerance
   0.01-0.02, softness 0.01, `lightness_weight` 0.1.
2. Clean: `blur` 1, `histogram_scan` to firm it, subtract weak noise.
3. Output it as **Mask** and use it as the mask of a fill tagged with that
   feature (the fill carries the scan's channels too, so only the feature's
   texels change ownership).
4. Color each feature: `hcl_flatten` the scan, then `match_mean` onto the
   feature's reference color swatch (a `color` node).

## Water and puddles

Height graph: the ground height, `blend` Lighten with a flat `color` at the
water level (only the lows rise to it, flat). Mask: `histogram_scan` of the
result at that level (or `color_to_mask` on height). In the stack: a
wetness fill (darker, roughness 0.05-0.15) masked by it, untagged (wetness
is not a feature).

## Dunes

`noise` Perlin scale 2-4 → `directional_warp` by itself (intensity 0.1-0.2)
→ `slope_blur` 0.05 → ripples: `gradient` repeat 30-60 warped by a `noise`,
limited to crests with `histogram_scan` of the dune height, added at 0.1.
Two dune graphs at different angles blended by a low-frequency `noise`
give flow variation (Benainous, Hodgson).

## Scale and texel density

Tile size in metres comes from the brief (2 m is common for ground). At
1024² over 2 m, one texel is 2 mm; a 2 cm pebble is 10 texels; anything
under ~4 texels belongs in roughness/albedo, not height. Scale MP sizes
from the brief, not from what looks good zoomed in.
