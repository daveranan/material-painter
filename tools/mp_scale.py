"""Rescale a Material Painter text project's procedural detail for the size
the object is meant to read as.

Presets are tuned for a hand-held prop filling the 2-unit view box. A capital
ship in the same box has features many times smaller, so edge/crease widths
must shrink and noise/variation must get finer by the same factor.

    python tools/mp_scale.py project.json --detail 5      # 5x finer
    python tools/mp_scale.py project.json --detail 0.5    # back off

Only layers that are (or sit inside) the given --layers ids are touched, if
given. Position-based generators (gradients, ID and part masks) are left
alone: they don't have a feature size.
"""
import argparse
import json

SIZE_FREE = {"World Gradient", "ID Mask", "Part Gradient", "Random Parts", "Custom Bake Map"}

ap = argparse.ArgumentParser()
ap.add_argument("project")
ap.add_argument("--detail", type=float, required=True, help="how many times finer the detail gets")
ap.add_argument("--layers", type=str, default="", help="comma-separated layer ids (folders include their contents)")
args = ap.parse_args()
k = args.detail

d = json.load(open(args.project, encoding="utf-8"))
for ts in d["texture_sets"]:
    L = ts["layers_bottom_to_top"]
    pick = {int(x) for x in args.layers.split(",") if x.strip()}
    parent = {l["id"]: l.get("parent") for l in L}

    def selected(i):
        while i is not None:
            if i in pick:
                return True
            i = parent.get(i)
        return False

    n = 0
    for l in L:
        if pick and not selected(l["id"]):
            continue
        f = l["content"].get("fill")
        if f:
            f["variation_scale"] = round(f.get("variation_scale", 4.0) * k, 4)
            if f.get("projection", "Uv") != "Uv":
                f["tiling"] = round(f.get("tiling", 1.0) * k, 4)
        g = (l.get("mask") or {}).get("generator")
        if g and g["kind"] not in SIZE_FREE:
            g["edge_width"] = round(max(g.get("edge_width", 0.02) / k, 0.0008), 5)
            g["scale"] = round(g.get("scale", 4.0) * k, 4)
            n += 1
    print(f"{ts.get('name', 'set')}: rescaled {n} generators and their fills by {k}x")
json.dump(d, open(args.project, "w", encoding="utf-8"), indent=2)
