# Normals and height

## What each channel is for
- **Height**: the *form*: low-frequency shapes (seams, plate thickness,
  rivets, dents). For real-time, keep high frequencies out of height and blur
  it slightly; parallax and displacement artifacts come from noisy height.
  Ray-traced displacement is the exception (it wants fine height).
- **Normal map**: high-frequency surface detail (micro scratches, grain,
  hammer marks) that geometry doesn't carry.
- **AO**: its own channel for the shader, affects diffuse only. Don't
  multiply it into base color.

## Normal maps miss vertical walls; derive and combine
A normal map stores slope per pixel. Where a height map *jumps* (brick sides,
rock edges, panel seams with depth), neighbouring normal pixels are "top"
and "bottom", both pointing up, so the walls between them interpolate to
pointing up too and light smears across them. A normal **derived from the
height map** (e.g. a Sobel gradient) gets those sides right but loses the
normal map's micro detail on the flat tops.

**Combined normals** take each where it's right:
1. Derive normal Nd from the height map.
2. Steepness weight `w = remap(dot(Nd, vertex_normal))`; ≈1 on flats,
   ≈0 on walls. A remap around 0.5 sharpens the transition.
3. `N = normalize(lerp(Nd, N_texture, w))`: texture normal on flats
   (keeps micro detail), derived normal on steep sides.

Use this whenever a material has strong height steps (bricks, cobbles,
pebbles, deep panel seams, armor plates with thickness). Plain lerp plus
normalize isn't a constant-speed rotation, but it is fine visually for this
blend.

## Practical rules
- If a bump looks like "orange peel" on a large surface, it's noise in
  height; remove it or move that frequency to roughness.
- Height intensity must match scale: a 5 mm seam on a 300 m ship is far
  below one texel. Use dark panel lines and roughness, not height.
- Check normals under a low grazing light; problems show there first.
