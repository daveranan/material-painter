"""Derive material IDs for a single-mesh, single-texture GLB (e.g. Tripo output).

Pipeline
  1. Weld vertices by position (ignore UV seams) to get real surface topology.
  2. Grow regions across edges whose dihedral angle < --angle  -> panels.
  3. Sample the base-color texture per face, cluster colors (Lab, k-means).
  4. Snap: each panel takes its area-weighted majority color cluster
     (mixed panels fall back to per-face clusters, lightly smoothed).
  5. Write ID maps (UV space), JSON for an LLM/human to label, a vertex-colored
     GLB, and preview renders.

Memory: the 8K texture is decoded once and downscaled immediately; nothing is
ever split into sub-meshes. A hard process memory cap is applied first.
"""
import argparse
import json
import struct
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import memguard  # noqa: E402

memguard.cap(4.0)

import io  # noqa: E402

import numpy as np  # noqa: E402
from PIL import Image, ImageDraw  # noqa: E402
from scipy import sparse  # noqa: E402
from scipy.sparse.csgraph import connected_components  # noqa: E402

Image.MAX_IMAGE_PIXELS = None

PALETTE = np.array([
    (230, 25, 75), (60, 180, 75), (255, 225, 25), (0, 130, 200), (245, 130, 48),
    (145, 30, 180), (70, 240, 240), (240, 50, 230), (210, 245, 60), (250, 190, 212),
    (0, 128, 128), (220, 190, 255), (170, 110, 40), (128, 0, 0), (170, 255, 195),
    (128, 128, 0), (0, 0, 128), (128, 128, 128),
], np.uint8)

T0 = time.time()


def log(*a):
    print(f"[{time.time() - T0:6.1f}s]", *a, flush=True)


# ---------------------------------------------------------------- GLB loading
def load_glb(path, normals=False):
    data = Path(path).read_bytes()
    jlen = struct.unpack_from("<I", data, 12)[0]
    j = json.loads(data[20:20 + jlen])
    boff = 20 + jlen
    blen = struct.unpack_from("<I", data, boff)[0]
    binbuf = data[boff + 8: boff + 8 + blen]

    def accessor(i):
        a = j["accessors"][i]
        bv = j["bufferViews"][a["bufferView"]]
        dt = np.dtype({5126: np.float32, 5125: np.uint32, 5123: np.uint16, 5121: np.uint8}[a["componentType"]])
        n = {"SCALAR": 1, "VEC2": 2, "VEC3": 3, "VEC4": 4}[a["type"]]
        off = bv.get("byteOffset", 0) + a.get("byteOffset", 0)
        stride = bv.get("byteStride") or dt.itemsize * n
        arr = np.ndarray((a["count"], n), dt, binbuf, off, (stride, dt.itemsize))
        return np.array(arr)  # own copy

    prims = [p for m in j["meshes"] for p in m["primitives"]]
    if len(prims) != 1:
        log(f"warning: {len(prims)} primitives, using the first")
    p = prims[0]
    pos = accessor(p["attributes"]["POSITION"]).astype(np.float64)
    uv = accessor(p["attributes"]["TEXCOORD_0"]).astype(np.float64)
    faces = accessor(p["indices"]).reshape(-1, 3).astype(np.int64)

    mat = j["materials"][p.get("material", 0)]
    tex_i = mat["pbrMetallicRoughness"]["baseColorTexture"]["index"]
    img_i = j["textures"][tex_i]["source"]
    bv = j["bufferViews"][j["images"][img_i]["bufferView"]]
    o = bv.get("byteOffset", 0)
    img = Image.open(io.BytesIO(binbuf[o:o + bv["byteLength"]]))
    if normals:
        return pos, uv, faces, img, accessor(p["attributes"]["NORMAL"]).astype(np.float64)
    return pos, uv, faces, img


