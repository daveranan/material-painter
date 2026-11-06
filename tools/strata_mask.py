"""Geological strata as a UV-space mask for a Material Painter paint mask.

Cliffs read as cliffs because of bedding: roughly horizontal bands of a
different rock running across every face. This places them by 3D position,
in metres, so bands stay level and continuous across UV islands and keep
the same thickness on every rock of a kit.

    python tools/strata_mask.py rock.obj out.png --size-m 14 [--res 2048] [--seed 1]

- --size-m: the rock's longest side in metres (the OBJ's units don't matter).
- Bands: thickness 0.12-0.6 m, spacing 0.35-1.5 m, a slight dip (a few degrees
  in a random direction) and a warp so they wander like real bedding.
- Only on steep faces (tops get the deposit layer instead), broken up by
  3D noise so they don't wrap the whole rock like tape.
"""
import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import memguard  # noqa: E402,F401  (4 GB memory cap)
import numpy as np  # noqa: E402
from PIL import Image  # noqa: E402
from scipy import ndimage  # noqa: E402


def load_obj(path):
    v, vt, vn, faces = [], [], [], []
    for line in open(path):
        if line.startswith('v '):
            v.append([float(x) for x in line.split()[1:4]])
        elif line.startswith('vt '):
            vt.append([float(x) for x in line.split()[1:3]])
        elif line.startswith('vn '):
            vn.append([float(x) for x in line.split()[1:4]])
        elif line.startswith('f '):
            idx = [[int(a) - 1 if a else -1 for a in c.split('/')] for c in line.split()[1:]]
            for k in range(1, len(idx) - 1):
                faces.append([idx[0], idx[k], idx[k + 1]])
    return np.array(v), np.array(vt), np.array(vn), np.array(faces)


def rasterize(v, vt, vn, faces, res):
    """Per texel: 3D position and normal (NaN where no triangle), texel centres
    within half a texel of a triangle included so borders are covered."""
    pos = np.full((res, res, 3), np.nan, np.float32)
    nrm = np.zeros((res, res, 3), np.float32)
    for f in faces:
        t = vt[f[:, 1]] * [res, res]
        t[:, 1] = res - t[:, 1]  # image rows go down
        p = v[f[:, 0]]
        n = vn[f[:, 2]] if f[0, 2] >= 0 else np.repeat(np.cross(p[1] - p[0], p[2] - p[0])[None], 3, 0)
        x0, y0 = np.floor(t.min(0) - 1).astype(int).clip(0, res - 1)
        x1, y1 = np.ceil(t.max(0) + 1).astype(int).clip(0, res - 1)
        if x1 < x0 or y1 < y0:
            continue
        xs, ys = np.meshgrid(np.arange(x0, x1 + 1) + 0.5, np.arange(y0, y1 + 1) + 0.5)
        d = (t[1, 0] - t[0, 0]) * (t[2, 1] - t[0, 1]) - (t[2, 0] - t[0, 0]) * (t[1, 1] - t[0, 1])
        if abs(d) < 1e-12:
            continue
        w1 = ((xs - t[0, 0]) * (t[2, 1] - t[0, 1]) - (t[2, 0] - t[0, 0]) * (ys - t[0, 1])) / d
        w2 = ((t[1, 0] - t[0, 0]) * (ys - t[0, 1]) - (xs - t[0, 0]) * (t[1, 1] - t[0, 1])) / d
        w0 = 1 - w1 - w2
        tol = -0.75 / max(np.sqrt(abs(d)), 1e-6)  # about half a texel outside, in barycentric units
        inside = (w0 >= tol) & (w1 >= tol) & (w2 >= tol)
        if not inside.any():
            continue
        w = np.stack([w0, w1, w2], -1)[inside].clip(0, 1)
        w /= w.sum(1, keepdims=True)
        yy, xx = (ys[inside] - 0.5).astype(int), (xs[inside] - 0.5).astype(int)
        pos[yy, xx] = w @ p
        nrm[yy, xx] = w @ n
    return pos, nrm


