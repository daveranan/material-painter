---
name: material-painter-agent
description: How to texture models in Material Painter (MP, the Rust PBR painter in this repository) as an agent, through its `mp` CLI and editable `.json` text projects with live reload. Use this whenever the user wants a model textured, a material or layer stack built or tuned, a smart material applied, masks or weathering adjusted, mesh maps baked, renders made or textures exported with Material Painter, or mentions mp, ship.json, layer ids, ID masks, Tripo/AI-generated meshes to texture, or Substance-style workflows "but in MP". Pair it with the pbr-texturing-craft skill for the artistic judgment.
---

# Texturing with Material Painter as an agent

MP runs as a desktop app. You drive it two ways that share one document:
- **`mp` CLI** (`target/debug/mp.exe`): calls the app's tools
  (bake, add layers, masks, render, export). Good for actions.
- **Text project** (`name.json` + `name.data/`): the whole document as
  readable JSON. Edit it on disk; the app reloads it live (one undo step,
  mesh maps kept). Good for building and tuning stacks in bulk.

Always pair with **pbr-texturing-craft**: MP's presets are tuned for
hand-held props and need the scale and restraint rules from that skill.

## Start
Paths below are relative to the repo root (`tools/` are this repo's agent
scripts). Keep working files (meshes, `out/`, `library/` textures) in a
folder outside the repo; `tools/cliff_kit.py` finds them via `MP_WORKSPACE`
(default: the folder the repo is in).
```bash
cargo build -j 8                               # from the repo root; first build ~5 min; mp.exe + material_painter.exe
./target/debug/mp.exe launch path/to/project.json   # or a mesh .obj/.fbx; prints the port
./target/debug/mp.exe tools                       # every tool and its arguments
./target/debug/mp.exe state                       # layers, bake report, live-reload status
```
`mp <tool> key=value …`: values parse as JSON (`index=3`, `color=[1,0.5,0]`,
`weights={"Edges":1}`), otherwise text. Images go to `--out` (file or folder).
Builds use `-j 8`; the app holds `material_painter.exe` open, so close it
before rebuilding with `mp quit` (`save=true` or `discard=true` when there are
unsaved changes). Never kill the process: MP then offers crash recovery on the
next start. `mp launch FILE` while the app runs loads FILE into *that* app
(a mesh replaces the user's mesh, keeping the layers): it never starts a
second instance, so don't use it for side experiments on the user's document.

## Preparing an AI-generated (Tripo, etc.) single mesh
These come as one piece with one baked texture and no material IDs.
1. `python tools/make_idmap.py model.glb --out out` gives per-triangle material
   clusters from texture color snapped to sharp-edge panels, plus preview renders.
2. `python tools/export_obj.py model.glb out/faces.json out/clusters.json out/model_ids.obj`
   writes an OBJ with the cluster IDs as vertex colors, plus `model_ids_basecolor.png`.
3. `mp load_mesh path=…obj`, then
   `mp bake id_source=vertex_colors part_crease_angle=30 quality=High resolution=4096`.
   - `part_crease_angle` makes every panel its own *part*, so per-part
     variation and Random Parts vary plate by plate. Without it a one-piece
     mesh has a handful of parts and per-part variation does nothing useful.
   - The bake report's `id_regions` are numbered in MP's own order: match
     them to clusters by `triangles` count, not by index.
4. Save as a text project: `mp save_project path=C:/…/model.json`.

Python tools run under a hard 4 GB memory cap (`tools/memguard.py`); keep
it that way. Never split a textured trimesh into submeshes (each copy holds
the full texture).

## Building the stack
Quick structure through the CLI, then detail in the JSON:
- `mp add_layer kind=smart preset="Painted Steel Worn"` gives a folder of
  masked fill layers (base, chips, scratches, grime, dust, micro layers).
- Mask a folder to one material: `mp set_mask index=<folder> generator="ID Mask" id=<region>`.
- `mp list_presets` lists smart materials, materials, generators, generator
  inputs, blend modes, textures and view modes.

**The user may be working in the app at the same time.** A reload from disk
replaces the document (undoable, but silent), so before every file edit run
`mp state`: if `unsaved_changes` is true, someone changed things in the app.
Ask whether to keep them (`mp save_project` writes them into the file, then
edit) or drop them; never edit over them blindly. Renders also show their
live tweaks, so a sudden change in look may not be yours.

Then edit the project JSON (see `references/project-json.md` for the schema):
recolor, set opacities, change generator inputs and levels, add layers or
folders (unique `id`, `parent` = folder id, a folder sits *after* its
children). Save the file; check `mp state` (`status` says "Reloaded …", and
`live_reload.error` explains a mistake). Python is fine for bulk edits:
load, change by layer `name` + `parent`, dump.

### Substance to MP translation
| Substance Painter | Material Painter |
|---|---|
| Fill layer (material) | `kind: fill` with color/roughness/metallic/height; `enabled` picks channels |
| Smart material | `add_layer kind=smart` (folder of masked fills) |
| Smart mask / generator | mask `generator` (`kind` + input `weights` + level/contrast/scale/noise) |
| Metal Edge Wear | `Edge Wear` (Edges, Convexity, Noise) |
| Dirt / Mask Builder grime | `Dirt`, `Cavity Grime` (Cavity), `Crease Grime` (Creases) |
| Dust / Light generator | `Dust` (Sky exposure, Facing up) |
| Leaks / streaks | `Rain Streaks`, `Drips & Bleeding` |
| Position gradient | `World Gradient` (World height) / `Part Gradient` |
| Color selection / ID mask | `ID Mask` (id_color, id_tolerance) from the ID bake |
| Random per-part variation | fill `part_variation`, generator `Random Parts` (`Part random`) |
| Levels / blur / warp filters | mask `filters`: levels, invert, contrast, blur, sharpen, warp, noise |
| Anchor points, decals, text | not available (see gaps) |
| Emissive channel | not available; bright, low-roughness color only |
| Normal map input | fill `images` `normal` (tangent-space; `normal_directx` for UE/DirectX maps), `normal_strength` |

## Look-dev loop
1. Change things (JSON edit or CLI).
2. Render: `mp render mode=Material width=1300 height=760 --views three_quarter,rear_three_quarter --out out/vN/`
   (named views center the mesh; add `distance=1.1 target=[x,y,z]` for close-ups).
3. Check channels: `mode=Roughness`, `"Base Color"`, `Metallic`, `Height`,
   and bake maps like `Cavity`, `"Crease Distance"`, `"Part Random"`, `ID`.
4. Look at the images yourself, critique with pbr-texturing-craft section 4,
   and fix causes, not symptoms.
5. Keep versions (`out/v1`, `v2`, …) so the user can compare.

## Lessons from the ship trial (presets on a capital ship)
- Preset widths and noise are prop-sized: run `python tools/mp_scale.py
  project.json --detail 5` (to 10 for capital ships), then tune by eye. Edge
  wear needs another 2x finer and low noise breakup (`noise ≈ 0.3`,
  `Noise` weight ≈ 0.1) or chips become blotches.
- Hide every `Micro Height` layer and zero `height_variation` on big hulls:
  that is the "orange peel / beginner" look. Hide `Dust` in space.
- Presets stack many grime layers at 50–100%; bring them to 10–30%.
  `Panel Gaps` / crease grime at full strength reads as "cavity too fat".
- Presets carry their own colors (red paint, orange random parts); recolor
  or hide them to match the reference.
- Roughness landed at **satin**: hull paints 0.48–0.55 with
  `roughness_variation` ≈ 0.12, worn exposed metal ≥ 0.4. At 0.3 the paint
  mirrored the sky like car paint; with grime stacks at 0.8–0.95 it was
  chalky. Painted parts are metallic 0, never 0.15.
- MP normalizes meshes to a 2-unit box: positions from the source GLB
  (here 1 unit long) are half of MP's. Convert before using MP coordinates
  (`target`, `point`, `select_surface` bounds).
- Per-panel tone (`part_variation` 0.2–0.55, after the crease-split bake)
  did more for "pro" than any amount of wear.
- A source texture (e.g. Tripo's) works as an Overlay color layer at
  ~0.3–0.45 for panel lines and greebles, but it also carries baked lighting.

## Fine detail at a fixed physical size (the Star Citizen idea, in MP)
Auto-generated UVs (Tripo etc.) are fragmented and uneven, so UV-projected
detail changes size from island to island. For fine, repeating detail
(panel grids, perforations, brushed metal, micro scratches) use a fill with
`"projection": "Triplanar"` and `tiling` per world unit: one size
everywhere, no seams. The mesh fits a 2-unit box, so for a ship meant to be
~500 m long, 1 unit ≈ 250 m; a 2 m tile is `tiling ≈ 125`. Shelf textures
(`mp list_presets` → textures: Tiles Square, Perforated Holes, Hex Mesh,
Grunge, Scratches…) can drive these.
- Make detail layers **channel-selective** like SC decals: set `enabled` to
  touch only what the detail changes (e.g. `[false, true, false, true]` for
  roughness + height), so the paint color continues underneath.
- Single placed details (rivet rows, bolts, vents) can be stamped with
  `paint_stroke` using brush tips `Rivet`, `Hex Bolt`, `Phillips Screw` or
  `Vent Slot` on a height/roughness-only paint layer, `projection=surface`.
- Uniform grids look fake on hulls; break them with an irregular mask, or
  keep them on secondary surfaces.

## Liveries and markings: masks placed by 3D position
Bands, stripes and hull numbers must follow the hull, not the UV layout.
`tools/livery_mask.py model.glb spec.json <project>.data/livery --faces out/faces.json`
computes each texel's 3D position and writes UV-space PNG masks: bands by
fraction of length from the bow (optionally limited to material ids and a
height range), and text in any TTF font on the side-facing surfaces of both
sides, reading correctly on each. Use them as a fill layer's mask `paint`
(`{"blob": "livery/x.png", "format": "R8Unorm", "width": 4096, "height": 4096}`),
placed above the material folders and below global weathering. Regenerating
the PNGs reloads live. Spec options (see the script's docstring): `facing`
("up"/"side"/"down") keeps accents on the panels that face that way;
`deck_edge` makes lengthwise stripes that follow the deck's taper; bands
sharing a name merge into one mask; `weather` gives patchy, chipped paint
and ragged letters from 3D noise; text `streaks` writes `<name>_streaks.png`
run-off for a layer under the text. Measure where a wall actually is before
placing text (histogram of side-facing face heights along the hull).
Accents should mark something (a deck edge, a hatch), not wrap the hull.
Keep livery colors muted (off-white ≈ 0.62, ochre ≈
[0.6, 0.43, 0.17]); saturated stripes look like toys.

## Surface detail from the source texture (what makes it read as a ship)
Smooth plates with color only read as a painted toy, whatever the
roughness. AI-generated models' textures already contain the design's panel
lines: `tools/detail_from_texture.py <basecolor.png> <project>.data/detail
--mesh model.glb` high-passes the texture into `seams.png` (dark lines,
widened to `--width 3` px and blurred `--soften 1` so they don't alias) and
`highlights.png`. Lessons:
- Use seams as a **dark, color + roughness line** (color ≈ 0.07, roughness
  0.6, opacity ≈ 0.7, `enabled` [true, true, false, false]). Height on thin
  lines makes one wall catch the sky: every seam reads as a bright embossed
  rim ("highlights instead of darkens"). Soft height reads as "divots". Keep
  seam height off, or at most −0.03 on a blurred copy.
- Don't use `highlights.png` as raised edges: it adds more bright lines.
- 1–2 texel lines shimmer whatever the resolution; MP's viewport now mips
  the painted channels, but lines still need to be ≥ 3 texels.

## Weathered roughness: what stops the "toy" look
Reference ships (Star Citizen etc.) read big because reflections are broken
up: dust, grime films, smudges and wipe marks, mostly in roughness.
- A roughness-only grime film (Clouds, scale ≈ 25, roughness ≈ 0.72,
  opacity ≈ 0.55) plus fine mottling (scale ≈ 120) over everything.
- A real dust imperfection map (Megascans etc., `library/imperfections/`)
  as a **triplanar** roughness fill at ≈ 20 m tiles (`tiling` ≈ 12 on a 500 m
  ship), opacity ≈ 0.6.
- Dust *color* films lift dark paint a lot (Screen especially): keep them at
  0.1–0.2 opacity or the charcoal livery turns to concrete grey.
- Metallic edge wear at roughness ≈ 0.4 draws a white chrome streak along
  every edge in a bright environment: dull it (roughness ≥ 0.55, opacity
  ≈ 0.35). "Gloss patches" should not go below roughness ≈ 0.34.
- Per-panel tone: two Random Parts fills (lighter and darker, opacity
  0.3–0.45, different seeds) above the material folders.

## Gotcha: app saves move paint masks
When the app saves a text project, paint masks are rewritten to
`set0/layerN_mask.png` and the JSON points there. Scripts that regenerate
masks under `livery/` or `detail/` then no longer affect the layer: point
the layer's `mask.paint.blob` back at the generated file after an app save.

## Look parity with Blender (is it MP or the textures?)
`tools/parity/blender_render.py` renders a mesh plus exported textures (or a
test sphere) in Blender with MP's exact camera, HDRI orientation and scale;
`tools/parity/chart.py` renders a roughness × material sphere chart in both
(`--engine cycles` is the exact answer for a sphere). Set MP to tonemap
"Linear (clip)" and key light off first (`preview_settings`), restore after.
Verified Nov 2026: MP's image-based lighting matches Cycles within ~2/255 on
paints and ~4/255 on steel, so a "toy" look comes from the content and the
lighting setup, not from MP's shading. Set `BLENDER` to use another install.

## Trust measurements over impressions
If the user's screen and your renders disagree, measure: `mp shot` (the app
window) next to `mp render` from the same camera, then compare pixel values on
a background patch. (This found an MP bug where the viewport showed every
value at value^2.2; fixed, so they should now match within a level or two.)

## Debugging an artifact: isolate, don't guess
To find which layer causes something (e.g. "dashed lines"), render a fixed
camera (`distance/yaw/pitch/target`, not the app's moving view), then hide
candidate layers one group at a time (`mp update_layer index=N
visible=false`, render, restore) and compare crops. On the ship, dashes came
from paint chips through to a mirror-smooth steel layer, broken by fine noise
into periodic dashes. Fix: rare chips (`level ≈ 0.9`, coarser noise
`scale ≈ 12`), exposed metal at roughness ≥ 0.4, and a separate low-opacity
"worn paint edges" layer (lighter, rougher paint, little noise) for the wear.
Careful with inverted chip masks: a *higher* level means fewer chips.

## Environment kits: many rocks, shared tri-planar surfaces (HD2-style)
`tools/cliff_kit.py` builds this for a set of meshes: each rock keeps its own
bake and masks, every rock samples the same 3 tri-planar surfaces (fresh
rock S0, fractured S1 by convexity, deposit S2 by sky/facing-up) at one
physical scale, plus cavity grime and a Multiply "biome grade". `build`,
`render <tag>`, `candidates` (each library surface alone on S0, deposits on
S2, as a comparison sheet). Lessons:
- **Tone-match S0 and S1** (mean albedo within ~0.05). A dark scan next to
  a light one turns every mask boundary into a black blotch; the layers
  should differ in grain and normals, not brightness.
- **Mask noise per metre**: generator `scale` is per normalized mesh, so
  scale it by real size / reference size or blobs change from rock to rock.
- Tiling per metre: `(longest side m / 2) / tile m` (Quixel json `physicalSize`).
- Quixel UE packs (`T_x_B/_N/_ORM`): roughness is ORM green, normals are
  DirectX. Non-square physicalSize (e.g. 80x40 m cliff faces) is for huge
  formations; on a 10 m rock it smears.
- **Unwrap Tripo meshes yourself** (Blender smart UV ~66 deg, fix overlaps,
  average island scale, pack): some come with no UVs, Tripo's own waste
  space. Check UV use (aim 40%+) and texels per metre.
- **Live detail**: the viewport re-samples up to 3 tri-planar fills with a
  color image (normal blend, visible) at full resolution, weighted by how
  much of each shows (`Engine::live_detail`). A 14 m rock in a 2K set is only
  ~100-150 px/m, a Quixel surface ~1000+, so without it close-ups look
  smeared. Exports and the path tracer still get the UV-space result.

## Gaps, and what to do instead
- **No emissive**: lights and engines are bright low-roughness color; say so.
- **No atlas decals or stencil text**: no way to place a baked detail
  atlas (rivets, seams, markings) as channel-selective decals. Use the
  tri-planar and stamp workarounds above, and propose an MP decal layer
  when markings or dense placed detail matter.
- **No PBR validator**: check `"Base Color"` and `Metallic` renders against
  pbr-texturing-craft's `pbr-values.md` by eye.
When a gap blocks the user's goal, propose the MP change (this repo is ours
to improve: follow `AGENTS.md`, keep tests and GPU tests
green).
