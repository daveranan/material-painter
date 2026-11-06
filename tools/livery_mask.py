"""Livery masks (bands, stripes, hull text) placed by 3D position, written as
UV-space PNG masks for Material Painter paint masks.

Painted liveries follow the hull, not the UV layout: each texel's 3D position
decides whether it is in a band or under a letter, so stripes stay straight
and continuous across Tripo-style fragmented UVs.

    python tools/livery_mask.py model.glb spec.json out_dir [--res 4096] [--faces out/faces.json]

spec.json:
{
  "axis": "z", "bow": "+",                  # long axis, and which end is the bow
  "bands": [{"name": "ochre_1", "from_bow": [0.22, 0.245], "ids": [1, 2]}],
  "text":  [{"name": "hull_number", "text": "C-3", "from_bow": 0.13, "height": 0.35,
             "y": 0.55, "font": "C:/Windows/Fonts/bahnschrift.ttf", "sides": ["+x", "-x"]}]
}
- from_bow: fractions of the hull length measured from the bow.
- ids: material clusters (faces.json) a band may paint; omit for all.
- y: optional [low, high] height range for a band, as fractions of hull height.
- text height and y are fractions of the hull height; it goes on faces
  facing the side (|n.x| > 0.5), reading bow-to-stern on both sides.
- facing (bands): "up", "down" or "side" keeps a band to faces turned that
  way (e.g. accents only on the dorsal panels, not the undercarriage).
- deck_edge (bands): [a, b] fractions of the local half width of the
  upward-facing deck: a lengthwise stripe that follows the deck's taper.
- Bands sharing a name are merged into one mask (one MP layer).
- weather (bands and text): {"grunge": 0.25, "chips": 0.35, "rag": 0.5, "seed": 1}
  patchy paint, chipped spots and ragged text edges, from 3D-position noise so
  it continues across UV seams. 0 turns a part off.
- streaks (text): {"length": 0.8, "strength": 0.5} also writes
  <name>_streaks.png: paint run-off below the letters (length in letter heights).
"""
import argparse
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import make_idmap as m  # noqa: E402  (applies the 4 GB memory cap)
import numpy as np  # noqa: E402
from PIL import Image, ImageDraw, ImageFont  # noqa: E402
from scipy import ndimage  # noqa: E402

ap = argparse.ArgumentParser()
ap.add_argument("glb")
ap.add_argument("spec")
ap.add_argument("out")
ap.add_argument("--res", type=int, default=4096)
ap.add_argument("--faces", default=None, help="faces.json from make_idmap (for band ids)")
args = ap.parse_args()
spec = json.loads(Path(args.spec).read_text())
out = Path(args.out)
out.mkdir(parents=True, exist_ok=True)

pos, uv, faces, _ = m.load_glb(args.glb)
R = args.res
cluster = np.array(json.loads(Path(args.faces).read_text())["cluster"]) if args.faces else None
tri = pos[faces]
nrm = np.cross(tri[:, 1] - tri[:, 0], tri[:, 2] - tri[:, 0])
nrm /= np.maximum(np.linalg.norm(nrm, axis=1, keepdims=True), 1e-20)

# UV raster: which face and where inside it, per texel (v down, like MP).
fuv = uv[faces] * R
fid, bary = m.rasterize(fuv, R, R)
hit = fid >= 0
f_hit = fid[hit]
p = np.einsum("nk,nkd->nd", bary[hit], tri[f_hit])
del bary

ax = "xyz".index(spec.get("axis", "z"))
lo, hi = pos[:, ax].min(), pos[:, ax].max()
along = (hi - p[:, ax]) / (hi - lo) if spec.get("bow", "+") == "+" else (p[:, ax] - lo) / (hi - lo)
ylo, yhi = pos[:, 1].min(), pos[:, 1].max()
up = (p[:, 1] - ylo) / (yhi - ylo)


span = hi - lo
pn = (p - pos.min(0)) / span  # hull-normalized position for noise


def hash3(i, seed):
    h = (i[:, 0] * 73856093) ^ (i[:, 1] * 19349663) ^ (i[:, 2] * 83492791) ^ (seed * 2654435761)
    h = (h ^ (h >> 13)) * 1274126177
    return ((h ^ (h >> 16)) & 0xFFFF).astype(np.float32) / 65535.0


