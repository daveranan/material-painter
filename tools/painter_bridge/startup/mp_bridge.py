"""Material Painter bridge for Substance 3D Painter.

Renders Substance assets (.sbsar materials and alphas) to plain image maps
for Material Painter's library, using Painter's own engine under the user's
license. Loaded at startup when Painter runs with
SUBSTANCE_PAINTER_PLUGINS_PATH pointing at tools/painter_bridge and
MP_BRIDGE_JOBS set to a job folder (tools/substance_render.py does both).

Protocol: the client writes <jobs>/<name>.job.json; the bridge renders it and
writes <jobs>/<name>.done.json ({"ok": bool, "files": [...], "error": str}).
<jobs>/bridge.ready appears once the bridge is listening.
"""
import json
import os
import traceback

import substance_painter as sp
from PySide6 import QtCore

_JOBS = os.environ.get("MP_BRIDGE_JOBS", "")
_PLANE = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "plane.obj")
_timer = None


def _log(msg):
    with open(os.path.join(_JOBS, "bridge.log"), "a", encoding="utf-8") as f:
        f.write(msg + "\n")


def _ensure_project():
    if sp.project.is_open():
        return
    settings = sp.project.Settings(default_texture_resolution=2048,
                                   normal_map_format=sp.project.NormalMapFormat.OpenGL)
    sp.project.create(mesh_file_path=_PLANE, settings=settings)


def _channels(name, src_type, src, kind, bits):
    if kind == "rgb":
        chans = [{"destChannel": c, "srcChannel": c, "srcMapType": src_type, "srcMapName": src, "srcPath": ""} for c in "RGB"]
    else:  # one channel, taken from R of the source (grayscale maps carry it everywhere)
        chans = [{"destChannel": "L", "srcChannel": "R", "srcMapType": src_type, "srcMapName": src, "srcPath": ""}]
    return {"fileName": name, "channels": chans, "parameters": {"fileFormat": "png", "bitDepth": str(bits), "dithering": False}}


def _export(out_dir, size_log2, maps, texture_set):
    os.makedirs(out_dir, exist_ok=True)
    preset = {"name": "mp_library", "maps": maps}
    config = {
        "exportPath": out_dir,
        "exportShaderParams": False,
        "defaultExportPreset": preset["name"],
        "exportPresets": [preset],
        "exportList": [{"rootPath": texture_set}],
        "exportParameters": [{"parameters": {"paddingAlgorithm": "infinite", "sizeLog2": size_log2}}],
    }
    result = sp.export.export_project_textures(config)
    if result.status != sp.export.ExportStatus.Success:
        raise RuntimeError(f"export failed: {result.status} {getattr(result, 'message', '')}")
    return [p for paths in result.textures.values() for p in paths]


def _run(job):
    _ensure_project()
    ts = sp.textureset.all_texture_sets()[0]
    stack = ts.get_stack()
    CT = sp.textureset.ChannelType
    if CT.Height not in stack.all_channels():
        stack.add_channel(CT.Height, sp.textureset.ChannelFormat.L16F)
    for node in sp.layerstack.get_root_layer_nodes(stack):
        sp.layerstack.delete_node(node)
    fill = sp.layerstack.insert_fill(sp.layerstack.InsertPosition.from_textureset_stack(stack))
    size = int(job.get("size_log2", 11))
    if job["kind"] == "material":
        res = sp.resource.import_session_resource(job["sbsar"], sp.resource.Usage.BASE_MATERIAL)
        fill.active_channels = set(stack.all_channels().keys())
        src = fill.set_material_source(res.identifier())
        if job.get("params"):
            src.set_parameters(job["params"])
        maps = [
            _channels("basecolor", "documentMap", "basecolor", "rgb", 8),
            _channels("roughness", "documentMap", "roughness", "gray", 8),
            _channels("metallic", "documentMap", "metallic", "gray", 8),
            _channels("height", "documentMap", "height", "gray", 16),
            _channels("normal", "virtualMap", "Normal_OpenGL", "rgb", 8),
        ]
    elif job["kind"] == "alpha":
        res = sp.resource.import_session_resource(job["sbsar"], sp.resource.Usage.ALPHA)
        fill.active_channels = {CT.Height}
        fill.set_source(CT.Height, res.identifier())
        maps = [_channels(job.get("name", "alpha"), "documentMap", "height", "gray", 16)]
    else:
        raise ValueError(f"unknown job kind {job['kind']!r}")
    return _export(job["out"], size, maps, ts.name)


def _poll():
    """One job per tick, so Painter stays responsive between jobs."""
    if not sp.project.is_open() and sp.project.is_busy():
        return
    for name in sorted(os.listdir(_JOBS)):
        if not name.endswith(".job.json"):
            continue
        path = os.path.join(_JOBS, name)
        done = path[: -len(".job.json")] + ".done.json"
        try:
            with open(path, encoding="utf-8") as f:
                job = json.load(f)
            files = _run(job)
            reply = {"ok": True, "files": files}
        except Exception as e:  # report every failure to the client
            reply = {"ok": False, "error": f"{e}", "trace": traceback.format_exc()}
            _log(f"{name}: {e}")
        with open(done, "w", encoding="utf-8") as f:
            json.dump(reply, f)
        os.remove(path)
        return


def start_plugin():
    global _timer
    if not _JOBS:
        return
    os.makedirs(_JOBS, exist_ok=True)
    _timer = QtCore.QTimer()
    _timer.setInterval(200)
    _timer.timeout.connect(_poll)
    _timer.start()
    with open(os.path.join(_JOBS, "bridge.ready"), "w", encoding="utf-8") as f:
        f.write(str(os.getpid()))
    _log("bridge started")


def close_plugin():
    global _timer
    if _timer is not None:
        _timer.stop()
        _timer = None
    if _JOBS and os.path.exists(os.path.join(_JOBS, "bridge.ready")):
        os.remove(os.path.join(_JOBS, "bridge.ready"))
