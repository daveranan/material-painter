---
name: game-ground-materials
description: How to design AAA-quality tiling ground and terrain materials (soil, sand, gravel, pebbles, rock beds, moss, grass, mud, snow, forest floor, desert) for a game, end to end in Material Painter and Material Designer, from the game's material brief to an in-engine review. Use it whenever a ground, terrain, floor, rock-bed or natural tiling material is being made, cleaned up from scans, combined from several scans, graded into features, scattered (pebbles, leaves, twigs, grass), or judged against a game screenshot; whenever the user mentions feature masks, biome palettes, Helldivers-style grading, height blend, tile sampler, color to mask, flatten, match mean or auto level; and before handing a ground material to the game.
---

# AAA ground materials, Material Painter to game

A ground material reads as AAA when its **height is built first and large to
small**, every element **sits in the ground** (buried, sloped, sheltered)
instead of floating on it, its **color is bland and disciplined** so the
game's palette can grade it, and it was **judged in the engine at the
camera distances players see**, not in a 2D view. Everything below serves
those four.

Pair with `pbr-texturing-craft` (values, restraint) and
`material-painter-agent` (driving MP). Node-by-node recipes are in
`references/recipes.md`; the brief the game sends and what goes back are in
`references/brief.md`; what each source article taught is in
`references/sources.md`.

## 0. Start from a brief, never from a node

Get (or write, then confirm) the brief in `references/brief.md` before
building: the biome, the **three features** (main, mask 1, mask 2), their
allowed color ranges, **tile size and relief in metres**, the camera
heights and distances that matter, what sits next to it (pebble meshes,
cliffs), and a screenshot of the current ground in game with what's wrong.
A material made without the camera distance in mind is detailed in the
wrong band; without the features it can't be graded.

## 1. Plan the layers on paper (5 minutes, saves days)

