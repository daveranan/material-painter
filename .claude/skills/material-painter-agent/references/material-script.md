# Material scripts

A material graph written as text: one named step per line, compiled onto
the same nodes Material Designer shows (the canvas is a view of the
script). Prefer scripts over node JSON for any procedural material: they
are short, diffable, and every named step can be rendered on its own.

## Where a script lives

In the project JSON's `"graphs"`, instead of nodes:
```jsonc
"graphs": [{"id": 1, "graph": {"name": "Sand and pebbles", "script_file": "scripts/sand.mat"}}]
// or inline: {"id": 1, "graph": {"name": "...", "script": "size 1024\n..."}}
```
`script_file` is relative to `<project>.data/`; editing the file reloads
the project live, like any data file. A fill uses the graph with
`"fill": {"graph": 1}`, a mask with `"mask": {"graph": 1}`. If someone edits
the graph on the canvas, the project saves nodes from then on (the script
no longer matches).

## Review step by step

```
mp script_step                      # script graphs and their step names
mp script_step step=stones --out stones.png
```
Renders that step's own output at the graph's size. Review height steps
before color; this is the "height first" loop from `game-ground-materials`.

## Syntax

```
size 1024                         # texture size (64-4096)
name "Sand and pebbles"
# comment
dunes  = noise(perlin, scale=3, octaves=4, seed=1)
sand   = dunes * 0.6 + noise(turbulence, scale=64) * 0.05
stones = tile_sampler(disc, columns=16, size_random=0.4) | blur(1.5)
height = max(sand, stones + blur(sand, radius=16) * 0.8)

fn crevices(h, radius=12, level=0.45) {
  hp = highpass(h, radius)
  return hp | histogram_scan(position=level, invert=true)
}
dirt = crevices(height)

output base_color = mix(gradient_map(sand, dark=#8f7a5c, light=#cdb994), #7d6a50, dirt * 0.5)
output height = height
output roughness = mix(0.85, 0.6, dirt)
```

- **Calls**: a node (table below, or `user/<name>` for custom nodes) or an
  `fn`. Positional arguments that are images (steps, calls) fill the
  node's inputs in order; plain values (numbers, `#rrggbb`, names) fill
  its parameters in order. `name=value` sets an input or a parameter by
  name; a number or color given to an input becomes a flat color.
- **Enum options** are bare names, compared loosely (`perlin`,
  `soft_light`, `pattern_input`).
- **`a | f(...)`** feeds `a` into `f`'s first input.
- **`+ - * /`** between images, or an image and a number, make math nodes
  (unclamped); `min(a, b)`, `max(a, b)` too. Clamp with `clamp01(x)` or
  `levels`.
