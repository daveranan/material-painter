"""What the game's displacement makes of each export: the height loaded at
1024, sampled at the mip its 5 cm grid picks (log2(step*1.5/texel)), lifted
by height minus the material's mean (today) or minus rest_height (proposed),
times relief. Reports the lift over each feature and across the tile, in cm.

    python tools/game_sim.py <export folder> <material> [<material> ...]

Reads `<material>.material.json`, `_ColorHeight.png` and `_NormalMasks.png`
from the "Game material (features)" export. Mirrors terrain.wgsl
(terrainRelief) and the 1024 load in view/src/splat.rs of the game; keep the
grid step (0.05 m) and load size in step with them."""
import json, math, sys
import numpy as np
from PIL import Image

if len(sys.argv) < 3:
    sys.exit(__doc__)
exp = sys.argv[1]
for name in sys.argv[2:]:
    m = json.load(open(f'{exp}/{name}.material.json'))
    tile, relief, rest = m['tile_m'], m['relief_m'], m.get('rest_height', 0.5)
    h = Image.open(f'{exp}/{name}_ColorHeight.png').convert('RGBA').split()[3].resize((1024, 1024), Image.LANCZOS)
    nm = np.asarray(Image.open(f'{exp}/{name}_NormalMasks.png').convert('RGBA').resize((1024, 1024)), dtype=np.float32) / 255
    lod = math.log2(0.05 * 1.5 / (tile / 1024))
    lo, hi = int(lod), int(lod) + 1
    def mip(l):
        n = 1024 >> l
        return np.asarray(h.resize((n, n), Image.BOX).resize((1024, 1024), Image.BILINEAR), dtype=np.float32) / 255
    g = mip(lo) * (hi - lod) + mip(hi) * (lod - lo)
    full = np.asarray(h, dtype=np.float32) / 255
    mean = full.mean()
    print(f'{name}: mip {lod:.1f} ({tile * 100 / (1024 >> lo):.1f}-{tile * 100 / (1024 >> hi):.1f} cm/texel), mean {mean:.2f}, rest {rest:.2f}')
    for label, mask in [(m['features']['mask1'], nm[..., 2] > 0.5), (m['features']['mask2'], nm[..., 3] > 0.5)]:
        if mask.sum() < 50:
            continue
        a = (g[mask] - mean) * relief * 100
        b = (g[mask] - rest) * relief * 100
        print(f'  {label:6s} lift p50/p90  from mean {np.percentile(a, 50):4.1f}/{np.percentile(a, 90):4.1f} cm   from rest {np.percentile(b, 50):4.1f}/{np.percentile(b, 90):4.1f} cm')
    t = (g - rest) * relief * 100
    print(f'  tile range from rest p5..p95 {np.percentile(t, 5):.1f}..{np.percentile(t, 95):.1f} cm')
