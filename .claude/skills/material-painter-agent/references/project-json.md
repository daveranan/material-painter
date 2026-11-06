# MP text project (`name.json`): what to edit

Top level: `app`, `version`, `mesh` (blob paths; don't edit), `bake`
(settings; editing triggers a full reopen and maybe a re-bake), `graphs`,
`texture_sets[]`, `active_set`, `view`.

`texture_sets[k]`: `name`, `resolution`, `tris`, `selected`, `map_levels`,
`map_fixes`, and **`layers_bottom_to_top`**, the stack, bottom first.

## Layer
```json
{
  "id": 42,                         // unique within the set; keep ids stable
  "name": "Hull paint",
  "parent": 30,                     // folder id, or null at top level
  "visible": true,
  "opacity": 1.0,
  "blend": "Normal",                // Normal Multiply Screen Overlay "Linear Dodge (Add)" Subtract Darken Lighten
  "content": {"kind": "fill", "fill": { … }},   // or {"kind":"folder","open":true}, or paint (PNG channels)
  "mask": null | { … },
  "images": [null, null, null, null] // color, roughness, metallic, height: {"name":…, "data":"ref/x.png"}
}
```
A folder layer comes **after** its children in the list. New layers: copy an
existing layer of the same kind and change `id`, `name`, `parent`.

## Fill
```json
{
  "enabled": [true, true, true, false],   // color, roughness, metallic, height channels
  "color": [0.8, 0.82, 0.83],            // sRGB 0-1
  "roughness": 0.35, "metallic": 0.0, "height": 0.0,
  "color_variation": 0.03, "roughness_variation": 0.08, "height_variation": 0.0,
  "variation_scale": 20.0,               // noise frequency: bigger = finer
  "part_variation": 0.3,                 // per-part tone/roughness shift
  "seed": 0.0,
  "projection": "Uv",                    // Uv Triplanar PlanarX PlanarY PlanarZ Spherical Cylindrical
  "tiling": 1.0, "rotation": 0.0, "offset": [0, 0], "triplanar_sharpness": …,  // 1 soft … 16 sharp
  "graph": null
}
```

## Mask
```json
{
  "generator": {
    "kind": "Edge Wear",        // Edge Wear, Dirt, Dust, Rain Streaks, "Drips & Bleeding", Cavity Grime,
                                // Crease Grime, Thin Areas, Part Gradient, Random Parts, Clouds,
                                // World Gradient, ID Mask, Custom Bake Map
    "weights": {"Edges": 1.0, "Convexity": 0.6, "Noise": 0.12},  // omit = the kind's defaults
    "level": 0.8, "contrast": 0.04,  // threshold and transition width (small = sharp)
    "scale": 50.0,                   // noise frequency: bigger = finer
    "noise": 0.3,                    // breakup amount
    "edge_width": 0.0012,            // world width for Edges/Creases (mesh fits a 2-unit box)
    "invert": false, "seed": 0.0,
    "id_color": [1, 0, 0], "id_tolerance": 0.15,   // ID Mask
    "custom": []
  },
  "paint": null,            // or a PNG blob path (a painted or script-made mask)
  "graph": null,
  "filters": [{"kind": "levels", "enabled": true, "a": …, "b": …, "c": …}],  // a/b/c: the filter's sliders; add one in the app and save to see its values
  "layers": []              // extra mask layers: {"source":{"generator":{…}}|{"fill":0.5}, "blend":"multiply", "opacity":1}
}
```
Generator inputs (`weights` keys): Edges, Convexity, Concavity, Occlusion,
Cavity, Sky exposure, Facing up, Thin areas, Part gradient, World height,
Part random, Creases, Streaks, Noise, ID match, Overhangs, Wood grain, Cells,
Scratches.

## Files
Blob paths are relative to `name.data/`. Paint textures are PNG (edit them
with any tool, or replace with a script-made mask of any size: it's
rescaled). Files you add under your own folder (e.g. `ref/`) aren't cleaned
up; the app's saves manage `mesh/` and `set<N>/`.
