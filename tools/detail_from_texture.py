"""Extract fine surface detail (panel seams, grooves, small hatches) from a
source base-color texture (e.g. Tripo's) as a UV-space mask, so it can drive
height and roughness in Material Painter, aligned with the model's own design.

Works on a high-pass of the luminance: baked lighting and large color areas
are removed (they're low frequency), thin dark lines remain.

    python tools/detail_from_texture.py texture.png out_dir [--res 4096] [--sigma 3] [--strength 4] [--width 3] [--soften 1]

Writes seams.png (dark lines -> white mask) and highlights.png (thin bright
lines, e.g. raised edges), both 8-bit, same UV layout as the texture.
"""
import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import memguard  # noqa: E402

memguard.cap(4.0)
import numpy as np  # noqa: E402
from PIL import Image  # noqa: E402
from scipy import ndimage  # noqa: E402

Image.MAX_IMAGE_PIXELS = None
ap = argparse.ArgumentParser()
ap.add_argument("texture")
ap.add_argument("out")
ap.add_argument("--res", type=int, default=4096)
ap.add_argument("--sigma", type=float, default=3.0, help="blur radius (px at --res) separating lines from areas")
ap.add_argument("--strength", type=float, default=4.0, help="contrast of the line mask")
ap.add_argument("--mesh", help="the model (.glb): ignore lines within --border px of UV island edges (padding, not design)")
ap.add_argument("--border", type=int, default=4)
ap.add_argument("--width", type=int, default=3, help="line width in px at --res (grey dilation); 1-2 px lines alias")
ap.add_argument("--soften", type=float, default=1.0, help="blur sigma (px) after widening, for anti-aliased edges")
args = ap.parse_args()
out = Path(args.out)
out.mkdir(parents=True, exist_ok=True)

img = Image.open(args.texture)
img.draft("RGB", (args.res, args.res))
lum = np.asarray(img.convert("L").resize((args.res, args.res), Image.LANCZOS), np.float32) / 255.0
del img
# Local contrast: subtract the blurred image, normalise by local brightness
# so lines read the same in dark and light paint.
blur = ndimage.gaussian_filter(lum, args.sigma)
hp = (lum - blur) / np.maximum(blur, 0.08)
# Ignore UV padding: texels where the texture is flat over a wide area.
active = ndimage.gaussian_filter(np.abs(hp), 8) > 0.004
seams = np.clip(-hp * args.strength, 0, 1) * active
highs = np.clip(hp * args.strength, 0, 1) * active
if args.mesh:
    import make_idmap as m
    _, uv, faces, _ = m.load_glb(args.mesh)
    fid, _ = m.rasterize(uv[faces] * args.res, args.res, args.res)
    inner = ndimage.binary_erosion(fid >= 0, iterations=args.border)
    seams, highs = seams * inner, highs * inner
    del fid
# Soft, wide lines: a gentle ramp instead of a hard threshold, widened to
# --width px and blurred, so they survive mipmapping and don't shimmer.
def soften(x, lo):
    x = np.clip((x - lo) / 0.6, 0, 1)
    x = x * x * (3 - 2 * x)  # smoothstep
    if args.width > 1:
        x = ndimage.grey_dilation(x, size=(args.width, args.width))
    return np.clip(ndimage.gaussian_filter(x, args.soften), 0, 1) if args.soften > 0 else x


seams, highs = soften(seams, 0.15), soften(highs, 0.2)
Image.fromarray((seams * 255).astype(np.uint8)).save(out / "seams.png")
Image.fromarray((highs * 255).astype(np.uint8)).save(out / "highlights.png")
print(f"seams: {float((seams > 0.5).mean()):.2%} of texels, highlights: {float((highs > 0.5).mean()):.2%}")
