"""Contact sheet + index for a folder of alphas/height stamps (any format PIL
reads, 8 or 16 bit). Loads one image at a time.

    python tools/alpha_sheet.py <folder> [--thumb 160] [--cols 10]

Writes <folder>/sheet.jpg (numbered thumbnails) and <folder>/index.json
([{"id", "file", "size", "bits", "coverage", "mean"}]).
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

ap = argparse.ArgumentParser()
ap.add_argument("folder")
ap.add_argument("--thumb", type=int, default=160)
ap.add_argument("--cols", type=int, default=10)
args = ap.parse_args()
folder = Path(args.folder)
files = sorted(p for p in folder.iterdir() if p.suffix.lower() in (".tif", ".tiff", ".png", ".jpg", ".tga", ".exr"))
T, C = args.thumb, args.cols
rows = (len(files) + C - 1) // C
sheet = Image.new("RGB", (C * T, rows * (T + 14)), (40, 40, 44))
draw = ImageDraw.Draw(sheet)
index = []
for i, f in enumerate(files):
    im = Image.open(f)
    a = np.asarray(im, dtype=np.float32)
    if a.ndim == 3:
        a = a[..., :3].mean(-1)
    peak = 65535.0 if im.mode.startswith("I") else 255.0
    a = a / peak
    index.append({"id": i, "file": f.name, "size": list(im.size), "bits": 16 if peak > 255 else 8,
                  "coverage": round(float((a > 0.02).mean()), 3), "mean": round(float(a.mean()), 3)})
    lo, hi = float(a.min()), float(a.max())
    t = Image.fromarray(((a - lo) / max(hi - lo, 1e-6) * 255).astype(np.uint8)).resize((T, T), Image.LANCZOS)
    x, y = (i % C) * T, (i // C) * (T + 14)
    sheet.paste(t.convert("RGB"), (x, y))
    draw.text((x + 3, y + T), f"{i} {f.stem[:18]}", fill=(220, 220, 220))
    del a, im
sheet.save(folder / "sheet.jpg", quality=85)
(folder / "index.json").write_text(json.dumps(index, indent=1))
print(f"{folder.name}: {len(files)} alphas -> sheet.jpg")
