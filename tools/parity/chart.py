"""Material chart: the same sphere, values and lighting in Material Painter
and in Blender EEVEE, side by side, with the mean brightness of each.

    python tools/parity/chart.py --layer <index of a top fill layer> --out out/parity/chart

MP must show the sphere (out/parity/sphere.obj) with that fill layer on top,
tonemap "Linear (clip)" and the key light off (preview_settings).
"""
import argparse
import json
import os
import subprocess
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parents[2]
MP = ROOT / "target/debug/mp.exe"
WORK = Path(os.environ.get("MP_WORKSPACE", ROOT.parent))  # renders go outside the repo
# Blender: $BLENDER, else a portable install in the workspace's tools/ (not committed), else on PATH.
_PORTABLE = WORK / "tools/blender-4.5.15-windows-x64/blender.exe"
BLENDER = Path(os.environ.get("BLENDER", _PORTABLE if _PORTABLE.exists() else "blender"))

ap = argparse.ArgumentParser()
ap.add_argument("--layer", type=int, required=True)
ap.add_argument("--out", default=str(WORK / "out/parity/chart"))
ap.add_argument("--hdri", default=str(ROOT / "assets/hdri/studio_small_09_1k.hdr"))
ap.add_argument("--size", type=int, default=300)
ap.add_argument("--yaw", type=float, default=0.6)
ap.add_argument("--pitch", type=float, default=0.25)
ap.add_argument("--dist", type=float, default=3.4)
ap.add_argument("--raytrace", action="store_true")
ap.add_argument("--engine", default="eevee", choices=["eevee", "cycles"])
ap.add_argument("--skip-mp", action="store_true", help="reuse the MP renders already in --out")
ap.add_argument("--reuse-blender", help="folder with Blender renders (bl_*.png) from an earlier run")
a = ap.parse_args()
out = Path(a.out).resolve()
out.mkdir(parents=True, exist_ok=True)

ROUGH = [0.1, 0.3, 0.5, 0.7, 0.9]
MATS = [("charcoal paint", [0.2, 0.2, 0.2], 0.0), ("grey paint", [0.5, 0.5, 0.5], 0.0),
        ("white paint", [0.8, 0.8, 0.8], 0.0), ("steel", [0.7, 0.7, 0.7], 1.0)]
cam = dict(yaw=a.yaw, pitch=a.pitch, distance=a.dist, target=[0, 0, 0])


def mp(*args):
    return subprocess.run([str(MP), *args], capture_output=True, text=True, check=True).stdout


def mp_render(path):
    mp("render_view", "--json", json.dumps({**cam, "mode": "Material", "width": a.size, "height": a.size,
                                            "transparent": True}), "--out", str(path))


rows = []
for name, col, metal in MATS:
    row = []
    for r in ROUGH:
        tag = f"{name.replace(' ', '_')}_r{r}"
        if not a.skip_mp:
            mp("update_layer", "--json", json.dumps({"index": a.layer, "color": col, "roughness": r, "metallic": metal,
                                                 "height": 0.0, "color_variation": 0, "roughness_variation": 0,
                                                 "part_variation": 0}))
            mp_render(out / f"mp_{tag}.png")
        if a.reuse_blender:
            import shutil
            shutil.copy(Path(a.reuse_blender) / f"bl_{tag}.png", out / f"bl_{tag}.png")
            row.append(tag)
            continue
        subprocess.run([str(BLENDER), "-b", "--factory-startup", "-P", str(ROOT / "tools/parity/blender_render.py"), "--",
                        "--sphere", *map(str, col), "--rough", str(r), "--metal", str(metal),
                        "--hdri", a.hdri, "--yaw", str(a.yaw), "--pitch", str(a.pitch), "--dist", str(a.dist),
                        "--width", str(a.size), "--height", str(a.size),
                        "--samples", "128" if a.engine == "cycles" else "32", "--engine", a.engine,
                        *(["--raytrace"] if a.raytrace else []),
                        "--out", str(out / f"bl_{tag}.png")], capture_output=True, check=True)
        row.append(tag)
    rows.append((name, row))

# Sheet: per material two rows (MP above Blender), and brightness stats.
S = a.size
sheet = Image.new("RGB", (S * len(ROUGH) + 140, S * 2 * len(MATS)), (40, 40, 40))
d = ImageDraw.Draw(sheet)
stats = {}
for i, (name, row) in enumerate(rows):
    for j, tag in enumerate(row):
        for k, app in enumerate(("mp", "bl")):
            im = Image.open(out / f"{app}_{tag}.png").convert("RGBA")
            bg = Image.new("RGBA", im.size, (40, 40, 40, 255))
            bg.alpha_composite(im)
            sheet.paste(bg.convert("RGB"), (140 + j * S, (2 * i + k) * S))
            px = np.asarray(im, float)
            m = px[..., 3] > 250
            stats[f"{app}_{tag}"] = round(float(px[..., :3][m].mean()), 1)
        d.text((140 + j * S + 6, (2 * i) * S + 6), f"r={ROUGH[j]}  mp {stats['mp_' + tag]}  bl {stats['bl_' + tag]}", fill=(255, 255, 0))
    d.text((6, 2 * i * S + 10), f"{name}\nMP", fill=(255, 255, 255))
    d.text((6, (2 * i + 1) * S + 10), "Blender\n" + a.engine.upper(), fill=(255, 255, 255))
sheet.save(out / "sheet.png")
json.dump(stats, open(out / "stats.json", "w"), indent=1)
print(json.dumps(stats))
