"""HD2-style cliff kit in Material Painter: many rock meshes, one shared set of surfaces.

Each rock keeps its own bake (AO, curvature, sky exposure) and its own masks
(the "local R/G controls"); every rock samples the same three tri-planar surfaces
(fresh rock, fractured rock, deposit) at one physical scale. Swapping a surface
set re-skins every rock without repainting anything.

    python tools/cliff_kit.py build     # load + bake + build the stack, save each rock's project
    python tools/cliff_kit.py render    # renders: geometry, weights, every surface set, contact sheet
    python tools/cliff_kit.py apply <rock> <set>   # leave one rock open in the app with a set applied

The app must be running (mp launch). It replaces whatever document is open.
"""
import json, os, subprocess, sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
MP = REPO / "target/debug/mp.exe"
# Meshes, projects, renders and surface textures live outside the repo: MP_WORKSPACE (default: the
# folder the repo is in), with out/cliffs/meshes_v4/<rock>.obj and library/surfaces/rock/<surface>/.
WORK = Path(os.environ.get("MP_WORKSPACE", REPO.parent))
OUT = WORK / "out/cliffs"
SURF = WORK / "library/surfaces/rock"

# Longest side in metres decides the tri-planar tiling (MP fits each mesh in a 2-unit box).
ROCKS = {
    "rock_sandstone":   14.0,
    "rock_formation_a": 10.0,
    "rock_formation_b": 16.0,
}

def physical_size(folder):
    """Quixel's physical size (metres) of a surface; non-square ones use the geometric mean."""
    for j in (SURF / folder).rglob("*.json"):
        try:
            maps = json.loads(j.read_text(encoding="utf-8")).get("maps", [])
        except (json.JSONDecodeError, UnicodeDecodeError):
            continue
        for m in maps:
            if m.get("physicalSize"):
                a, b = (float(v) for v in m["physicalSize"].split("x"))
                return (a * b) ** 0.5
    return 2.0


def surface(folder, tile_m=None):
    """A surface folder (plain Quixel download, or a UE pack converted by ue_convert) at a physical tile size."""
    col = next(f for f in sorted((SURF / folder).rglob("*.jpg")) if f.stem.endswith(("_BaseColor", "_Albedo")))
    stem, kind = col.stem.rsplit("_", 1)
    return {"dir": col.parent, "stem": stem, "color": kind, "tile_m": tile_m or physical_size(folder), "name": folder}

# Base-layer candidates (try with `candidates`) and deposit candidates (shown as S2 over the base).
BASE_CANDIDATES = [
    "granite_rock_wgpqfg3_2k", "_cand/south_african_slate_quarry_low/xb5gfgc",
    "layered_rock_cliff_ulgldj3dy_2k", "rock_cliff_xccibbi_2k_ue_mid", "layered_rock_cliff_ubulehofw_2k_ue_mid",
    "desert_western_cliff_surface_layered_01_vetgagfdy_2k_ue_mid", "layered_rock_cliff_vl4jdiwg_2k_ue_mid",
    "rock_cliff_tkrkadal_2k_ue_mid", "layered_rock_cliff_tjtmcg3g_2k_ue_mid", "icelandic_rock_cliff_ucdmcawdy_2k_ue_mid",
]
DEPOSIT_CANDIDATES = [
    "mossy_rocky_ground_vcrkeeb_2k", "icelandic_moss_smkmagnb_2k_ue_mid", "icelandic_moss_smkjdacb_2k",
    "snow_covered_ground_sinjagua_2k", "fresh_windswept_snow_uhrkaisdy_2k",
    "sand_sgfodj1a_2k", "rippled_sand_vd3lecfs_2k", "red_desert_sand_sj0jahwa_2k",
]

SLATE = "_cand/south_african_slate_quarry_low/"

