# Sources and what each taught

Read 2026-11-06. Most articles are interviews and give few numbers; values in
the recipes are starting points to tune by eye, not quotes.

- **Helldivers 2 biome materials** (GDC talk, Arrowhead's material artist):
  per-feature grading groups with masks, up to three features per material,
  bland neutral colors inside allowed ranges, scans limited to one or two
  features and blended by hand, color-to-mask to split the rest, HCL tools
  (hue changes that keep lightness), flatten to mean, match mean value, auto
  level balance, scatters for free masks, reusable feature "layers".
- **Desert rocks, Jonathan Benainous (Adobe)**: height as the raw material
  for every other map; rocks from paraboloid primitives, sculpted by
  subtracting chunks, eroded with clouds + slope blur + non-uniform blur;
  dunes from warped gradients at four scales, small ripples limited to
  crests by a shadow mask; sand pushed under rocks with a blurred mask, the
  contact softened separately; masks from highpass, AO, facing normal;
  pebbles colored through a gradient map whose last key is transparent; HSL
  to balance values. No numbers in the article.
- **Approaching rocky ground, Tristan Meere (80.lv)**: graph skeleton and
  blends first; height only until shapes read; roughness before albedo;
  large and medium forms in height, fine detail weak (show it in roughness
  and albedo); debris via tile samplers blended with Lighten; tile sampler
  masks stop clipping; water by lightening a flat color into height and
  extracting it with color-to-mask; check tiling beyond 1x; check in engine
  often; preview on planes and cylinders.
- **Building rocky ground, Tom Jacobs (80.lv)**: placement from physics
  (stones fall, cluster, break); height/random/bounding-box masks; gradients
  and Perlin blended with Darken for sharp rock edges; flood-fill random and
  bbox masks for size and color variation; slope blur kills hardness, use a
  curvature mask to restore it; rocks must stay rocks under warps.
- **Rocky ground with Substance and Marmoset, Amir Habibi (80.lv, 2025)**:
  world units (200 × 200 cm surface, 24 cm depth); macro to micro; flood
  fill for stone detail; Shape Splatter (heavy; no size random, so squash
  with transforms; small stones at 512); AO and shadow masks to scatter
  naturally; scans via Atlas Scatter kept off big stones; palette first;
  roughness from greyscale albedo plus the same patterns; render with two
  opposed lights and several times of day.
- **Riverstones bed, Shray Khanna (80.lv)**: big boulders from tile sampler
  + distance + edge detect + flood fill; medium stones placed through the
  previous layer's mask, normal and displacement; separate tile samplers
  per size; height blend almost everywhere, a custom mask where mud climbs
  stones; pebbles height-blended into mud so the mud buries them; moss from
  curvature/dirt masks; leaves sitting on the terrain via non-uniform blur
  of the base under them; puddles height-blended into the lowest crevices;
  roughness ordering by material.
- **Cobblestones and clover, Vasilina Sirotina (80.lv)**: inverted stone
  mask from the first height blend is the ground mask, so clover never grows
  through stones; clover overlap through luminance random as height; dew by
  plain blend; balance the normal so small details don't dominate; no dry
  stones next to wet clover.
- **Forest ground, Yuting Chen (80.lv, 2025)**: big, medium, small; dirt from
  noises plus highpass to even it; rocks from tile generator + distance +
  edge detect + flood fills (Darken); scanned atlases via Atlas Scatter with
  their low height range fixed by Levels; Quixel normals are OpenGL (flip
  green for DirectX); non-uniform blur before blending roots to avoid bad
  intersections.
- **Pebbles layering, Romain Lemaire (ArtStation, via the ArtStation
  Magazine roundup)**: four pebble layers, each a custom cell pattern (no
  self-intersection) with per-pebble slope from flood-fill gradients
  (Darken) and roundness from non-uniform blur; each layer placed in the
  previous layer's holes, its height and slope taken from a blurred copy of
  the previous layer, blended in 32-bit and offset just above it; the new
  layer's mask = after minus before, for per-layer color IDs.
- **Dunes, Derk Elshof (ArtStation)**: Clouds 2 through Normal to Height HQ
  (relief balance controls detail) and Non-Uniform Directional Warp in Min
  trail mode for wind carving.
- **ArtStation Magazine "Natural materials" (2022)**: a roundup linking the
  two ArtStation posts above plus a sand scan WIP and a frozen lake
  breakdown (not read in depth).
- **Adobe Tile Sampler reference**: the page wouldn't load (403 to the
  fetcher, blank in the browser). The parameter knowledge in the recipes
  (pattern input atlas and random pick, scale/position/rotation/luminance
  random, mask map and threshold, mask random, scale and vector map
  inputs, Max/Add blending) is from general Designer knowledge, not that
  page.
