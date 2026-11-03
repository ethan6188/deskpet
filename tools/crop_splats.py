"""Keep only the splats of the subject, and move them into deskpet's axes.

SHARP writes a whole scene (subject plus background) in OpenCV camera axes
(x right, y down, z forward), with the photo's intrinsics in the same PLY.
This keeps splats whose center projects inside the subject mask and are not
far behind the subject's front, then flips to y up, z towards the camera,
centers the result on the ground under the subject, and scales it to height 1.

    python tools/crop_splats.py scene.ply mask.png out.ply [max_splats]

With max_splats, the faintest splats (lowest opacity) are dropped first.

Run it with SHARP's Python (it needs numpy, plyfile and pillow).
"""

import sys

import numpy as np
from PIL import Image
from plyfile import PlyData, PlyElement

src, mask_path, dst = sys.argv[1:4]
limit = int(sys.argv[4]) if len(sys.argv) > 4 else None
ply = PlyData.read(src)
v = ply["vertex"].data
k = np.array(ply["intrinsic"].data.tolist(), dtype=np.float64).reshape(-1)[:9].reshape(3, 3)
w, h = (int(x) for x in np.array(ply["image_size"].data.tolist()).reshape(-1)[:2])

mask = np.asarray(Image.open(mask_path).convert("L").resize((w, h))) > 127
x, y, z = (np.asarray(v[a], dtype=np.float64) for a in ("x", "y", "z"))
u = np.round(k[0, 0] * x / z + k[0, 2]).astype(int)
r = np.round(k[1, 1] * y / z + k[1, 2]).astype(int)
inside = (z > 0) & (u >= 0) & (u < w) & (r >= 0) & (r < h)
keep = np.zeros(len(v), bool)
keep[inside] = mask[r[inside], u[inside]]

# Splats behind the subject that project onto it are background seen through
# fur gaps: drop anything deeper than the subject's own depth range.
zs = z[keep]
near, far = np.percentile(zs, 2), np.percentile(zs, 60)
keep &= z < far + (far - near) * 1.5
# Drop bluish splats: sky and snow showing through, never fox fur.
dc = np.stack([np.asarray(v[f"f_dc_{i}"]) for i in range(3)], 1) * 0.2820948 + 0.5
keep &= ~((dc[:, 2] > dc[:, 0] + 0.05) & (dc[:, 2] > dc[:, 1]))
if limit and keep.sum() > limit:
    idx = np.flatnonzero(keep)
    strongest = idx[np.argsort(-np.asarray(v["opacity"])[idx])[:limit]]
    keep[:] = False
    keep[strongest] = True
print(f"kept {keep.sum()} of {len(v)} splats")

out = v[keep].copy()
# OpenCV -> y up, z towards the camera: rotate 180 degrees about X.
out["y"] = -out["y"]
out["z"] = -out["z"]
q = np.stack([out[f"rot_{i}"] for i in range(4)], 1)  # w, x, y, z
# q' = (0, 1, 0, 0) * q  (180 degrees about X)
w0, x0, y0, z0 = q.T
for i, c in enumerate((-x0, w0, -z0, y0)):
    out[f"rot_{i}"] = c

px, py, pz = (np.asarray(out[a], dtype=np.float64) for a in ("x", "y", "z"))
lo, hi = np.percentile(py, 0.5), np.percentile(py, 99.5)
scale = 1.0 / (hi - lo)
cx, cz = np.median(px), np.median(pz)
out["x"] = (px - cx) * scale
out["y"] = (py - lo) * scale
out["z"] = (pz - cz) * scale
for i in range(3):
    out[f"scale_{i}"] = out[f"scale_{i}"] + np.log(scale)

PlyData([PlyElement.describe(out, "vertex")], text=False).write(dst)
print(f"{dst}: height 1, feet at y = 0, centered on x and z")
