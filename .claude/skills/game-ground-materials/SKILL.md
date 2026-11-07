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
- **Size raised features for the game's displacement grid** (about 5 cm
  between vertices, sampled at a 7-9 cm mip; see 2b). Roots, branches and
  rocks wider than ~5 cm carry a soft swell in the displacement height
  (roots 6-10 cm wide standing 5-8 cm); anything thinner (twigs, grit,
  small pebbles) is normal detail only. Thin detail looks 3D only when big
  shapes beside it carry the relief.
- **Contact shading in the material.** Darken each raised feature's low
  flanks and a ring of soil beside it (AO from its own height); the game's
  shadow map is far too coarse for centimetre shadows.
- **Low contrast between features.** Roots and stones near the soil's
  value read as shape and shadow; bright sticks on dark soil read as a
  pattern.
- **Whole-number repeats only.** A layer tiling or bitmap repeat of 1.5 breaks
  the material at its tile border. Change feature size with integer repeats
  (or a larger `tile_m`), never a fractional tiling.
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

## 2b. Split displacement from detail (the HD2 texture set)

What reaches the screen in the game is not the height you painted: it loads
the material at 1024 and samples the height at the mip its ~5 cm vertex
grid picks (`lod = log2(step * 1.5 / texel)`; for a 3 m tile that is mip
~4.7, 7-9 cm per texel). Crisp 6 cm roots and stones lose 60-80% of their
rise there. Helldivers 2's ground sets solve this, and so do ours now:

- **One soft height for displacement.** A single bottom layer (main
  feature, usually "Ground") owns the exported height: low, flat soil
  mounds (`soil_soft`: 1 m and 30 cm noise, 0.08-0.2) plus a **swell**
  under every big feature: the feature's crisp height above the soil,
  blurred 4-6 cm and gained so it keeps its rise through the mip
  (`swell(f, s, radius, gain)`; radius 14-40 texels at 2048 per 3 m,
  gain 1.5-2.4). Blobs wider than the pieces, never the pieces themselves.
- **All detail as normal.** Every other layer turns on **Height as normal**
  (`height_detail: true`): its height shades the normal map at the export's
  normal-from-height scale and writes no height. A "Ground detail" layer
  (soil grain, twig litter, gravel, pebbles; colour off, normal strength
  ~2) plus one layer per feature whose crisp height gives the sharp edges.
- **Crisp, chunky masks.** Each feature layer's mask is near-binary
  (`histogram_scan` contrast 0.9+ at the end) and agrees with its height:
  if the height builds a root only where a value passes 0.5, the mask must
  cut at the same 0.5. Soft grey masks make the colour, the masks and the
  displacement disagree (roots in the mask where the height has none).
- **A separate AO map.** The game template writes `$project_AO`, ambient
  occlusion from the final height at the material's real tile and relief
  (white open). Don't bake strong contact AO into colour as well; keep
  colour contrast moderate.
- **A separate displacement map (contract v2).** The game displaces its
  grid with `$project_Displacement.png` when the manifest lists it, not
  with the blend height. Make it with a **Displacement layer** (a fill with
  `displacement: true`; colour and roughness off, height on): only these
  layers draw in the displacement pass, where 0..1 spans the features'
  `displacement_m` (0.3 for our forest set). **The features carry it**:
  each root and stone gets a rounded profile from its distance to its own
  edge (`distance(invert(mask), max_distance=0.012-0.02) | levels(gamma=2.5)`,
  times the stone's own profile so touching stones stay separate bumps), so
  wide pieces stand tall and thin ones barely rise. Targets, per feature
  mask: roots p50 6-8 cm along their length, stones 5-10 cm scaled with
  size, soil 0, mounds and ruts +-2 cm only (wide mounds are an invisible
  slope and eat the range). Nothing under ~8 cm wide. No clipping: the soil
  sits at 0.5, so the tallest piece must stay under displacement_m / 2;
  raise displacement_m and scale the layer down rather than clip. The
  export rounds it (about a 1.8 cm Gaussian), sets the soil's median to
  0.5, writes 16 bits and adds the forms' slopes to the normal map. Measure
  each feature's lift from the exported map after every change. Preview with `preview_settings
  show_displacement=true`. With the displacement apart, the blend height
  (ColorHeight alpha) can carry full HD2-style detail again: tapered,
  stacked pieces, moderate contrast, not soft swells.
- **Rest height.** The manifest's `rest_height` is the main feature's
  median height (0..1, as stored). Engines should lift displacement from it,
  not the tile mean, so soil sits at rest and features rise above it.
- Exported fills: `height` 1.0 (0.5 halves the exported range), metallic
  off, `tiling` 1, whole-number bitmap repeats only.

Verify against the game, not the 2D view: `python tools/game_sim.py
<export folder> <name>...` reports each feature's lift above `rest_height`
after the game's mip, in cm. Aim for features p50 ≥ 2.5 cm, p90 4-7 cm,
soil within ±1 cm of rest. Below that, widen or gain the swell; never add
crisp height (it vanishes in the mip and fights the normal map).

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
  darkening a few percent); the exported AO map carries the contact shading.
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
a non-uniform directional warp, world-unit normal (AO at world scale is the
export's AO map), a graph normal
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