def vnoise(q, seed):
    i = np.floor(q).astype(np.int64)
    f = (q - i).astype(np.float32)
    f = f * f * (3 - 2 * f)
    out = np.zeros(len(q), np.float32)
    for dx in (0, 1):
        for dy in (0, 1):
            for dz in (0, 1):
                w = (f[:, 0] if dx else 1 - f[:, 0]) * (f[:, 1] if dy else 1 - f[:, 1]) * (f[:, 2] if dz else 1 - f[:, 2])
                out += w * hash3(i + np.array([dx, dy, dz]), seed)
    return out


def fbm(sel, freq, octaves, seed):
    """Fractal noise in [0, 1] at the texels `sel`; freq in cycles per hull length."""
    q = pn[sel] * freq
    tot, amp, norm = np.zeros(len(q), np.float32), 1.0, 0.0
    for o in range(octaves):
        tot += amp * vnoise(q * (2 ** o), seed + o)
        norm += amp
        amp *= 0.5
    return tot / norm


def smooth(e0, e1, x):
    t = np.clip((x - e0) / (e1 - e0), 0, 1)
    return t * t * (3 - 2 * t)


def weather(vals, w):
    """Patchy, chipped paint: scales mask values (float 0..1) in place."""
    if not w:
        return vals
    sel = np.nonzero(vals > 0)[0]
    seed = int(w.get("seed", 1))
    g = w.get("grunge", 0.25)
    if g > 0:  # uneven paint: thin patches, ~5 m blotches and finer mottling
        n = fbm(sel, 90, 4, seed)
        vals[sel] *= 1 - g * smooth(0.35, 0.75, n)
    c = w.get("chips", 0.35)
    if c > 0:  # rare, ragged chips of a few metres, more where the paint is worn thin
        n = fbm(sel, 260, 4, seed + 10) * 0.75 + fbm(sel, 60, 2, seed + 11) * 0.25
        t = 0.72 - 0.08 * c
        vals[sel] *= 1 - smooth(t, t + 0.025, n)
    return vals


masks = {}


def put(name, values):
    masks[name] = np.maximum(masks[name], values) if name in masks else values


def save(name, values):
    mask = np.zeros((R, R), np.uint8)
    mask[hit] = values
    mask = m.dilate_ids(mask[..., None], hit, 4)[..., 0]  # bleed past UV seams
    Image.fromarray(mask).save(out / f"{name}.png")
    print(f"{name}.png: {int((values > 127).sum())} texels")


FACING = {"up": (1, 1), "down": (1, -1), "side": (0, None)}


def deck_half_width(y_from):
    """Half width of the upward-facing deck at each texel's position along the
    hull (above height fraction y_from), so stripes can follow the deck edge."""
    bins = 200
    fc = tri.mean(1)
    f_al = (hi - fc[:, ax]) / (hi - lo) if spec.get("bow", "+") == "+" else (fc[:, ax] - lo) / (hi - lo)
    f_up = (fc[:, 1] - ylo) / (yhi - ylo)
    deck = (nrm[:, 1] > 0.5) & (f_up >= y_from)
    hw = np.zeros(bins)
    k = np.clip((f_al[deck] * bins).astype(int), 0, bins - 1)
    np.maximum.at(hw, k, np.abs(tri[deck][:, :, 0]).max(1))
    hw = ndimage.maximum_filter1d(hw, 3)
    return hw[np.clip((along * bins).astype(int), 0, bins - 1)]
for b in spec.get("bands", []):
    a0, a1 = b["from_bow"]
    inside = (along >= a0) & (along <= a1)
    if "facing" in b:
        ax_n, sgn = FACING[b["facing"]]
        n_hit = nrm[f_hit, ax_n]
        inside &= (n_hit * sgn > 0.5) if sgn else (np.abs(nrm[f_hit, 0]) > 0.5)
    if "deck_edge" in b:  # lengthwise stripe at this fraction of the local deck half width
        e0, e1 = b["deck_edge"]
        rel = np.abs(p[:, 0]) / np.maximum(deck_half_width(b.get("y", [0.7, 1])[0]), 1e-6)
        inside &= (rel >= e0) & (rel <= e1) & (nrm[f_hit, 1] > 0.5)
    if "ids" in b and cluster is not None:
        inside &= np.isin(cluster[f_hit], b["ids"])
    if "y" in b:  # height range as fractions of the hull height (keep bands off the undercarriage)
        inside &= (up >= b["y"][0]) & (up <= b["y"][1])
    put(b["name"], weather(inside.astype(np.float32), b.get("weather")))