# ---------------------------------------------------------------- color
def srgb_to_lab(rgb):
    rgb = rgb.astype(np.float32)
    c = np.where(rgb <= 0.04045, rgb / 12.92, ((rgb + 0.055) / 1.055) ** 2.4)
    m = np.array([[0.4124564, 0.3575761, 0.1804375],
                  [0.2126729, 0.7151522, 0.0721750],
                  [0.0193339, 0.1191920, 0.9503041]], np.float32)
    xyz = (c @ m.T) / np.array([0.95047, 1.0, 1.08883], np.float32)
    f = np.where(xyz > 0.008856, np.cbrt(xyz), 7.787 * xyz + 16 / 116)
    return np.stack([116 * f[..., 1] - 16, 500 * (f[..., 0] - f[..., 1]), 200 * (f[..., 1] - f[..., 2])], -1)


def kmeans(x, w, k, iters=60, seed=0):
    """Weighted k-means++ (small data; avoids sklearn thread spin-up)."""
    rng = np.random.default_rng(seed)
    p = w / w.sum()
    centers = [x[rng.choice(len(x), p=p)]]
    for _ in range(1, k):
        d = np.min(((x[:, None] - np.array(centers)[None]) ** 2).sum(-1), 1) * w
        centers.append(x[rng.choice(len(x), p=d / d.sum())])
    c = np.array(centers)
    for _ in range(iters):
        lab = ((x[:, None] - c[None]) ** 2).sum(-1).argmin(1)
        new = np.array([np.average(x[lab == i], 0, w[lab == i]) if (lab == i).any() else c[i] for i in range(k)])
        if np.allclose(new, c, atol=1e-3):
            break
        c = new
    lab = ((x[:, None] - c[None]) ** 2).sum(-1).argmin(1)
    return c, lab


# ---------------------------------------------------------------- raster
def rasterize(tri, W, H, depth=None):
    """tri: (F,3,2) pixel coords. Returns face-id buffer (H,W) and barycentrics (H,W,3)."""
    fid = np.full((H, W), -1, np.int32)
    bary = np.zeros((H, W, 3), np.float32)
    zbuf = np.full((H, W), np.inf, np.float32) if depth is not None else None
    for f in range(len(tri)):
        (x0, y0), (x1, y1), (x2, y2) = tri[f]
        den = (y1 - y2) * (x0 - x2) + (x2 - x1) * (y0 - y2)
        if abs(den) < 1e-12:
            continue
        xa = max(int(np.floor(min(x0, x1, x2))), 0)
        xb = min(int(np.ceil(max(x0, x1, x2))), W - 1)
        ya = max(int(np.floor(min(y0, y1, y2))), 0)
        yb = min(int(np.ceil(max(y0, y1, y2))), H - 1)
        if xa > xb or ya > yb:
            continue
        px, py = np.meshgrid(np.arange(xa, xb + 1) + 0.5, np.arange(ya, yb + 1) + 0.5)
        l0 = ((y1 - y2) * (px - x2) + (x2 - x1) * (py - y2)) / den
        l1 = ((y2 - y0) * (px - x2) + (x0 - x2) * (py - y2)) / den
        l2 = 1 - l0 - l1
        inside = (l0 >= -1e-4) & (l1 >= -1e-4) & (l2 >= -1e-4)
        if not inside.any():
            continue
        sl = (slice(ya, yb + 1), slice(xa, xb + 1))
        if depth is not None:
            z = l0 * depth[f, 0] + l1 * depth[f, 1] + l2 * depth[f, 2]
            inside &= z < zbuf[sl]
            zbuf[sl][inside] = z[inside]
        fid[sl][inside] = f
        b = bary[sl]
        b[inside] = np.stack([l0, l1, l2], -1)[inside]
    return fid, bary


def dilate_ids(img, mask, passes):
    """Grow covered pixels outward so UV seams don't bleed background."""
    img, mask = img.copy(), mask.copy()
    for _ in range(passes):
        for dy, dx in ((0, 1), (0, -1), (1, 0), (-1, 0)):
            src_m = np.roll(mask, (dy, dx), (0, 1))
            src = np.roll(img, (dy, dx), (0, 1))
            grow = src_m & ~mask
            img[grow] = src[grow]
            mask = mask | grow
    return img