- **`fn`**: a reusable step (the talk's "feature layers"). Its body is
  `name = ...` lines and one `return`; it sees its parameters and the
  script's earlier steps. Defaults are allowed.
- **Outputs**: `output base_color | roughness | metallic | height | mask = expr`.
- **Images**: `bitmap(image="scans/soil_basecolor.jpg")`, relative to the
  data folder.
- **Built-in functions** (written in the language itself): `mix(a, b,
  mask)` = b over a through mask; `clamp01(x)`.
- **Node authoring** (custom WGSL nodes): `// @mean` gives `mean_<input>()`;
  `// @minmax` gives a grey input's `min_`, `max_`, `mean_`; `// @passes log2
  | N | <int param>` renders the node several times, each pass reading the
  last with `prev_at(texel)` and knowing `pass_index()` / `pass_count()`
  (jump flooding, region growing); see `distance.wgsl`, `flood_fill.wgsl`.
- Errors name the line and suggest the closest node or option; the app's
  status line shows them (`mp get_state` → `status`).

Feature masks for the game export still come from layers tagged with
features (see `project-json.md`): give each feature its own fill and
script, or put the feature's mask script on that layer's mask.

## Nodes (from `src/graph/nodes/*.wgsl`; the headers are the reference)

| Node | Inputs | Parameters (default) | Output |
|---|---|---|---|
| `ambient_occlusion` | height | radius=float(1, 64) (12); depth=float(0, 4) (1); intensity=float(0, 4) (1.5) | gray |
| `auto_level` | input | mid=float(0, 1) (0.5); amount=float(0, 1) (1) | gray |
| `auto_levels` | input | - | gray |
| `bitmap` | - | image=image; repeat=int(1, 32) (1) | color |
| `blend` | foreground, background, mask | mode=Normal|Multiply|Screen|Overlay|Add|Subtract|Lighten|Darken|Soft Light (Normal); opacity=float(0, 1) (1) | color |
| `blur` | input | radius=float(0, 64) (4) | color |
| `cells` | - | mode=Distance|Edges|Random (Distance); scale=int(1, 128) (8); jitter=float(0, 1) (1); seed=int(0, 1000) (0) | gray |
| `checker` | - | count=int(1, 64) (4) | gray |
| `color` | - | color=color (0.5, 0.5, 0.5) | color |
| `color_to_mask` | input | color=color (0.35, 0.45, 0.2); tolerance=float(0, 0.2) (0.03); softness=float(0, 0.2) (0.02); lightness_weight=float(0, 1) (0.3); invert=bool (false) | gray |
| `curvature` | height | width=float(0.5, 16) (2); intensity=float(0, 32) (6) | gray |
| `directional_noise` | - | direction=Horizontal|Vertical (Horizontal); scale=int(1, 64) (4); stretch=int(1, 64) (16); octaves=int(1, 8) (4); roughness=float(0, 1) (0.55); seed=int(0, 1000) (0) | gray |
| `directional_warp` | input, warp | intensity=float(0, 1) (0.1); angle=float(-180, 180) (90) | color |
| `distance` | input, source | mode=Spread Source|Distance (Spread Source); max_distance=float(0.001, 1) (0.2); threshold=float(0, 1) (0.5) | gray |
| `edge_detect` | input | width=float(0.5, 16) (1.5); strength=float(0, 16) (4) | gray |
| `facing` | height | angle=float(-180, 180) (45); intensity=float(0, 64) (8) | gray |
| `flood_fill` | input | threshold=float(0, 1) (0.5); iterations=int(8, 512) (128) | color |
| `flood_fill_to` | flood | mode=Random|Gradient|Size|Position X|Position Y (Random); angle=float(-180, 180) (0); angle_random=float(0, 1) (1); random_min=float(0, 1) (0); random_max=float(0, 1) (1); size_scale=float(1, 64) (8); seed=int(0, 1000) (0) | gray |
| `gradient` | - | angle=float(-180, 180) (0); mirror=bool (false); repeat=int(1, 32) (1) | gray |
| `gradient_map` | input | dark=color (0.08, 0.06, 0.05); mid=color (0.45, 0.32, 0.22); light=color (0.9, 0.85, 0.75); mid_position=float(0.01, 0.99) (0.5) | color |
| `grayscale` | input | channel=Luminance|Red|Green|Blue|Alpha (Luminance) | gray |
| `hcl_adjust` | input | hue=float(-0.5, 0.5) (0); chroma=float(0, 2) (1); lightness=float(-0.5, 0.5) (0); contrast=float(0, 2) (1) | color |
| `hcl_flatten` | input | lightness=float(0, 1) (0.3); chroma=float(0, 1) (0.5); hue=float(0, 1) (0.5) | color |
| `hcl_merge` | lightness, chroma, hue | - | color |
| `hcl_split` | input | channel=Lightness|Chroma|Hue (Lightness) | gray |
| `height_blend` | top, bottom, mask | contrast=float(0.001, 0.5) (0.05); offset=float(-1, 1) (0); output=Height|Mask (Height) | gray |
| `highpass` | input | radius=float(1, 64) (8); contrast=float(0, 4) (1) | gray |
| `histogram_scan` | input | position=float(0, 1) (0.5); contrast=float(0, 1) (0.8); invert=bool (false) | gray |
| `hsl` | input | hue=float(-0.5, 0.5) (0); saturation=float(0, 2) (1); lightness=float(-1, 1) (0) | color |
| `invert` | input | - | color |
| `levels` | input | in_low=float(0, 1) (0); in_high=float(0, 1) (1); gamma=float(0.1, 4) (1); out_low=float(0, 1) (0); out_high=float(0, 1) (1) | color |
| `match_mean` | input, reference | lightness=float(0, 1) (1); color=float(0, 1) (1) | color |
| `math` | a, b | op=Add|Subtract|Multiply|Divide|Min|Max|Power|Difference (Multiply); value=float(0, 4) (1); clamp_result=bool (true) | gray |
| `mesh_map` | - | map=meshmap | gray |
| `multi_warp` | input, warp | intensity=float(0, 1) (0.05); directions=int(2, 8) (4); angle=float(-180, 180) (0) | color |
| `noise` | - | style=Perlin|Value|Turbulence|Ridged (Perlin); scale=int(1, 128) (6); octaves=int(1, 10) (5); roughness=float(0, 1) (0.5); seed=int(0, 1000) (0) | gray |
| `nonuniform_blur` | input, map | radius=float(0, 64) (8) | color |
| `output` | input | usage=Base Color|Roughness|Metallic|Height|Mask (Base Color) | color |
| `posterize` | input | steps=int(2, 16) (4) | color |
| `scratches` | - | count=int(1, 400) (80); length=float(0.01, 1) (0.2); width=float(0.2, 8) (1); angle=float(-180, 180) (30); spread=float(0, 1) (0.5); seed=int(0, 1000) (0) | gray |
| `shadows` | height | angle=float(-180, 180) (45); elevation=float(1, 89) (25); depth=float(0.001, 0.5) (0.05); distance=float(0.01, 0.5) (0.1); softness=float(0, 1) (0.5) | gray |
| `shape` | - | shape=Disc|Square|Diamond|Cone|Ring (Disc); size=float(0, 1) (0.7); softness=float(0, 1) (0.05); repeat=int(1, 32) (1) | gray |
| `slope_blur` | input, slope | intensity=float(0, 1) (0.1); uphill=bool (false) | color |
| `tile_sampler` | pattern, pattern_2, pattern_3, pattern_4, mask, scale, rotation | shape=Disc|Square|Diamond|Cone|Pattern Input|Paraboloid|Bell|Gaussian|Pyramid|Capsule (Disc); columns=int(1, 64) (8); rows=int(1, 64) (8); size=float(0.05, 2) (0.8); size_random=float(0, 1) (0.3); position_random=float(0, 1) (0.5); rotation_random=float(0, 1) (0.5); brightness_random=float(0, 1) (0.4); gaps=float(0, 1) (0); blend=Max|Add (Max); seed=int(0, 1000) (0); pattern_count=int(1, 4) (1); mask_threshold=float(0, 1) (0.5); scale_map=float(0, 1) (0); rotation_map=float(0, 1) (0); output=Height|Instance Random|Instance Mask|Pattern Index (Height) | gray |
| `tiles` | - | columns=int(1, 64) (4); rows=int(1, 64) (8); offset=float(0, 1) (0.5); gap=float(0, 0.4) (0.04); bevel=float(0, 0.5) (0.08); variation=float(0, 1) (0.3); seed=int(0, 1000) (0) | gray |
| `transform` | input | repeat=int(1, 32) (1); rotation=float(-180, 180) (0); scale=float(0.1, 8) (1); offset_x=float(-1, 1) (0); offset_y=float(-1, 1) (0) | color |
| `warp` | input, warp | intensity=float(0, 1) (0.1) | color |
