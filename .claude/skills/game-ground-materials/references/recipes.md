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

## From real production graphs (Meere 2017, Habibi 2025)

Read off the authors' graph screenshots. Substance node names, with the MP
translation where one exists; "(missing)" marks what MP can't do yet (see
the roadmap in `SKILL.md`).

**Cracks (Meere).** Tile Sampler of sparse tiny dots → Levels clamped to
white → **Distance** (source only, distance cranked up: every texel takes
its nearest dot's value, Voronoi regions) → Histogram Scan → Edge Detect =
crack lines. *Islanding fragments*: Edge Detect with Edge Roundness → Blur
HQ → Histogram Scan (blur and contrast remove the thin lines, leaving
islands) → Invert, blended with the crack lines. *Micro cracks*: the
Distance output through a Directional Warp (smaller, wigglier cracks) and a
Levels, blended in; a Histogram Scan of Distance gives a mask covering a
whole "chunk". MP today: `cells` Edges gives the crack lines directly; the
islanding is `blur` + `histogram_scan` on it; per-chunk masks need
Distance (missing).

**Multi-directional warp (Meere).** Creased → Safe Transform → Blur HQ is
the warp map; Clouds 2 goes through four Directional Warps of the same
intensity, each 90° from the last, then they're blended: the image warps
without drifting one way. MP: four `directional_warp` nodes at angles 0,
90, 180, 270 in sequence, same intensity.

**Grass blade (Meere).** Shape (bell) → Levels → Safe Transform mirror +
Blend (a pointed blade) → multiply by Gradient Linear (fades to the tip) →
Transformation 2D (squeeze thin) → Levels → two Directional Warps (bend) →
Transformation 2D (place). Blades go through Splatter Circular, then Tile
Samplers, oriented in one general direction. MP: `shape` Cone + `gradient`
+ `math` Multiply + `directional_warp` ×2; the thin squeeze needs a
non-uniform scale (missing).

**Lowpass feathering (Meere).** Auto Levels → Blur 6 → Blur 3 → Blur 1.5,
each blended back over the previous result with Copy at about 0.56, 0.43,
0.5, then the whole thing over the original at Copy 0.1. Softens height and
normal transitions without losing detail. MP: `blur` 6/3/1.5 chained,
`blend` Normal at those opacities.

**Water level (Meere).** Uniform Color (the water level as a grey) blended
into the height with Lighten (lows fill flat) → Gradient Map → Color to
Mask (a colour parameter picks the level) → Blur HQ → Histogram Scan = the
water mask. Exposing the uniform colour moves water and mask together. MP:
`color` + `blend` Lighten + `histogram_scan` at the level.

**Graph layout (Habibi).** Frames in order: Noises (shared noises sent
everywhere), Cliff pattern, Large Stones, Mid Stones, Ground, Stones
Scattering, Surface Detail(s), Scan Data, Color Palette (five uniform
colours: dark slate, grey-tan, blue-grey stone, sand beige, a cool
highlight), PBR outputs. Shared noises are computed once and reused, which
keeps every layer's breakup consistent.

**Large stones (Habibi).** Three base shapes (Cube 3D, Shape, Polygon 2,
each blended into a bevelled stone) → one Tile Sampler with the three as
pattern inputs (random pick) → Auto Levels → blended with Cells 1 → Auto
Levels → three Multi-Directional Warps driven by transformed noises (BnW
spots, Clouds) → Histogram Scan → **Flood Fill** → two Flood Fill to
Gradient (per-stone tilt, different seeds) → Levels → blended → Blur HQ →
Non-Uniform Directional Warp → Slope Blur by Clouds 2 → Auto Levels →
Non-Uniform Directional Warp by the cliff pattern → another Flood Fill to
Gradient through a Gradient Map → Auto Levels. A stone is a shape, tilted
per instance, warped, eroded twice, re-levelled after each step. Mid stones
repeat this with fewer steps.

**Stone scattering with Shape Splatter (Habibi).** Stones_Large goes into
eight inputs, each through a Transformation (squash on X or Y, since Shape
Splatter has no size random) and Auto Crop to isolate one stone. The
splatter's height output (32-bit) → Auto Levels; its mask output → Flood
Fill to Random Grayscale → Histogram Scan (keep some stones) → **Ambient
Occlusion (HBAO)** → Invert → Levels: that becomes the mask and background
for the *next* splatter, so the next, smaller stones settle around and
against the first. Mid stones repeat it with a **Shadows** node instead of
AO, the smallest at 512² to save memory. The masks combine into Mask_Large
and Mask_Mid_Small for colour. MP today: separate `tile_sampler` layers in
the layer stack with `height_blend`, each masked by `ambient_occlusion` of
the stack below, is the closest.

**Surface detail (Habibi).** Clouds 2 → Slope Blur → Non-Uniform Blur →
Highpass → blended through a Histogram Scan mask. Then Fractal Sum Base
blended with Clouds 1 → a Gradient Map into a fake normal → Normal to
Height HQ (crisp micro relief) blended in; BnW Spots 2 → Highpass →
Histogram Scan → Slope Blur → Blur HQ blended, masked away from the stones
by a Histogram Scan of the stone height; final Auto Levels = Final_Height.

**Outputs in world units (Habibi).** Surface size 200 cm and depth 24 cm
drive Height to Normal (World Units) → Sharpen → Height Normal Blender (a
BnW Spots detail normal on top) → Normal Normalize; AO from HBAO and RTAO
with the same world units; the height output gets a light Non-Uniform Blur;
height scale 12 (half the depth). MP: tile size and relief live in the
texture set's features (`tile_m`, `relief_m`); the normal comes from height
with `height_strength`.

**Albedo (Habibi).** Facing Normal and a Light node on the final normal
make lighting-direction masks; with the height and Auto Levels they feed a
stack of **Gradient Maps** (dark soil, light soil, blue-grey stone, warm
stone), each followed by HSL and blended through masks (Mask_Scattering via
Histogram Shift + Scan, Mask_Mid_Small). Moisture Noise → Directional Warp →
Non-Uniform Directional Warp blends damp and dirty patches. Speckles:
Highpass → Histogram Scan → Gradient Map → HSL (reddish grains); Grunge Map
001 warped by the height → Highpass → Histogram Scan → Gradient Map → HSL
(light grains); Clouds/BnW warped by the scatter mask → Highpass →
Non-Uniform Directional Warp → Histogram Scan → Gradient Map. Finish:
Contrast/Luminosity → HSL. MP: `gradient_map` per material zone, then
`hcl_adjust` (not HSL), blended by masks from height; speckles as `noise` →
highpass (`math` Subtract of `blur`) → `histogram_scan` → `gradient_map`.

## Tile Sampler, the reference (Adobe docs)

Inputs: Pattern Input 1-6 (custom shapes, picked by Pattern Input Number
and Distribution: Random, sequence, or a Distribution Map), Scale Map,
Displacement Map, Rotation Map, Vector Map (non-uniform scale and
displacement direction), Color Map (per-tile tint), Mask Map (hides
tiles), Pattern Distribution Map, Background.

Parameters: X/Y Amount (0-64); Pattern (Image Input, Square, Disc,
Paraboloid, Bell, Gaussian, Thorn, Pyramid, Brick, Gradation, Waves, Half
Bell, Ridged Bell, Crescent, Capsule, Cone) with Pattern Specific (and its
random); stepped Rotation and Rotation Random; Symmetry Random (random
mirroring, H/V); Size Mode (Normal, Keep Ratio, Absolute, Pixel), Size,
Size Random; Scale (0-10), Scale Random, Scale Map and Vector Map
multipliers (X, Y or both); Position Random (0-10), Offset (quincunx or
global), Displacement Map intensity and angle; Rotation, Rotation Random,
Rotation Map multiplier; Mask Map Threshold, Invert, Sampling (pattern
centre or bounding box: no half tiles), Mask Random (and invert); Blending
Mode Add/Sub or Max (grey), Add/Sub or Alpha Blend (colour); Color,
Color/Luminance Random; Color Parametrization (by input colour, scale, row
or line index, pattern index; the colour version also by pattern centre
position or bounding size) and its multiplier; Global Opacity; Background
Color; Reverse Rendering Order.

What matters for ground: **several pattern inputs with random pick** (rock
variety), **Mask Map with bounding-box sampling** (scatter only in the gaps
or only on ground, no half stones), **Scale and Rotation maps** (stones
smaller and aligned on slopes), **Luminance Random** (per-stone height and
colour variation), **Color Parametrization by pattern index** (a clean
per-instance ID for masks and colour), **Max** blending (stones don't add
through each other). MP's `tile_sampler` today has one pattern on a grid,
size/position/rotation/brightness random, gaps, Max/Add.