# Surface sets: [S0 fresh rock, S1 fractured rock, S2 deposit]. Tiles are physical repeat sizes.
# S0 and S1 are tone-matched (mean albedo ~0.48): like HD2's shared layers they differ in grain and
# normals, not in brightness, so mask boundaries don't read as blotches.
SETS = {
    "temperate": [surface("granite_rock_wgpqfg3_2k", 3.0), surface(SLATE + "xb5gfgc", 2.5),
                  surface("mossy_rocky_ground_vcrkeeb_2k", 1.5)],
    "arctic":    [surface("granite_rock_wgpqfg3_2k", 3.0), surface(SLATE + "xb5gfgc", 2.5),
                  surface("snow_covered_ground_sinjagua_2k", 6.0)],
    "arid":      [surface(SLATE + "xbkibje", 3.0), surface("granite_rock_wgpqfg3_2k", 2.5),
                  surface("sand_sgfodj1a_2k", 2.0)],
}
# The cliff look (rock_cliff scan): S0 the blocky cliff face, S1 a layered cliff in horizontal
# strata bands (tools/strata_mask.py, placed by height in metres), S2 deposit on tops only.
SETS["cliff"] = [surface("rock_cliff_xccibbi_2k_ue_mid", 3.5), surface("layered_rock_cliff_tjtmcg3g_2k_ue_mid", 3.0),
                 surface("mossy_rocky_ground_vcrkeeb_2k", 1.5)]
BUILD_SET = "cliff"
MESHES = OUT / "meshes_v4"  # the artist's own UVs (68-74% used), normals split at 30-degree edges
SHARPNESS = 12.0  # tri-planar: crisp side planes; soft blends smear the strata on steep faces
STRATA_SET = {"cliff"}  # sets whose S1 is masked by strata instead of the convexity generator
REF_M = 14.0  # mask noise "scale" values below are for a rock this long; others scale so blobs stay the same size in metres

# Local controls, identical recipe on every rock (the per-rock result comes from each rock's bake).
LAYERS = [
    {"name": "S0 fresh rock", "mask": None},
    {"name": "S1 fractured rock (blend01)",
     "mask": {"generator": "Edge Wear", "weights": {"Edges": 0.0, "Convexity": 1.0, "Occlusion": 0.0, "Noise": 0.55},
              "level": 0.55, "contrast": 0.06, "scale": 2.5, "noise": 0.5, "seed": 3.0}, "opacity": 0.9},
    {"name": "S2 deposit (blend2)",
     # Up-facing faces only: on ~45 degree slopes tri-planar mixes two projections and the deposit smears.
     "mask": {"generator": "Dust", "weights": {"Sky exposure": 0.25, "Facing up": 1.0, "Noise": 0.45},
              "level": 0.78, "contrast": 0.1, "scale": 9.0, "noise": 0.55, "seed": 7.0}},
]
GRIME = {"name": "Cavity grime (intrinsic)", "opacity": 0.4,
         "mask": {"generator": "Cavity Grime", "level": 0.55, "contrast": 0.2}}
# Biome grade: a Multiply layer over the whole stack (HD2 keeps a separate grading group per look).
GRADE = {"name": "Grade (biome)", "colors": {"temperate": [0.74, 0.73, 0.70], "arctic": [0.80, 0.82, 0.86],
                                             "arid": [0.84, 0.76, 0.66], "cliff": [1.0, 0.97, 0.93]}}
GRADE_IDX = len(LAYERS) + 1
DEBUG_COLORS = [[0.05, 0.12, 0.85], [0.85, 0.06, 0.05], [0.08, 0.8, 0.1]]  # S0 blue, S1 red, S2 green


# Ragged deposit edges: instead of stopping on the mesh edge in a ruler line, the deposit spills over
# each lip onto the top of the face below, in noisy fingers (sky-exposed faces near a convex edge).
# Added to the S2 mask as a mask layer. ("subtract" with an edge-wear generator erodes the lips instead.)
LIP_EDGE = {"kind": "Dust", "weights": {"Sky exposure": 0.35, "Facing up": 0.5, "Edges": 1.0, "Noise": 0.5},
            "level": 0.74, "contrast": 0.1, "scale": 9.0, "noise": 0.6, "seed": 11.0, "opacity": 0.9,
            "blend": "add"}


DEPOSIT_FROM_GEOMETRY = True  # S2 mask from tools/deposit_mask.py instead of the Dust generator + LIP_EDGE


