# Texturing large objects (ships, buildings, vehicles, machines)

The goal is that the object reads as **huge**. Scale is communicated by the
*size of the smallest details* and by how weathering behaves.

## Scale cues
- **Small, repeated, precise details**: panel lines, rivet rows, hatches,
  vents, windows, running lights, stencilled text and warning markings. The
  smaller and denser (relative to the silhouette), the bigger the object
  reads. One human-scale element (a railing, a door, a 1 m stencil) can make
  a structure feel enormous.
- **Irregular paneling**: plates of varied sizes, not a uniform grid. Random
  panel layouts are the classic sci-fi "sense of scale" device.
- **Panel-to-panel tonal variation** ("aztec" patterns on studio models,
  plate-by-plate shade shifts): slight value or hue differences per plate.
  It reads as many separately made and replaced parts.

## Weathering at scale
- Prefer **large, soft, directional** effects over small sharp ones: soot and
  heat discoloration behind engines and vents, long grime streaks below
  outlets, sun fading on upward faces, grime pooling in recesses.
- **Small damage is tiny.** Chips and scratches on a 500 m hull are
  sub-pixel at most viewing distances; keep them hairline and sparse, or
  skip them and put the wear into roughness breakup.
- **No height noise** on hull plates; it reads as a small object.
- Avoid every-edge wear: contact damage happens at docking clamps, hatches,
  leading edges and landing gear, not uniformly.

## The Star Citizen approach (tiling + decals + chamfers)
Why big hard-surface ships in SC look crisp. It isn't huge unique textures;
one unwrap with everything baked runs out of texel density long before a
ship is big, even at 8K. Instead there are three parts:
1. **Tiling materials at one fixed physical scale.** Surfaces are split into
   areas, each mapped to a seamless tiling material at a fixed texel
   density (e.g. one tile per 2×2 m), regardless of overlaps or area. A
   1 km hull is as crisp as a barrel, and smooth metal tiles can be as small
   as 256–512 px.
2. **Pre-modeled decals for all surface detail.** Seams, rivets, vents,
   bolts and panels are modeled once in high poly, laid out in an atlas and
   baked to normal, AO, cavity and height. Small floating quads on the hull
   map to atlas regions. The decal shader blends **only chosen channels**:
   typically normal + AO/cavity, keeping the underlying albedo, roughness
   and paint. So one rivet works on red paint, bare steel or plastic, and
   changing the atlas once updates every ship.
3. **Chamfered geometry with face-weighted normals** gives smooth,
   light-catching edges without baked normal maps. This is a modeling
   choice: if the mesh has crisp chamfers, don't fight it with edge wear.

What to take from it even without that tooling:
- Fine detail should keep **one physical size everywhere**: project it in
  world space (tri-planar) at a fixed tiling, not through a messy UV
  layout.
- **Detail layers should touch only the channels they need.** Rivets and
  seams change normal/height and cavity; the paint color and roughness
  continue underneath.
- **Scale cues come from small, sharp, repeated detail**, which is cheap
  when it's tiling or decal-based rather than painted per texel.

## Other techniques studios use (when the tool supports them)
- Trim sheets: one texture of reusable strips (edges, beams, vents) that
  many meshes map onto.
- Modular pieces share UV space; long beams tile.
- World-space or tri-planar detail for fine noise, so it keeps one physical
  size across the object regardless of UV island scale.
- Check textures are displayed at full resolution in the target engine;
  "blurry big object" is often a display-size setting.

## Procedural defaults and scale
Generator widths and noise sizes are tuned for a prop filling the frame.
Divide edge/crease widths and multiply noise frequencies by the size ratio:
roughly 5x for a large vehicle and 10–20x for a capital ship, then judge by
eye. In Material Painter, `tools/mp_scale.py` does this to a text project.
