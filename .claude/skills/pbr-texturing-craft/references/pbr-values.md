# PBR values (metal/roughness)

From Adobe's *The PBR Guide, Part 2* (measured-data guidelines, not hard
laws). All values sRGB 0–255 unless noted; MP fill colors are sRGB 0–1
(divide by 255).

## Base color
| Surface | sRGB | 0–1 |
|---|---|---|
| Darkest dielectric (coal, rubber, soot) | ≥ 30 tolerant, ≥ 50 strict | ≥ 0.12 / 0.2 |
| Brightest dielectric (fresh snow, white paint) | ≤ 240 | ≤ 0.94 |
| Raw metal reflectance (metallic = 1) | 180–255 (70–100% specular) | 0.7–1.0 |

- Base color holds albedo for dielectrics and **reflectance** for metals.
  Grey metal at 0.3 with metallic 1 is wrong: it reads as dark plastic-metal.
  Iron/steel ≈ 0.53–0.6 linear → ~190–200 sRGB; aluminium, silver brighter;
  gold, copper tinted.
- Lower contrast than a diffuse photo; no AO, shadows or highlights.
- Dielectric F0 is 4% in the shader; leave it unless the material is
  unusual (gems, water).

## Metallic
- Binary in principle: 0 for dielectrics (paint, plastic, rubber, rust, dirt,
  oxidation), 235–255 for raw metal.
- Grey only where a thin dielectric (dirt, oxidation, grease film) partly
  covers metal. Where metallic drops below ~235, also lower the base color
  toward the covering material, or the shader reads dirt color as metal
  reflectance.
- Painted metal: paint = 0; chips and scratches through to bare metal = 1.
- Rust is dielectric (0) with dark orange-brown albedo.

## Roughness (0 smooth … 1 rough, linear)
No official ranges; roughness is "the most creative map" and tells the
story. Working values artists commonly use (craft knowledge, not from the
guide):
- polished chrome / glass 0.0–0.1; machined or brushed metal 0.15–0.4
- clear-coated or new paint 0.2–0.35; satin/military paint 0.35–0.55
- plastic 0.3–0.6; rubber 0.6–0.9; concrete, dust, soot 0.7–0.95
- Wear *polishes* contact points (lower roughness) and *dulls* exposed
  surfaces (higher). Fingerprints and smudges are roughness, not color.
- Reuse the normal or height detail in roughness (crevices collect grime,
  edges get polished).

## Common mistakes the guide calls out
- Dirt marked as raw metal in the metallic map.
- Metal reflectance too low in base color.
- Wrong dielectric F0.
- Texel density too low for the texture size, giving white fringes at
  metal/dielectric edges in metal/rough.
