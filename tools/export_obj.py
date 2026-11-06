"""Write a GLB as OBJ for Material Painter, with the material IDs from
make_idmap.py as vertex colors (bake with ID source = vertex colors), and the
base-color texture as a PNG for use as a reference layer.

    python tools/export_obj.py ship.glb out/faces.json out/clusters.json out/ship_ids.obj
"""
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import make_idmap as m  # noqa: E402  (applies the memory cap)
import numpy as np  # noqa: E402
from PIL import Image  # noqa: E402

glb, faces_json, clusters_json, out = map(Path, [a for a in sys.argv[1:] if not a.startswith("--")][:4])
pos, uv, faces, img, nrm = m.load_glb(glb, normals=True)
faces_data = json.loads(faces_json.read_text())
cluster = np.array(faces_data["cluster"])
# --panels: split along panel borders, so each panel is its own mesh part
# (per-part variation then varies plate by plate).
panels = "--panels" in sys.argv
region = np.array(faces_data["region"]) if panels else np.zeros(len(cluster), int)
clusters = json.loads(clusters_json.read_text())["clusters"]
id_rgb = np.array([[int(c["id_color"][i:i + 2], 16) / 255 for i in (1, 3, 5)] for c in clusters])

# One OBJ vertex per (GLB vertex, cluster): colors are per vertex in OBJ.
corner = np.stack([faces.ravel(), np.repeat(cluster, 3), np.repeat(region, 3)], 1)
uniq, inv = np.unique(corner, axis=0, return_inverse=True)
inv = inv.ravel()
v_src, v_cl = uniq[:, 0], uniq[:, 1]

lines = [f"# {glb.name}: material IDs as vertex colors ({len(clusters)} clusters)", "o ship"]
P, C = pos[v_src], id_rgb[v_cl]
lines += [f"v {p[0]:.6f} {p[1]:.6f} {p[2]:.6f} {c[0]:.4f} {c[1]:.4f} {c[2]:.4f}" for p, c in zip(P, C)]
lines += [f"vt {t[0]:.6f} {1 - t[1]:.6f}" for t in uv[v_src]]  # OBJ v points up
lines += [f"vn {n[0]:.5f} {n[1]:.5f} {n[2]:.5f}" for n in nrm[v_src]]
f = inv.reshape(-1, 3) + 1
lines += [f"f {a}/{a}/{a} {b}/{b}/{b} {c}/{c}/{c}" for a, b, c in f]
out.write_text("\n".join(lines) + "\n")
print(f"wrote {out}: {len(uniq)} vertices, {len(f)} triangles, {len(np.unique(region))} panels")

tex = out.with_name(out.stem + "_basecolor.png")
img.draft("RGB", (4096, 4096))
img.convert("RGB").resize((4096, 4096), Image.LANCZOS).save(tex)
print(f"wrote {tex}")
