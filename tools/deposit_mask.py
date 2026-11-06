"""Deposit (moss, snow, sand) mask from geometry, as a UV-space PNG for a
Material Painter paint mask.

Deposit settles on up-facing surfaces and creeps a little way over their
edges. Mesh-map generators can't tell "just below a ledge" from "any convex
corner", so this measures it in 3D: every texel on a steep face gets moss if
an up-facing surface lies just above it (within a reach that noise varies
between `--reach` limits), so it hangs over the lips in ragged fingers and
leaves bare corners and faces alone.

    python tools/deposit_mask.py rock.obj out.png --size-m 14 [--res 2048] [--seed 7]
"""
import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import numpy as np  # noqa: E402
from PIL import Image  # noqa: E402
from scipy import ndimage  # noqa: E402
from scipy.spatial import cKDTree  # noqa: E402

from strata_mask import fbm, load_obj, rasterize, smooth  # noqa: E402  (also applies the memory cap)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('obj')
    ap.add_argument('out')
    ap.add_argument('--size-m', type=float, required=True)
    ap.add_argument('--res', type=int, default=2048)
    ap.add_argument('--seed', type=int, default=7)
    ap.add_argument('--reach', type=float, nargs=2, default=[0.2, 0.8], help='metres moss hangs below a lip (min max)')
    ap.add_argument('--up', type=float, nargs=2, default=[0.66, 0.8], help='normal.y range that counts as up-facing')
    a = ap.parse_args()
    v, vt, vn, faces = load_obj(a.obj)
    v = v * (a.size_m / (v.max(0) - v.min(0)).max())
    pos, nrm = rasterize(v, vt, vn, faces, a.res)
    sel = ~np.isnan(pos[..., 0])
    p = pos[sel].astype(np.float64)
    n = nrm[sel]
    n /= np.linalg.norm(n, axis=1, keepdims=True) + 1e-9

    # Up-facing cover, patchy: bare spots of rock show through.
    patch = fbm(p * 1.4, a.seed)
    up = smooth(a.up[0], a.up[1], n[:, 1]) * smooth(0.18, 0.3, patch)

    # Over the lips: for steep texels, the nearest well-covered up-facing point above them.
    src = (up > 0.5)
    tree = cKDTree(p[src])
    reach = a.reach[0] + (a.reach[1] - a.reach[0]) * fbm(p * np.array([1.6, 0.4, 1.6]), a.seed + 3)
    steep = n[:, 1] < a.up[0]
    q = p[steep]
    d, j = tree.query(q, k=1, distance_upper_bound=a.reach[1] * 1.5)
    hit = np.isfinite(d)
    lip = np.zeros(len(q))
    if hit.any():
        above = p[src][j[hit]]
        dy = above[:, 1] - q[hit, 1]
        # Hanging below the surface, mostly straight down from it.
        down = smooth(-0.05, 0.05, dy) * (1 - smooth(0.6, 1.0, np.linalg.norm((above - q[hit])[:, [0, 2]], axis=1) / np.maximum(dy, 0.05)))
        lip[hit] = (1 - smooth(reach[steep][hit] * 0.6, reach[steep][hit], d[hit])) * down
    m = up.copy()
    m[steep] = np.maximum(m[steep], lip * smooth(0.25, 0.45, fbm(q * 2.5, a.seed + 5)))

    img = np.zeros((a.res, a.res), np.float32)
    img[sel] = m
    filled = sel.copy()
    for _ in range(16):
        grown = ndimage.grey_dilation(np.where(filled, img, -1), size=3)
        newly = ~filled & (grown >= 0)
        img[newly] = grown[newly]
        filled |= newly
    Image.fromarray((img.clip(0, 1) * 255).astype(np.uint8)).save(a.out)
    print(f'{a.out}: up-facing {up.mean():.2f}, lip spill on {(lip > 0.3).mean() * 100:.0f}% of steep texels')


if __name__ == '__main__':
    main()
