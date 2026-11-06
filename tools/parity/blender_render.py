"""Render a mesh + exported PBR textures in Blender EEVEE the way Material
Painter's viewport frames it, for side-by-side look parity tests.

    blender -b --factory-startup -P tools/parity/blender_render.py -- \
        --mesh model.obj --tex out/parity/tex --prefix ship --hdri studio.hdr \
        --yaw 0.6 --pitch 0.35 --dist 2.8 --target 0 0 0 --out shot.png

Conventions matched to MP:
- MP normalizes meshes: p_mp = (p - center) * scale (from the project's
  "mesh" entry), Y up. Blender is Z up: (x, y, z)_mp -> (x, -z, y).
- MP orbit camera: eye = target + dist * (cos p sin y, sin p, cos p cos y),
  vertical field of view 38 degrees.
- MP equirect lookup: u = 0.5 + atan2(x, -z) / 2pi, which is Blender's
  mapping turned 90 degrees about Z (checked with --check-env).
- Compare with MP set to tonemap "Linear (clip)", key light off, and
  Blender's "Standard" view transform: both then show linear radiance.
"""
import argparse
import math
import sys

import bpy
from mathutils import Vector

argv = sys.argv[sys.argv.index("--") + 1:] if "--" in sys.argv else []
ap = argparse.ArgumentParser()
ap.add_argument("--mesh")
ap.add_argument("--tex", help="folder with <prefix>_BaseColor/Roughness/Metallic/Normal.png")
ap.add_argument("--prefix", default="ship")
ap.add_argument("--center", type=float, nargs=3, default=[0, 0, 0], help="MP mesh center (source coords, Y up)")
ap.add_argument("--scale", type=float, default=1.0, help="MP mesh scale")
ap.add_argument("--hdri", required=True)
ap.add_argument("--env-rot", type=float, default=0.0, help="MP env_rotation, degrees")
ap.add_argument("--env-base", type=float, default=-90.0, help="Blender-to-MP equirect offset, degrees (checked with --check-env)")
ap.add_argument("--env-strength", type=float, default=1.0)
ap.add_argument("--yaw", type=float, default=0.6)
ap.add_argument("--pitch", type=float, default=0.35)
ap.add_argument("--dist", type=float, default=2.8)
ap.add_argument("--target", type=float, nargs=3, default=[0, 0, 0])
ap.add_argument("--fov", type=float, default=38.0)
ap.add_argument("--width", type=int, default=1300)
ap.add_argument("--height", type=int, default=760)
ap.add_argument("--samples", type=int, default=64)
ap.add_argument("--raytrace", action="store_true", help="EEVEE screen-space ray-traced reflections")
ap.add_argument("--engine", default="eevee", choices=["eevee", "cycles"],
                help="cycles: the exact image-based lighting integral (ground truth for a sphere)")
ap.add_argument("--sphere", nargs=3, type=float, metavar=("R", "G", "B"),
                help="instead of --mesh: a unit sphere with this sRGB base color")
ap.add_argument("--rough", type=float, default=0.5)
ap.add_argument("--metal", type=float, default=0.0)
ap.add_argument("--check-env", action="store_true", help="render the environment only (no mesh)")
ap.add_argument("--out", required=True)
a = ap.parse_args(argv)


def mp_to_b(v):
    return Vector((v[0], -v[2], v[1]))


scene = bpy.context.scene
for o in list(scene.objects):
    bpy.data.objects.remove(o, do_unlink=True)

# ---- world: MP's HDRI with MP's equirect orientation
world = bpy.data.worlds.new("mp") if not scene.world else scene.world
scene.world = world
world.use_nodes = True
nt = world.node_tree
nt.nodes.clear()
tc = nt.nodes.new("ShaderNodeTexCoord")
mp_map = nt.nodes.new("ShaderNodeMapping")
mp_map.inputs["Rotation"].default_value[2] = math.radians(a.env_base + a.env_rot)
envt = nt.nodes.new("ShaderNodeTexEnvironment")
envt.image = bpy.data.images.load(a.hdri)
bg = nt.nodes.new("ShaderNodeBackground")
bg.inputs["Strength"].default_value = a.env_strength
outw = nt.nodes.new("ShaderNodeOutputWorld")
nt.links.new(tc.outputs["Generated"], mp_map.inputs["Vector"])
nt.links.new(mp_map.outputs["Vector"], envt.inputs["Vector"])
nt.links.new(envt.outputs["Color"], bg.inputs["Color"])
nt.links.new(bg.outputs["Background"], outw.inputs["Surface"])