From references (high-res photos, scans' own previews), write the stack
bottom to top with a **size in centimetres** for each element and the
feature it belongs to:

```
soil base       noise 40-200 cm undulation, 1-3 cm grain   feature: soil (main)
large stones    15-40 cm, partly buried, 4-5 per m²         feature: rock (main or mask2)
pebbles         1-4 cm, fill gaps between stones            feature: pebbles (mask2)
dirt in lows    fills crevices below local mean             feature: dirt (mask1; AO comes from it)
grass tufts     scattered by a mask, never on stones        feature: grass
```

Rules from every breakdown: **big, medium, small**; physical logic decides
placement (stones roll downhill and cluster, sand and dirt settle in lows,
moss grows on sheltered tops, water pools at the lowest level); a rock
covered in sand must still read as a rock; vegetation stays smaller and
simpler than the stones next to it.

Feature mapping: at most three graded features. Put the **crevice/low
feature in mask 1** (the game derives AO from it), the **feature that rises
on top in mask 2**, the dominant surface as main. Untagged layers (color
tweaks, grime, wetness) don't change the masks.

## 2. Height first, and only height, until it reads

Build only height (plus the viewport's displacement and lighting) until the
forms read at the brief's camera distances. In MP: a Plane, Displacement
0.03-0.08 (`preview_settings displacement=`), a low sun.

- **Large to small, falling intensity.** Large and medium forms belong in
  height and normal. Small high-frequency detail goes in **weakly** (low
  opacity, small slope); fine detail is shown in roughness and albedo
  instead. Too much fine height is the most common failure: it turns into
  noise at distance and fights the game's normal mapping.
- **Each layer is its own scatter/shape** (separate tile samplers per size),
  so sizes and layering stay controllable and same-layer stones don't
  intersect. Cell patterns (distance to cell + edges) avoid self-
  intersection better than random splats.
- **Every element sinks in.** Combine with **height blend** (the element
  shows where it rises above what's below); offset each new layer to sit
  just above the previous one and let the ground's undulation bury it
  unevenly. Push the base slightly *under* big elements (a blurred mask of
  them) and soften only the contact zone so junctions don't cut hard.
- **Shape each stone**: random tilt/slope per instance (two random-gradient
  fills, Darken), roundness (blur), then a light noise on top. Rocks stay
  rocks: warps and slope blur destroy hard edges quickly; restore with a
  curvature mask if needed.
- **Fix the scan height range.** Scanned atlases and scans often have a
  tiny height range: Levels them to the brief's relief before blending, or
  they disappear under procedural layers.
- **Check tiling at 3x3** (2D view, or `tiling` 3 on a test layer). A
  material that only looks right at 1x has a seam or a stand-out feature
  the eye will find repeating in game.

Write each element's height, masks and color as a **material script**
(`material-painter-agent/references/material-script.md`) and review it with
`mp script_step step=<name>` after every change: height steps first.
In MP the stack is: one fill per element on a Plane, `height_blend` 0.02-0.1
on each element that should rise through, masks from Designer graphs or
smart masks, each layer tagged with its feature. Within a Designer graph,
use `math` Max/Min and `blend` Lighten/Darken for height combining.

## 3. Masks come from the height

Extract, don't paint:
- **Peaks / valleys**: height minus blurred height (a highpass) through
  `histogram_scan`. Crevice = below the local mean, peak = above.
- **Cavities and contacts**: `ambient_occlusion` on the height.
- **Edges**: `curvature` (convex bright) through `histogram_scan`.
- **Instance masks**: the scatter's own output, before blending (free and
  exact: the reason scatters beat scans).
- **Water level**: Lighten a flat value into the height so the lows fill
  flat, then `histogram_scan` (or `color_to_mask` on the height) at that
  level; move the level and the mask follows.
- Break every mask's regularity by subtracting a grunge/noise at low weight.

## 4. Color: bland, disciplined, per feature

The game grades each feature with its palette, so author **one neutral
color per feature inside its allowed range** (from the brief) and keep
variation small. Exaggerated or contrasty color breaks under grading.

- Per feature: a `gradient_map` from height or masks (dark crevice, mid,
  light top), plus per-instance variation (scatter brightness random into
  the gradient) of a few percent of lightness, not hue swings.
- **Scans** (always run this chain; it's the Helldivers 2 cleanup):
  1. `color_to_mask` to split pre-blended features (key on the feature's
     measured average color, tight tolerance; scans are low-chroma, features
     often differ by 0.02-0.04 in OKLab).
  2. `hcl_flatten` each feature toward its mean (lightness 0.2-0.4, chroma
     0.5-0.9, hue 0.5-1) to narrow its range.
  3. `match_mean` scans that are the same feature onto one reference, so
     dirt from two scans is one dirt.
  4. `hcl_adjust` for hue moves (lightness stays put), never `hsl` for
     hue.
  5. `auto_level` scan roughness maps and noises before blending (never
     height).
- Curvature and AO into color only at very low strength (edge lift, crevice
  darkening a few percent); the game adds its own AO from mask 1.
- Check value: no albedo under ~0.04 or over ~0.85 linear-ish
  (`pbr-texturing-craft/references/pbr-values.md`); dry soil and sand are
  brighter than people paint them.

## 5. Roughness, by material, not by noise

Build it after height and before final color (it's easier to judge without
color). The game takes roughness **per feature from the palette**, so in the
export roughness is only a preview aid; spend effort on it only when the
brief says roughness is exported. Ordering to aim for: puddles and wet mud
glossiest, fresh plants glossy, stones slightly glossy with grain, dry soil
and sand matte, dry leaves dullest. Never put a dry stone next to wet
vegetation.

## 6. Review in MP, then in the game

In MP (fast loop):
- Plane, three views: top-down, the brief's player-eye angle, a grazing
  angle; Displacement on; the key light low (20-35°) and then high noon.
- 3x3 tiling check; the 3D view's mode spheres for Height, each mask, and
  roughness; `color_nodes` style numbers for coverage per feature (aim for
  the brief's ratios).
- Self-review with `pbr-texturing-craft` §review.

In the game (the real judge): export with the "Game material (features)"
template, then render the game's fixed cameras (see `references/brief.md`,
"What comes back") with palette off, each biome palette, and the features
view. Compare to the brief's reference and the previous screenshot; fix
the biggest problem first (scale and tiling, then height read, then color
range, then detail).

## Roadmap: what production graphs need

Done: Tile Sampler with four pattern inputs, mask (tile-centre sampling),
scale and rotation maps, Bell/Paraboloid/Gaussian/Pyramid/Capsule shapes and
per-instance outputs (random ID, mask, pattern index); `distance` (spread a
value or distance, jump flooding); `flood_fill` + `flood_fill_to` (random,
gradient tilt, size, position per shape); `highpass`; `nonuniform_blur`;
`multi_warp`; `auto_levels` (min/max); `height_blend` (height or mask);
`shadows`; `facing`.

Still missing: layered scattering that settles each stone layer around the
last automatically (do it by hand: mask the next tile sampler with the
`shadows` or `ambient_occlusion` of the layers so far), Normal to Height,
a non-uniform directional warp, world-unit normal and AO, a graph normal
output. Each new node becomes callable from material scripts as soon as it exists
(the script language calls any node by its id).

## Gaps in MP today and what to do instead

Missing compared with Substance (see `references/recipes.md` for
workarounds): a Designer **height-blend node** (use the layer stack's
`height_blend`), **atlas scatter / tile sampler with color, masks and
per-instance outputs** (scatter grey shapes, color them by gradient map;
for scan atlases cut pieces into separate images), **flood fill** (cells
Random gives per-cell values, not per-shape), **non-uniform blur**,
**normal to height**, **shape splatter**, a **normal output** from graphs
(normal comes from height). Say so in the review when a gap limits the
result rather than faking it.