def add_lip_erosion(doc, rock):
    """Gives the S2 deposit layer's mask a noisy layer that grows it over the lips (see LIP_EDGE)."""
    for L in doc["texture_sets"][0]["layers_bottom_to_top"]:
        if L["name"] != LAYERS[2]["name"] or not L.get("mask") or not L["mask"].get("generator"):
            continue
        g = dict(L["mask"]["generator"])
        e = dict(LIP_EDGE)
        g.update(kind=e["kind"], weights=e["weights"], level=e["level"], contrast=e["contrast"],
                 scale=e["scale"] * ROCKS[rock] / REF_M, noise=e["noise"], seed=e["seed"], invert=False)
        L["mask"]["layers"] = [{"source": {"generator": g}, "blend": e["blend"], "opacity": e["opacity"], "enabled": True}]


def mp(tool, **args):
    r = subprocess.run([str(MP), tool, "--json", json.dumps(args)], capture_output=True, text=True)
    if r.returncode != 0:
        raise SystemExit(f"mp {tool} {args}: {r.stdout}{r.stderr}")
    try:
        return json.loads(r.stdout)
    except json.JSONDecodeError:
        return r.stdout


def render(path, mode="Material", views="three_quarter,rear_three_quarter"):
    r = subprocess.run([str(MP), "render", f"mode={mode}", "width=900", "height=640", "distance=3.4", "--views", views,
                        "--out", str(path)], capture_output=True, text=True)
    if r.returncode != 0:
        raise SystemExit(r.stdout + r.stderr)


def tiling(rock, s):
    return (ROCKS[rock] / 2.0) / s["tile_m"]


def apply_surface(rock, i, s):
    p = lambda ch: str(s["dir"] / f"{s['stem']}_{ch}.jpg").replace("\\", "/")
    mp("update_layer", index=i, images={"color": p(s["color"]), "roughness": p("Roughness"), "normal": p("Normal")},
       projection="triplanar", tiling=tiling(rock, s), triplanar_sharpness=SHARPNESS, normal_strength=1.0,
       normal_directx=False, metallic=0.0)


def apply_set(rock, set_name):
    for i, s in enumerate(SETS[set_name]):
        apply_surface(rock, i, s)
    if len(mp("get_state")["layers_bottom_to_top"]) > GRADE_IDX:
        mp("update_layer", index=GRADE_IDX, color=GRADE["colors"][set_name])


def debug_weights(on):
    """Flat diagnostic colours on the three surface layers (images removed); re-apply a set to restore."""
    for i, c in enumerate(DEBUG_COLORS):
        if on:
            mp("update_layer", index=i, images={"color": None, "roughness": None, "normal": None},
               color=c, roughness=0.7, normal=False)


