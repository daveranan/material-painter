---
name: pbr-texturing-craft
description: Professional PBR texturing judgment, the knowledge a senior Substance Painter artist applies, for any texturing, material or look-dev work. Use it whenever you create, tune or critique materials, layer stacks, smart materials, masks, edge wear, grime, roughness, normals/height, or weathering, in Material Painter, Substance, Blender, Unreal or a script, and whenever the user says a texture looks fake, plastic, too dark, too noisy, "like a beginner's model", wrong scale, or asks how pros would do it. Also use it before handing back any render of a textured asset, to self-review it.
---

# PBR texturing craft

A texture reads as professional when it is **physically plausible**, **varied
at three scales**, **sized to the object**, and **restrained**. Most amateur
results fail on restraint and scale, not on missing features: too much grime,
wear everywhere, height noise on every surface, details sized for a different
object.

Read `references/pbr-values.md` for the numbers, `references/normals-height.md`
before touching height or normal channels, and `references/large-objects.md`
for anything big (ships, buildings, vehicles, machines). Sources are listed in
`references/sources.md`.

## 1. Decide what the object is before touching a layer

Write down, even in one line each:
- **Real size.** A hand tool, a car and a 1 km capital ship need different
  wear widths, noise sizes and grime amounts. Every procedural default
  assumes some size (usually a prop that fills the view). Rescale it.
- **Materials.** List them (painted steel, bare titanium, rubber, glass,
  emissive lights). Each gets its own base layer with physically plausible
  values. Paint over metal is two materials: dielectric paint on top of a
  raw-metal base that shows only where the paint is gone.
- **Story.** Who used it, where, and how hard? A carrier's hull sees
  micrometeorites, heat and soot near engines, and grime where crews
  service hatches. It doesn't see hand-worn chips on every edge. Weathering
  that answers "why here?" looks real; uniform weathering looks procedural.

## 2. Build the stack in this order

1. **Base materials**, one per material, flat but correct (values in
   `pbr-values.md`). Check each on its own before adding anything; if the
   base is wrong nothing on top saves it.
2. **Macro variation**: panel-to-panel or part-to-part tonal shifts (value
   ±3–8%, slight hue or roughness shifts), large soft gradients (sun fade,
   heat discoloration, grime pooling low). This is what makes big things
   read as made of many pieces. Usually the biggest single realism win.
3. **Meso detail**: panel lines and seams (thin, dark, low opacity), cavity
   grime, decals and markings, edge wear at contact zones.
4. **Micro detail**: roughness breakup, fine scratches, smudges. Mostly in
   **roughness**, barely in color, almost never in height on large or
   painted surfaces.
5. **Global weathering** last (dust, soot, streaks), with low opacity and
   driven by geometry (occlusion, sky exposure, gravity direction).

## 3. Rules that separate pro from amateur

- **Restraint.** Grime and dirt layers almost always end up at 10–40%
  opacity. If a weathering layer is at 100%, question it.
- **Edge wear is thin and placed.** Hairline on most edges; heavier only
  where hands, tools, cargo or debris actually hit. Never a uniform halo
  on every edge. Chips break up with fine noise, not big blobs.
- **Roughness carries the story.** Variation in roughness reads as
  "real" long before color variation does. Base roughness for paint is
  satin (≈0.3–0.5); raw machined metal 0.15–0.4; dust and soot 0.7–0.95
  but only where they are. A whole object near 0.8+ looks chalky; near
  0.1 looks plastic or wet.
- **Height and normal only where the form needs it.** Use them for seams,
  rivets, chipped paint thickness, real dents. Big flat panels stay flat:
  orange-peel noise in height is the most common "beginner" tell.
- **Value control.** No pure black or white albedo. Dark grime is
  dark brown/grey (≈30–60 sRGB), not 0. Keep the value structure of the
  design readable from far away (squint test).
- **Scale consistency.** Wear width, scratch length, noise size, panel line
  width and decal size must all agree with the object's real size. One
  wrong-scale element (big blotchy chips on a capital ship) breaks the
  illusion of the whole.
- **No baked lighting in base color.** AO, shadows and highlights belong to
  the renderer (micro-occlusion in cavities is the one tolerated exception,
  and still within range).

## 4. Review like a lead artist before showing anything

Render or view the asset and check, in this order:
1. **Each channel alone**: base color (value ranges, no lighting), roughness
   (variation present, not uniformly high or low), metallic (binary except
   under dirt or oxidation), height or normal (only intended forms).
2. **At the distance it will be seen** and close up. Large objects must
   still read as large up close (fine detail) and stay calm far away.
3. **Under two or three lighting setups** (bright overcast, hard key light,
   dark environment). A material that only works under one light is wrong.
4. **The silhouette of the design**: two-tone schemes and color blocking
   should read first, weathering second.
5. **Ask "why is this here?"** for every visible piece of weathering.

Report problems in those terms ("roughness is uniformly 0.8 on the hull",
"chips are ~10x too large for a 500 m ship") and fix the cause, usually a
layer's opacity, scale, mask level or a wrong base value, rather than
stacking correction layers.

## 5. Common failure modes and fixes

| Looks like | Usually because | Fix |
|---|---|---|
| Beginner model, noisy | height noise on big surfaces, weathering at 100% | remove micro height, drop grime to 10–30% |
| Small prop instead of huge object | default wear and noise sizes | shrink edge widths and noise sizes by the size ratio (5–20x) |
| Plastic | uniform low roughness, no variation | add roughness breakup, satin base |
| Chalky or dead | uniform high roughness from stacked dirt | lower dirt opacity, cap dust to exposed areas |
| Too dark | dark base + cavity/crease grime with wide reach | lift base value, narrow crease and cavity widths |
| Fake metal | grey albedo with metallic 1, or dirt marked metallic | metal reflectance 180–255 sRGB; dirt and rust are dielectric |
| Flat and procedural | no macro variation | per-part tonal shifts, large soft gradients |