def image_node(nodes, path, color):
    n = nodes.new("ShaderNodeTexImage")
    n.image = bpy.data.images.load(path)
    n.image.colorspace_settings.name = "sRGB" if color else "Non-Color"
    return n


# ---- object
if not a.check_env:
    if a.sphere:
        bpy.ops.mesh.primitive_uv_sphere_add(segments=96, ring_count=48, radius=1.0)
        obj = bpy.context.active_object
    else:
        bpy.ops.wm.obj_import(filepath=a.mesh, forward_axis="NEGATIVE_Z", up_axis="Y")
        obj = bpy.context.selected_objects[0]
        bpy.context.view_layer.objects.active = obj
        bpy.ops.object.transform_apply(location=True, rotation=True, scale=True)
        c = mp_to_b(a.center)
        obj.scale = (a.scale,) * 3
        obj.location = -c * a.scale
    bpy.ops.object.shade_smooth()
    mat = bpy.data.materials.new("mp")
    mat.use_nodes = True
    nodes, links = mat.node_tree.nodes, mat.node_tree.links
    bsdf = nodes["Principled BSDF"]
    if a.sphere:
        bsdf.inputs["Base Color"].default_value = [x ** 2.2 for x in a.sphere] + [1.0]
        bsdf.inputs["Roughness"].default_value = a.rough
        bsdf.inputs["Metallic"].default_value = a.metal
    else:
        t = f"{a.tex}/{a.prefix}_"
        links.new(image_node(nodes, t + "BaseColor.png", True).outputs["Color"], bsdf.inputs["Base Color"])
        links.new(image_node(nodes, t + "Roughness.png", False).outputs["Color"], bsdf.inputs["Roughness"])
        links.new(image_node(nodes, t + "Metallic.png", False).outputs["Color"], bsdf.inputs["Metallic"])
        nm = nodes.new("ShaderNodeNormalMap")
        links.new(image_node(nodes, t + "Normal.png", False).outputs["Color"], nm.inputs["Color"])
        links.new(nm.outputs["Normal"], bsdf.inputs["Normal"])
    obj.data.materials.clear()
    obj.data.materials.append(mat)

# ---- camera: MP's orbit camera
t = Vector(a.target)
cy, sy, cp, sp = math.cos(a.yaw), math.sin(a.yaw), math.cos(a.pitch), math.sin(a.pitch)
eye = t + Vector((cp * sy, sp, cp * cy)) * a.dist
cam_data = bpy.data.cameras.new("mp")
cam_data.sensor_fit = "VERTICAL"
cam_data.angle_y = math.radians(a.fov)
cam_data.clip_start, cam_data.clip_end = 0.01, 100.0
cam = bpy.data.objects.new("mp", cam_data)
scene.collection.objects.link(cam)
cam.location = mp_to_b(eye)
cam.rotation_euler = (mp_to_b(t) - mp_to_b(eye)).to_track_quat("-Z", "Y").to_euler()
scene.camera = cam

# ---- render
if a.engine == "cycles":
    scene.render.engine = "CYCLES"
    scene.cycles.samples = a.samples
    scene.cycles.use_denoising = True
    scene.cycles.device = "CPU"
else:
    # "BLENDER_EEVEE_NEXT" in 4.2-4.5, "BLENDER_EEVEE" again from 5.0.
    engines = {e.identifier for e in bpy.types.RenderSettings.bl_rna.properties["engine"].enum_items}
    scene.render.engine = "BLENDER_EEVEE_NEXT" if "BLENDER_EEVEE_NEXT" in engines else "BLENDER_EEVEE"
    scene.eevee.taa_render_samples = a.samples
    scene.eevee.use_raytracing = a.raytrace
scene.render.resolution_x, scene.render.resolution_y = a.width, a.height
scene.render.resolution_percentage = 100
scene.render.film_transparent = not a.check_env
scene.view_settings.view_transform = "Standard"
scene.view_settings.look = "None"
scene.view_settings.exposure = 0.0
scene.view_settings.gamma = 1.0
scene.render.image_settings.file_format = "PNG"
scene.render.image_settings.color_mode = "RGBA"
scene.render.filepath = a.out
bpy.ops.render.render(write_still=True)
print("wrote", a.out)