def build(rock):
    mp("load_mesh", path=str(MESHES / f"{rock}.obj").replace("\\", "/"))
    mp("bake", resolution=2048, quality="High")
    while mp("get_state")["layers_bottom_to_top"]:
        mp("delete_layer", index=len(mp("get_state")["layers_bottom_to_top"]) - 1)
    for i, L in enumerate(LAYERS):
        mp("add_layer", kind="fill", preset="Stone Dark", name=L["name"])
        if L["mask"]:
            m = dict(L["mask"]); m["scale"] = m["scale"] * ROCKS[rock] / REF_M
            mp("set_mask", index=i, **m)
    mp("add_layer", kind="fill", preset="Stone Dark", name=GRIME["name"])
    g = len(LAYERS)
    mp("update_layer", index=g, opacity=GRIME["opacity"], color=[0.09, 0.075, 0.06], roughness=0.85,
       color_variation=0.0, roughness_variation=0.0)
    mp("set_mask", index=g, **GRIME["mask"])
    mp("add_layer", kind="fill", preset="Stone Dark", name=GRADE["name"])
    mp("update_layer", index=GRADE_IDX, blend="Multiply", color_variation=0.0, roughness_variation=0.0)
    for i, L in enumerate(LAYERS):
        if L.get("opacity"):
            mp("update_layer", index=i, opacity=L["opacity"])
    apply_set(rock, BUILD_SET)
    (OUT / rock).mkdir(parents=True, exist_ok=True)
    proj = OUT / rock / f"{rock}.json"
    mp("save_project", path=str(proj).replace("\\", "/"))
    # Channels the CLI can't set: no height anywhere (surfaces carry normals instead), grime = colour + roughness.
    doc = json.loads(proj.read_text())
    for L in doc["texture_sets"][0]["layers_bottom_to_top"]:
        en = L["content"]["fill"]["enabled"]
        en[2] = False; en[3] = False
        if L["name"] == GRIME["name"]:
            en[4] = False
        if L["name"] == GRADE["name"]:
            en[:] = [True, False, False, False, False, False]
        if L["name"] == LAYERS[1]["name"] and BUILD_SET in STRATA_SET:
            # S1 follows the strata texture (generated below) instead of a noise generator.
            L["mask"] = {"generator": None, "paint": {"blob": "strata/s1.png", "format": "R8Unorm", "width": 2048,
                                                      "height": 2048}, "graph": None, "filters": [], "layers": []}
    if DEPOSIT_FROM_GEOMETRY:
        # S2 from tools/deposit_mask.py: up-facing cover plus moss hanging just below each lip
        # (generators put it on every convex corner too).
        for L in doc["texture_sets"][0]["layers_bottom_to_top"]:
            if L["name"] == LAYERS[2]["name"]:
                L["mask"] = {"generator": None, "paint": {"blob": "deposit/s2.png", "format": "R8Unorm", "width": 2048,
                                                          "height": 2048}, "graph": None, "filters": [], "layers": []}
        d = OUT / rock / f"{rock}.data" / "deposit"
        d.mkdir(parents=True, exist_ok=True)
        subprocess.run([sys.executable, str(REPO / "tools/deposit_mask.py"), str(MESHES / f"{rock}.obj"),
                        str(d / "s2.png"), "--size-m", str(ROCKS[rock]), "--seed", "7"], check=True)
    else:
        add_lip_erosion(doc, rock)
    if BUILD_SET in STRATA_SET:
        d = OUT / rock / f"{rock}.data" / "strata"
        d.mkdir(parents=True, exist_ok=True)
        subprocess.run([sys.executable, str(REPO / "tools/strata_mask.py"), str(MESHES / f"{rock}.obj"),
                        str(d / "s1.png"), "--size-m", str(ROCKS[rock]), "--seed", "1"], check=True)
    proj.write_text(json.dumps(doc, indent=1))
    open_rock(rock)


def open_rock(rock):
    mp("open_project", path=str(OUT / rock / f"{rock}.json").replace("\\", "/"))


def render_all(tag):
    out = OUT / "renders" / tag
    for rock in ROCKS:
        open_rock(rock)
        n = GRADE_IDX + 1
        for i in range(n):
            mp("update_layer", index=i, visible=False)
        render(out / rock / "1_geometry")
        render(out / rock / "2_ao", mode="Ambient Occlusion")
        for i in range(n):
            mp("update_layer", index=i, visible=True)
        for i in (len(LAYERS), GRADE_IDX):
            mp("update_layer", index=i, visible=False)
        debug_weights(True)
        render(out / rock / "3_weights")
        for i in (len(LAYERS), GRADE_IDX):
            mp("update_layer", index=i, visible=True)
        for s in SETS:
            apply_set(rock, s)
            render(out / rock / f"4_{s}")
        apply_set(rock, BUILD_SET)
        mp("save_project")


def candidates(rocks=("rock_formation_a", "rock_sandstone")):
    """Each base candidate alone on S0 (other surfaces and the grade hidden), then each deposit as S2 over granite."""
    out = OUT / "renders" / "candidates"
    for rock in rocks:
        open_rock(rock)
        for i in (1, 2, GRADE_IDX):
            mp("update_layer", index=i, visible=False)
        for name in BASE_CANDIDATES:
            apply_surface(rock, 0, surface(name))
            render(out / "base" / name / rock, views="three_quarter")
        mp("update_layer", index=2, visible=True)
        apply_surface(rock, 0, SETS["temperate"][0])
        for name in DEPOSIT_CANDIDATES:
            apply_surface(rock, 2, surface(name))
            render(out / "deposit" / name / rock, views="three_quarter")
        mp("open_project", path=str(OUT / rock / f"{rock}.json").replace("\\", "/"))  # drop the trial edits


if __name__ == "__main__":
    cmd = sys.argv[1]
    if cmd == "build":
        for rock in (sys.argv[2:] or ROCKS):
            build(rock)
    elif cmd == "render":
        render_all(sys.argv[2] if len(sys.argv) > 2 else "v1")
    elif cmd == "candidates":
        candidates()
    elif cmd == "apply":
        open_rock(sys.argv[2]); apply_set(sys.argv[2], sys.argv[3])