for t in spec.get("text", []):
    font = ImageFont.truetype(t.get("font", "C:/Windows/Fonts/bahnschrift.ttf"), 512)
    box = font.getbbox(t["text"])
    glyphs = Image.new("L", (box[2] - box[0] + 40, box[3] - box[1] + 40), 0)
    ImageDraw.Draw(glyphs).text((20 - box[0], 20 - box[1]), t["text"], fill=255, font=font)
    g = np.asarray(glyphs, np.float32) / 255
    gh, gw = g.shape
    w8 = t.get("weather") or {}
    st = t.get("streaks")
    # Canvas below the letters for run-off (in letter heights).
    ext = st.get("length", 0.8) if st else 0.0
    rows = int(gh * (1 + ext))
    canvas = np.zeros((rows, gw), np.float32)
    canvas[:gh] = g
    soft = ndimage.gaussian_filter(canvas, gh * 0.012)  # for ragged edges
    streak = None
    if st:
        rng = np.random.default_rng(int(w8.get("seed", 1)) + 7)
        # Per-column run length and strength, smooth across neighbouring columns.
        # Thin runs of very different lengths: a few long, most short.
        L = ndimage.gaussian_filter1d(gh * ext * (0.1 + 1.4 * rng.random(gw) ** 2), 2) + 4
        A = ndimage.gaussian_filter1d(rng.random(gw) ** 2.5, 1.2)
        decay = np.exp(-1.0 / L).astype(np.float32)
        S = np.zeros_like(canvas)
        for y in range(1, rows):
            S[y] = np.maximum(canvas[y] * A, S[y - 1] * decay)
        streak = ndimage.gaussian_filter(S * (1 - ndimage.grey_dilation(canvas, size=(3, 3))), (1.5, 2.5))
        streak = np.clip(streak / max(float(np.percentile(streak[streak > 0], 99.5)), 1e-6), 0, 1)
        streak *= st.get("strength", 0.5)
    h = t["height"]                       # fraction of hull height
    w = h * (yhi - ylo) * gw / gh / (hi - lo)  # same aspect, as a fraction of the length
    c_along, c_up = t["from_bow"], t["y"]
    vals = np.zeros(len(p), np.float32)
    svals = np.zeros(len(p), np.float32)
    for side in t.get("sides", ["+x", "-x"]):
        sgn = 1 if side[0] == "+" else -1
        facing = nrm[f_hit, 0] * sgn > 0.5
        # Read bow-to-stern on both sides: u grows toward the stern.
        u = (along - (c_along - w / 2)) / w
        v = 1 - (up - (c_up - h / 2)) / h       # 0 top of the letters, 1 bottom, >1 below
        ok = facing & (u >= 0) & (u < 1) & (v >= 0) & (v < 1 + ext)
        # Seen from +x the bow is on the left, from -x on the right; text reads
        # left to right on both, so the -x side runs stern-to-bow.
        ok_u = u if sgn > 0 else 1 - u
        idx = np.nonzero(ok)[0]
        gx = np.clip((ok_u[idx] * gw).astype(int), 0, gw - 1)
        gy = np.clip((v[idx] * gh).astype(int), 0, rows - 1)
        if w8.get("rag", 0.5) > 0:  # ragged, worn letter edges: noisy threshold of the soft glyph
            n = fbm(idx, 220, 3, int(w8.get("seed", 1)) + 20)
            thr = 0.5 + w8.get("rag", 0.5) * 0.5 * (n - 0.5)
            a = smooth(thr - 0.09, thr + 0.09, soft[gy, gx])
        else:
            a = canvas[gy, gx]
        vals[idx] = np.maximum(vals[idx], a)
        if streak is not None:
            svals[idx] = np.maximum(svals[idx], streak[gy, gx])
    put(t["name"], weather(vals, w8))
    if streak is not None:
        sel = np.nonzero(svals > 0)[0]
        svals[sel] *= 0.55 + 0.45 * fbm(sel, 300, 3, 5)  # uneven run-off
        put(t["name"] + "_streaks", svals)

for name, v in masks.items():
    save(name, (np.clip(v, 0, 1) * 255).astype(np.uint8))