# ---------------------------------------------------------------- main
def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("glb")
    ap.add_argument("--out", default="out")
    ap.add_argument("--k", type=int, default=14, help="initial color clusters (over-cluster, then merge)")
    ap.add_argument("--merge", type=float, default=12.0, help="merge clusters closer than this delta-E")
    ap.add_argument("--angle", type=float, default=30.0, help="sharp-edge threshold in degrees")
    ap.add_argument("--purity", type=float, default=0.6, help="min majority share to snap a region")
    ap.add_argument("--flat-angle", type=float, default=4.0, help="faces on a plate flatter than this share its material")
    ap.add_argument("--speck-faces", type=int, default=4, help="largest island (faces) the despeckle pass may recolor")
    ap.add_argument("--speck-fold", type=float, default=75.0, help="despeckle only islands joined by a fold gentler than this")
    ap.add_argument("--tex", type=int, default=2048, help="working texture resolution")
    ap.add_argument("--lw", type=float, default=0.7, help="weight of lightness vs hue in clustering")
    args = ap.parse_args()
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)

    pos, uv, faces, img = load_glb(args.glb)
    log(f"mesh: {len(pos)} verts, {len(faces)} faces, texture {img.size} {img.mode}")
    img.draft("RGB", (args.tex, args.tex))  # JPEG: decode at reduced size, never full 8K
    img = img.convert("RGB")
    tex = np.asarray(img.resize((args.tex, args.tex), Image.LANCZOS), np.float32) / 255.0
    del img
    TH, TW = tex.shape[:2]

    # 1. weld
    diag = np.linalg.norm(pos.max(0) - pos.min(0))
    _, weld = np.unique(np.round(pos / (diag * 1e-6)).astype(np.int64), axis=0, return_inverse=True)
    weld = weld.ravel()
    wf = weld[faces]
    log(f"welded: {weld.max() + 1} unique positions (from {len(pos)})")

    tri = pos[faces]
    cr = np.cross(tri[:, 1] - tri[:, 0], tri[:, 2] - tri[:, 0])
    area = np.linalg.norm(cr, axis=1) / 2
    normal = cr / np.maximum(2 * area, 1e-20)[:, None]

    # 2. face adjacency via shared welded edges
    F = len(faces)
    e = np.stack([wf[:, [0, 1]], wf[:, [1, 2]], wf[:, [2, 0]]], 1).reshape(-1, 2)
    e.sort(1)
    key = e[:, 0] * (weld.max() + 1) + e[:, 1]
    owner = np.repeat(np.arange(F), 3)
    order = np.argsort(key, kind="stable")
    ks, os_ = key[order], owner[order]
    same = ks[1:] == ks[:-1]
    fa, fb = os_[:-1][same], os_[1:][same]
    keep = fa != fb
    fa, fb = fa[keep], fb[keep]
    dots = np.clip((normal[fa] * normal[fb]).sum(1), -1, 1)
    ang = np.degrees(np.arccos(dots))
    log(f"adjacent face pairs: {len(fa)}, near-opposite normals (possible flips/fins): {(dots < -0.9).sum()}")
    smooth = ang < args.angle
    adj = sparse.coo_matrix((np.ones(smooth.sum()), (fa[smooth], fb[smooth])), shape=(F, F)).tocsr()
    adj = adj + adj.T
    n_reg, region = connected_components(adj, directed=False)
    reg_area = np.bincount(region, area, n_reg)
    log(f"regions at {args.angle} deg: {n_reg}; "
        f"largest 10 cover {np.sort(reg_area)[::-1][:10].sum() / area.sum():.0%} of area; "
        f"single-face regions: {(np.bincount(region) == 1).sum()}")

    # 3. per-face color: 7 barycentric samples, median in Lab
    bw = np.array([[1 / 3, 1 / 3, 1 / 3], [.6, .2, .2], [.2, .6, .2], [.2, .2, .6],
                   [.45, .45, .1], [.1, .45, .45], [.45, .1, .45]])
    fuv = uv[faces]                                        # (F,3,2)
    suv = np.einsum("sk,fkd->fsd", bw, fuv)                # (F,S,2)
    px = (np.mod(suv[..., 0], 1) * TW).astype(int).clip(0, TW - 1)
    py = (np.mod(suv[..., 1], 1) * TH).astype(int).clip(0, TH - 1)
    srgb = tex[py, px]                                     # (F,S,3)
    lab = srgb_to_lab(srgb)
    face_lab = np.median(lab, 1)
    face_rgb = np.median(srgb, 1)

    scale = np.array([args.lw, 1, 1], np.float32)
    w = area / area.sum() + 1e-9
    centers, face_cl = kmeans(face_lab * scale, w, args.k)
    # merge clusters that are perceptually the same (delta-E below --merge)
    while len(centers) > 2:
        d = np.sqrt(((centers[:, None] - centers[None]) ** 2).sum(-1))
        np.fill_diagonal(d, np.inf)
        i, j = np.unravel_index(d.argmin(), d.shape)
        if d[i, j] >= args.merge:
            break
        face_cl[face_cl == j] = i
        face_cl[face_cl > j] -= 1
        centers = np.delete(centers, j, 0)
        centers[i] = np.average(face_lab[face_cl == i] * scale, 0, w[face_cl == i])
    log(f"color clusters: {args.k} -> {len(centers)} after merging (dE < {args.merge})")
    args.k = len(centers)
    # order clusters by area (cluster 0 = most common)
    cl_area = np.bincount(face_cl, area, args.k)
    rank = np.argsort(-cl_area)
    remap = np.empty(args.k, int)
    remap[rank] = np.arange(args.k)
    face_cl = remap[face_cl]
    centers = centers[rank]
    cl_area = cl_area[rank]

    # 4. snap regions
    k = args.k
    hist = np.zeros((n_reg, k))
    np.add.at(hist, (region, face_cl), area)
    maj = hist.argmax(1)
    purity = hist.max(1) / np.maximum(hist.sum(1), 1e-20)
    pure = purity >= args.purity
    snapped = np.where(pure[region], maj[region], face_cl)
    # smooth leftovers inside impure regions with area-weighted neighbour vote
    A = (adj + sparse.identity(F, format="csr")).multiply(area[None, :]).tocsr()
    for _ in range(2):
        onehot = sparse.csr_matrix((np.ones(F), (np.arange(F), snapped)), shape=(F, k))
        vote = np.asarray((A @ onehot).todense()).argmax(1)
        snapped = np.where(pure[region], snapped, vote)
    # A flat plate is one material: faces joined by near-zero folds take the
    # plate's majority, so a stray triangle (texture bleed, baked lighting)
    # inside a plate follows it even when its wider region was too mixed to snap.
    flat = ang < args.flat_angle
    fadj = sparse.coo_matrix((np.ones(flat.sum()), (fa[flat], fb[flat])), shape=(F, F)).tocsr()
    n_flat, plate = connected_components(fadj + fadj.T, directed=False)
    phist = np.zeros((n_flat, k))
    np.add.at(phist, (plate, snapped), area)
    pshare = phist.max(1) / np.maximum(phist.sum(1), 1e-20)
    psize = np.bincount(plate, minlength=n_flat)
    follow = (pshare[plate] >= 0.5) & (psize[plate] > 1)
    flipped = follow & (phist.argmax(1)[plate] != snapped)
    snapped = np.where(follow, phist.argmax(1)[plate], snapped)
    log(f"flat plates (< {args.flat_angle} deg): {n_flat}; faces moved to their plate's material: {flipped.sum()}")

    # Despeckle: a tiny island (a stray facet the texture painted the wrong
    # color) whose every neighbour is one other material, joined to it by a
    # fold gentler than --speck-fold, takes that material. Real details (boxes,
    # greebles) meet the hull near 90 degrees and are left alone.
    despeckled = 0
    for _ in range(2):
        same = snapped[fa] == snapped[fb]
        sg = sparse.coo_matrix((np.ones(same.sum()), (fa[same], fb[same])), shape=(F, F)).tocsr()
        n_isl, isl = connected_components(sg + sg.T, directed=False)
        isl_n = np.bincount(isl, minlength=n_isl)
        cross = ~same
        a_side = np.concatenate([isl[fa[cross]], isl[fb[cross]]])
        other = np.concatenate([snapped[fb[cross]], snapped[fa[cross]]])
        folds = np.concatenate([ang[cross], ang[cross]])
        mats = np.zeros((n_isl, k), bool)
        mats[a_side, other] = True
        gentle = np.full(n_isl, 180.0)
        np.minimum.at(gentle, a_side, folds)
        target = mats.argmax(1)
        speck = (isl_n <= args.speck_faces) & (mats.sum(1) == 1) & (gentle < args.speck_fold)
        move = speck[isl]
        despeckled += int(move.sum())
        snapped = np.where(move, target[isl], snapped)
    log(f"despeckle (<= {args.speck_faces} faces, fold < {args.speck_fold} deg): {despeckled} faces joined their surroundings")
    log(f"regions snapped (purity >= {args.purity}): {pure.sum()}/{n_reg} "
        f"= {reg_area[pure].sum() / area.sum():.0%} of area; faces changed by snapping: {(snapped != face_cl).mean():.1%}")

    # 5a. JSON
    cl_rgb = np.array([np.average(face_rgb[snapped == i], 0, area[snapped == i]) if (snapped == i).any() else np.zeros(3)
                       for i in range(k)])
    snap_area = np.bincount(snapped, area, k)
    hexc = lambda c: "#%02x%02x%02x" % tuple((np.clip(c, 0, 1) * 255).round().astype(int))  # noqa: E731
    clusters = [{"id": i, "id_color": hexc(PALETTE[i % len(PALETTE)] / 255), "texture_color": hexc(cl_rgb[i]),
                 "area_pct": round(100 * snap_area[i] / area.sum(), 2), "faces": int((snapped == i).sum()),
                 "label": "", "material": ""} for i in range(k)]
    (out / "clusters.json").write_text(json.dumps({"source": str(args.glb), "params": vars(args),
                                                   "clusters": clusters}, indent=2))
    reg_cl = np.array([np.bincount(snapped[region == r], area[region == r], k).argmax() for r in range(n_reg)])
    regions = [{"id": int(r), "faces": int((region == r).sum()), "area_pct": round(100 * reg_area[r] / area.sum(), 3),
                "cluster": int(reg_cl[r]), "purity": round(float(purity[r]), 2)}
               for r in np.argsort(-reg_area)]
    (out / "regions.json").write_text(json.dumps(regions, indent=1))
    (out / "faces.json").write_text(json.dumps({"region": region.tolist(), "cluster": snapped.tolist()}))

    # 5b. ID maps in UV space
    uvpix = np.stack([np.mod(fuv[..., 0], 1) * TW, np.mod(fuv[..., 1], 1) * TH], -1)
    # faces whose UVs straddle a wrap get mangled by mod; use raw uv instead when in [0,1]
    inside01 = (fuv.min((1, 2)) >= 0) & (fuv.max((1, 2)) <= 1)
    uvpix[inside01] = fuv[inside01] * [TW, TH]
    ufid, _ = rasterize(uvpix, TW, TH)
    covered = ufid >= 0
    log(f"UV coverage: {covered.mean():.0%} of texture")
    pal = PALETTE[np.arange(k) % len(PALETTE)]
    id_face = np.zeros((TH, TW, 3), np.uint8)
    id_face[covered] = pal[snapped[ufid[covered]]]
    Image.fromarray(dilate_ids(id_face, covered, 4)).save(out / "idmap_faces.png")
    raw_cl = np.zeros((TH, TW, 3), np.uint8)
    raw_cl[covered] = pal[face_cl[ufid[covered]]]
    Image.fromarray(dilate_ids(raw_cl, covered, 4)).save(out / "idmap_faces_unsnapped.png")
    # per-pixel clustering (keeps painted detail that geometry doesn't have)
    plab = srgb_to_lab(tex[covered]) * scale
    pcl = np.empty(len(plab), np.int64)
    for s in range(0, len(plab), 500_000):
        pcl[s:s + 500_000] = ((plab[s:s + 500_000, None] - centers[None].astype(np.float32)) ** 2).sum(-1).argmin(1)
    id_pix = np.zeros((TH, TW, 3), np.uint8)
    id_pix[covered] = pal[pcl]
    Image.fromarray(dilate_ids(id_pix, covered, 4)).save(out / "idmap_pixels.png")
    del plab, pcl
    log("wrote ID maps")

    # 5c. vertex-coloured GLB for viewing (per-face colours, unwelded)
    import trimesh
    rng = np.random.default_rng(1)
    reg_col = rng.integers(40, 255, (n_reg, 3), dtype=np.uint8)
    for name, cols in (("ship_clusters.glb", pal[snapped]), ("ship_regions.glb", reg_col[region])):
        m = trimesh.Trimesh(tri.reshape(-1, 3), np.arange(3 * F).reshape(-1, 3), process=False)
        m.visual.vertex_colors = np.repeat(np.c_[cols, np.full(F, 255, np.uint8)], 3, 0)
        m.export(out / name)
    log("wrote GLBs")

    # 5d. preview renders: textured | clusters | regions, several views
    center = (pos.max(0) + pos.min(0)) / 2
    radius = np.linalg.norm(pos - center, axis=1).max()
    RW, RH = 720, 460
    views = {"three_quarter": (-35, 20), "side": (90, 0), "front": (0, 0), "top": (0, 89), "bottom": (0, -89),
             "rear_three_quarter": (215, 20)}
    light = np.array([0.4, 0.8, 0.45])
    light /= np.linalg.norm(light)
    for vname, (yaw, pitch) in views.items():
        y, p = np.radians(yaw), np.radians(pitch)
        Ry = np.array([[np.cos(y), 0, np.sin(y)], [0, 1, 0], [-np.sin(y), 0, np.cos(y)]])
        Rx = np.array([[1, 0, 0], [0, np.cos(p), -np.sin(p)], [0, np.sin(p), np.cos(p)]])
        R = Rx @ Ry
        v = (pos - center) @ R.T
        lo, hi = v[:, :2].min(0), v[:, :2].max(0)
        s = 0.92 * min(RW / (hi[0] - lo[0]), RH / (hi[1] - lo[1]))
        mid = (lo + hi) / 2
        sx = RW / 2 + (v[:, 0] - mid[0]) * s
        sy = RH / 2 - (v[:, 1] - mid[1]) * s
        t2 = np.stack([sx, sy], -1)[faces]
        dz = -v[:, 2][faces]
        fid, bar = rasterize(t2, RW, RH, dz)
        hit = fid >= 0
        n_cam = normal @ R.T
        shade = 0.35 + 0.65 * np.abs(n_cam @ light)
        sh = shade[fid[hit]][:, None]
        bg = np.array([45, 45, 50], np.uint8)
        panels = []
        # textured
        puv = np.einsum("nk,nkd->nd", bar[hit], fuv[fid[hit]])
        tx = tex[(np.mod(puv[:, 1], 1) * TH).astype(int).clip(0, TH - 1),
                 (np.mod(puv[:, 0], 1) * TW).astype(int).clip(0, TW - 1)]
        for cols in (tx * 255 * (0.6 + 0.4 * sh), pal[snapped[fid[hit]]] * sh, reg_col[region[fid[hit]]] * sh):
            im = np.empty((RH, RW, 3), np.uint8)
            im[:] = bg
            im[hit] = np.clip(cols, 0, 255).astype(np.uint8)
            panels.append(im)
        sheet = Image.fromarray(np.concatenate(panels, 1))
        d = ImageDraw.Draw(sheet)
        for i, t in enumerate(("texture", "material clusters (snapped)", f"sharp-edge regions ({n_reg})")):
            d.text((i * RW + 10, 8), t, fill=(255, 255, 255))
        # legend
        lg = Image.new("RGB", (3 * RW, 34), (30, 30, 34))
        ld = ImageDraw.Draw(lg)
        for i in range(k):
            x = 10 + i * (3 * RW - 20) // k
            ld.rectangle([x, 8, x + 18, 26], fill=tuple(int(c) for c in pal[i]))
            ld.rectangle([x + 22, 8, x + 40, 26], fill=tuple(int(c * 255) for c in cl_rgb[i]))
            ld.text((x + 46, 12), f"{i}: {clusters[i]['area_pct']:.1f}%", fill=(230, 230, 230))
        full = Image.new("RGB", (3 * RW, RH + 34))
        full.paste(sheet, (0, 0))
        full.paste(lg, (0, RH))
        full.save(out / f"render_{vname}.png")
    log("wrote renders -> done")


if __name__ == "__main__":
    main()