def vnoise(q, seed):
    """Smooth value noise at points q (N x 3)."""
    i = np.floor(q).astype(np.int64)
    f = q - i
    f = f * f * (3 - 2 * f)

    def h(o):
        k = (i + o) * np.array([73856093, 19349663, 83492791]) + seed * 2654435761
        k = (k[:, 0] ^ k[:, 1] ^ k[:, 2]) & 0xFFFFFF
        return (k * 2654435761 % 0xFFFFFF) / 0xFFFFFF

    out = 0
    for dx in (0, 1):
        for dy in (0, 1):
            for dz in (0, 1):
                wgt = (f[:, 0] if dx else 1 - f[:, 0]) * (f[:, 1] if dy else 1 - f[:, 1]) * (f[:, 2] if dz else 1 - f[:, 2])
                out = out + wgt * h(np.array([dx, dy, dz]))
    return out


def fbm(q, seed, octaves=4):
    s, a, t = 0, 0.5, 0
    for o in range(octaves):
        s += a * vnoise(q * 2 ** o, seed + o)
        t += a
        a *= 0.5
    return s / t


def smooth(e0, e1, x):
    t = np.clip((x - e0) / (e1 - e0), 0, 1)
    return t * t * (3 - 2 * t)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('obj')
    ap.add_argument('out')
    ap.add_argument('--size-m', type=float, required=True)
    ap.add_argument('--res', type=int, default=2048)
    ap.add_argument('--seed', type=int, default=1)
    a = ap.parse_args()
    v, vt, vn, faces = load_obj(a.obj)
    v = v * (a.size_m / (v.max(0) - v.min(0)).max())  # metres
    pos, nrm = rasterize(v, vt, vn, faces, a.res)
    sel = ~np.isnan(pos[..., 0])
    p = pos[sel].astype(np.float64)
    n = nrm[sel]
    n /= np.linalg.norm(n, axis=1, keepdims=True) + 1e-9
    rng = np.random.default_rng(a.seed)
    # Bedding planes: height plus a few degrees of dip and a slow warp.
    dip = np.radians(rng.uniform(2, 6))
    az = rng.uniform(0, 2 * np.pi)
    h = p[:, 1] + np.tan(dip) * (p[:, 0] * np.cos(az) + p[:, 2] * np.sin(az))
    h = h + (fbm(p * 0.12, a.seed + 10) - 0.5) * 1.2
    # Irregular spacing: walk band boundaries up the height range.
    lo, hi = h.min() - 3, h.max() + 3
    edges, z = [], lo
    while z < hi:
        thick = rng.uniform(0.12, 0.6)
        edges.append((z, z + thick))
        z += thick + rng.uniform(0.35, 1.5)
    m = np.zeros(len(p))
    soft = 0.06
    for b0, b1 in edges:
        m = np.maximum(m, smooth(b0 - soft, b0 + soft, h) * (1 - smooth(b1 - soft, b1 + soft, h)))
    # Steep faces only, and broken into lenses so bands don't run all the way round.
    steep = 1 - smooth(0.5, 0.8, n[:, 1])
    lens = smooth(0.3, 0.45, fbm(p * np.array([0.18, 0.6, 0.18]), a.seed + 20))
    m = m * steep * lens
    img = np.zeros((a.res, a.res), np.float32)
    img[sel] = m
    # Grow past island borders so filtering never reads black.
    filled = sel.copy()
    for _ in range(16):
        grown = ndimage.grey_dilation(np.where(filled, img, -1), size=3)
        newly = ~filled & (grown >= 0)
        img[newly] = grown[newly]
        filled |= newly
    Image.fromarray((img.clip(0, 1) * 255).astype(np.uint8)).save(a.out)
    print(f'{a.out}: {len(edges)} bands, {sel.mean() * 100:.0f}% texels covered, mean {m.mean():.2f}')


if __name__ == '__main__':
    main()
