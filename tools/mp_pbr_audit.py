"""Check a Material Painter text project's fill layers against PBR value
guidelines (Adobe PBR Guide, metal/roughness), and optionally fix them.

    python tools/mp_pbr_audit.py project.json          # report
    python tools/mp_pbr_audit.py project.json --fix    # also correct the file

Rules (fill colors are sRGB 0-1):
- dielectric base color: max channel >= 30 sRGB (0.12) and <= 240 (0.94)
- raw metal (metallic >= 0.9): reflectance 180-255 sRGB (max channel >= 0.71)
- dirt/grime/soot/oil/rust layers aren't metal: metallic must be 0
--fix scales colors into range (keeping hue) and clears metallic on grime.
It checks layer values, not the final composite: a 20% grime layer can still
be dark; the rule guards against near-black or fake-metal inputs.
"""
import json
import sys

GRIME_WORDS = ("grime", "dirt", "soot", "oil", "rust", "dust", "leak", "stain", "scorch", "gaps")
DARK, BRIGHT, METAL = 30 / 255, 240 / 255, 180 / 255

path = sys.argv[1]
fix = "--fix" in sys.argv
d = json.load(open(path, encoding="utf-8"))
problems = 0
for ts in d["texture_sets"]:
    for l in ts["layers_bottom_to_top"]:
        f = l["content"].get("fill")
        if not f or not l.get("visible", True):
            continue
        color_on, metal_on = f["enabled"][0], f["enabled"][2]
        c, m = f["color"], f["metallic"]
        name = f'{l["id"]} "{l["name"]}" (in {l.get("parent")})'
        grime = any(w in l["name"].lower() for w in GRIME_WORDS)
        if metal_on and grime and m > 0.0:
            problems += 1
            print(f"{name}: grime marked metallic ({m}); dirt and rust are dielectric")
            if fix:
                f["metallic"] = m = 0.0
        if not color_on:
            continue
        top = max(c)
        if metal_on and m >= 0.9:
            if top < METAL:
                problems += 1
                print(f"{name}: metal reflectance {round(top * 255)} sRGB < 180")
                if fix:
                    f["color"] = [round(min(x * METAL / top, 1.0), 3) for x in c]
        elif top < DARK:
            problems += 1
            print(f"{name}: dielectric too dark ({round(top * 255)} sRGB < 30)")
            if fix:
                k = DARK / max(top, 1e-3)
                f["color"] = [round(min(x * k, 1.0), 3) for x in c]
        elif top > BRIGHT and not any(w in l["name"].lower() for w in ("light", "glow", "emissive", "engine")):
            # (Lights are bright on purpose: MP has no emissive channel.)
            problems += 1
            print(f"{name}: dielectric too bright ({round(top * 255)} sRGB > 240)")
            if fix:
                f["color"] = [round(x * BRIGHT / top, 3) for x in c]
print(f"{problems} problem(s)" + (" fixed" if fix and problems else ""))
if fix and problems:
    json.dump(d, open(path, "w", encoding="utf-8"), indent=2)
sys.exit(1 if problems and not fix else 0)
