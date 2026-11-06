"""Index a decal atlas: find each decal from the alpha (opacity) channel and
write its rectangle, plus a numbered contact sheet to pick decals by eye.

    python tools/decal_atlas.py <pack folder> --alpha <file>[:channel] [--min-px 400]

Writes <pack>/atlas.json ({"size": [w, h], "decals": [{"id", "rect": [x, y, w, h],
"uv": [u0, v0, u1, v1], "coverage"}]}) and <pack>/sheet.jpg.
"""
import argparse
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import memguard  # noqa: E402

memguard.cap(3.0)
import numpy as np  # noqa: E402
from PIL import Image, ImageDraw  # noqa: E402
from scipy import ndimage  # noqa: E402

ap = argparse.ArgumentParser()
ap.add_argument("pack")
ap.add_argument("--alpha", required=True, help="image whose alpha (or :R/:G/:B/:A channel) marks the decals")
ap.add_argument("--min-px", type=int, default=400, help="ignore specks smaller than this many pixels")
ap.add_argument("--preview", help="image to draw the contact sheet from (default: the alpha image)")
args = ap.parse_args()
pack = Path(args.pack)
name, _, ch = args.alpha.partition(":")
img = Image.open(pack / name)
band = {"R": 0, "G": 1, "B": 2, "A": 3}[(ch or "A").upper()]
alpha = np.asarray(img.convert("RGBA"))[..., band] > 127
H, W = alpha.shape

# Close small gaps so one decal with holes (grilles) stays one component.
mask = ndimage.binary_closing(alpha, iterations=3)
labels, n = ndimage.label(mask)
decals = []
for i, sl in enumerate(ndimage.find_objects(labels), 1):
    ys, xs = sl
    area = int((labels[sl] == i).sum())
    if area < args.min_px:
        continue
    x, y, w, h = xs.start, ys.start, xs.stop - xs.start, ys.stop - ys.start
    decals.append({"rect": [x, y, w, h], "uv": [round(x / W, 5), round(y / H, 5), round((x + w) / W, 5), round((y + h) / H, 5)],
                   "coverage": round(area / (w * h), 3)})
# Reading order: rows top to bottom, then left to right.
decals.sort(key=lambda d: (d["rect"][1] // max(1, H // 24), d["rect"][0]))
for k, d in enumerate(decals):
    d["id"] = k
(pack / "atlas.json").write_text(json.dumps({"size": [W, H], "decals": decals}, indent=1))

prev = Image.open(pack / (args.preview or name)).convert("RGBA")
bg = Image.new("RGBA", prev.size, (70, 72, 78, 255))
bg.alpha_composite(prev)
sheet = bg.convert("RGB")
draw = ImageDraw.Draw(sheet)
for d in decals:
    x, y, w, h = d["rect"]
    draw.rectangle([x, y, x + w, y + h], outline=(255, 60, 60), width=3)
    draw.rectangle([x, y, x + 34, y + 22], fill=(255, 60, 60))
    draw.text((x + 4, y + 4), str(d["id"]), fill=(255, 255, 255))
sheet.thumbnail((1600, 1600))
sheet.save(pack / "sheet.jpg", quality=88)
print(f"{len(decals)} decals in {W}x{H}; wrote atlas.json and sheet.jpg")
