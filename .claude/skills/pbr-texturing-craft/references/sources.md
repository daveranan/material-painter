# Sources

Summarized in our own words; consult the originals for figures and images.

- Adobe, *The PBR Guide, Part 2*: base color, metallic and roughness rules, common mistakes.
  https://www.adobe.com/learn/substance-3d-designer/web/the-pbr-guide-part-2
- Adobe, Substance 3D Sampler, *PBR Validate*: what validation checks.
  https://experienceleague.adobe.com/en/docs/substance-3d-sampler/using/filters/tools/pbr-validate
- Parth Naik, *Bench Grinder: Workflow Tips for Hard-Surface Prop Production* (80.lv): material separation,
  paint-over-metal stacks, decals, then wear, then dust, lighting for review.
  https://80.lv/articles/bench-grinder-workflow-tips-for-hard-surface-prop-production/
- World of Level Design, *Substance Painter roughness variation*: roughness breakup methods,
  single-channel checks. https://worldofleveldesign.com/categories/substance/painter/roughness-variation.php
- 80.lv, *Texturing Weapons: Tips on Creating Surfaces, Weathering & Stylized Assets*: wide base palettes,
  weathering by source (oil, dust, dirt).
  https://80.lv/articles/texturing-weapons-tips-on-creating-surfaces-weathering-stylized-assets
- Martin de Graaf, *Material study: terre ossidate*: macro/meso/micro breakdown.
  https://martindegraaf.artstation.com/blog/pMv/material-study-terre-ossidate
- Games Artist, *Sci-Fi Panel*: color blocks, then gradients and darker values near cavities, then
  decals and soft discoloration. https://gamesartist.co.uk/sci-fi-panel/
- Brown Bag Films Labs, *Sci-fi ship model texturing breakdown*: dirt masks for scale.
  https://www.brownbagfilms.com/labs/entry/ercldok-sci-fi-ship-model-breakdown-tutorial
- Pixel Sanctuary, *Sci-fi architecture: greebles, paneling, decals and scale cues*.
  https://www.pixelsanctuary.com/tutorials/scifi-architecture-greebles
- Polycount, *sci-fi paneling* (random paneling for scale). https://polycount.com/discussion/94906/sci-fi-paneling
- Unreal forums, *How to texture large scale models* and *How to texture/model a huge asset*:
  modular UV sharing, tiling plus decal atlases.
  https://forums.unrealengine.com/t/how-to-texture-large-scale-models/305113 ,
  https://forums.unrealengine.com/t/how-to-texture-model-a-huge-asset/59992
- r/starcitizen, *How Star Citizen ship art actually works* (industry artist, not affiliated with SC):
  fixed-density tiling materials, pre-modeled channel-selective decal atlases, chamfers with
  face-weighted normals. https://www.reddit.com/comments/3ogi3o/ (Polycount discussion it
  draws on: https://polycount.com/discussion/155894, not readable by our tools)
- Unreal docs, *Dynamic normal maps* (Sobel height-to-normal).
  https://docs.unrealengine.com/udk/Three/DevelopmentKitGemsDynamicNormalMap.html
- Combined normals (derived from height, blended by dot with the vertex normal): from a video transcript
  the user shared (FBomb material demo).

Craft knowledge not taken from a cited page (roughness working ranges,
opacity ranges, scale factors) is marked as such where it appears; it came
out of the Material Painter ship trial in this project.
