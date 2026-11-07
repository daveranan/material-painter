# The material brief, and what comes back from the game

Ground materials go through one loop: **brief → MP → export → game loads it
→ fixed game cameras → review → next pass.** The brief is what the game side
(or the person) gives before work starts; the captures are what the game
sends back each pass. Ask for anything missing before building.

## The brief (one per material)

Write it as JSON next to the MP project (`<material>.brief.json`), so both
sides can read it:

```jsonc
{
  "material": "sand_pebbles",              // becomes the MP project / export name
  "stratum": "vault",                      // vault | surface
  "biome": "temperate",
  "role": "Beach and dry riverbed floor; most common ground in the biome.",
  "features": {                            // the game's feature names (view::palette::Feature)
    "main":  {"name": "sand",    "coverage": 0.6},
    "mask1": {"name": "soil",    "coverage": 0.15, "why": "crevices; AO comes from it"},
    "mask2": {"name": "pebbles", "coverage": 0.25}
  },
  "color_ranges": {                        // allowed authored color per feature, OKLCH
    "sand":    {"L": [0.62, 0.74], "C": [0.02, 0.06], "h_deg": [70, 95]},
    "soil":    {"L": [0.40, 0.52], "C": [0.02, 0.05], "h_deg": [55, 85]},
    "pebbles": {"L": [0.50, 0.66], "C": [0.00, 0.03], "h_deg": [60, 110]}
  },
  "tile_m": 2.0,                           // metres per repeat
  "relief_m": 0.06,                        // metres from height 0 to 1
  "texture_size": 1024,
  "cameras": {                             // what players see
    "eye_height_m": 1.7,
    "near_m": [0.5, 4],                    // where full detail must hold up
    "mid_m": [4, 30],
    "far_m": [30, 400]                     // where only tone and macro variation matter
  },
  "neighbours": ["pebble meshes (rock palette)", "cliff_a (controls: fractured/deposit/AO)", "grass splat"],
  "engine": {
    "anti_tiling": "rotate",               // the ground shader's anti-tiling mode
    "displacement_m": 0.25,
    "compression": "BC7",
    "roughness": "per feature from palette",
    "ao": "packed with the normal (from height)"
  },
  "references": ["refs/beach_01.jpg", "refs/riverbed_scan_preview.jpg"],
  "current": {"screenshot": "shots/before_ground.png", "problems": "pebbles float, sand too yellow under Surface palette"},
  "scans": ["library/_incoming/soil_dirt_pkbpp20_4k"]   // optional starting scans
}
```

Minimum to start: material, features with names, tile_m, relief_m, the
camera distances, one reference image, and the current screenshot (or "new").
The color ranges can come from the palette work later; until then author
bland (chroma under ~0.06) and say so.

## What MP sends to the game

The "Game material (features)" export (see `material-painter-agent`'s
project-json reference): `<material>_ColorHeight.png`,
`<material>_NormalAOFeature.png` (normal XY, AO, one-channel feature),
`<material>_Displacement.png`, `<material>.material.json` (version 3:
feature names, tile_m, relief_m, displacement_m, rest_height, channel
layout), into the game's ground-materials folder. The game displaces with
the Displacement map and shades with the packed AO.
Plus the MP project (`<material>.json` + `.data/`) so any later pass edits
the same stack.

## What comes back from the game (the review set)

The game's offscreen bench (`studio-bench ASSETS OUT_DIR`, fixed cameras,
PNG per camera + JSON) is the judge. Per pass, ask for:

1. **The material on the fixed cameras**: close-up (0.5-4 m), ground at
   player eye height, mid, vista. Same cameras every pass so passes compare.
2. **A swatch**: a flat patch of only this material, top-down and at 30°,
   under a low sun and at noon.
3. **Palette variants**: palette off, then each biome palette that uses the
   material (Vault temperate, Surface mineral...), same camera.
4. **The features view** (`VIEW: FEATURES`: each feature a flat color), to
   check the masks land where the brief says.
5. **Numbers** from the bench JSON when relevant (frame cost if the material
   changes texture count or size).
6. Which game commit and switches (`CONQUEST_TERRAIN` keys) made them.

Until the bench can point a camera at one named material and switch
palettes from the command line, ask the game session to add that: a
`--material NAME` (or surface slot) and `--palette NAME` on `studio-bench`,
plus a swatch camera. That turns each pass into one command and a folder of
PNGs.

## Reviewing a pass

Compare with the reference, the previous pass and the brief's problems, in
this order, and fix only the top one or two per pass:
1. **Scale and tiling** at mid and vista (repeats, a stand-out feature,
   wrong size against the player and props).
2. **Height read** at close-up and eye height (elements sit in the ground,
   no floating, no noise in the normals at mid distance).
3. **Feature masks** in the features view (coverage close to the brief,
   crevice feature in the lows).
4. **Color under every palette** (bland enough to grade, no feature that
   breaks under one palette, matches the neighbours: pebbles in the sand
   match the pebble meshes).
5. **Detail** last (roughness breakup, small debris).

Write the pass result as a short note next to the brief (`passes.md`: what
changed, what the shots showed, what's next), so the next pass starts from
it.
