"""Renders HAL's eye on black in a glossy 70s style: polished chrome bezel, dark rings, black glass disc, dark red lens
rim and crisp reflections. godot/hal.gdshader adds the red light; LENS_* must match the shader's lens_center and
lens_radius. Drawn at twice the size and scaled down, so edges stay sharp but smooth.

uvx --with numpy --with pillow python scripts/make-hal-panel.py godot/hal_panel.png
"""

import sys

import numpy as np
from PIL import Image, ImageDraw, ImageFilter

K = 2
W, H = 720 * K, 1280 * K
CX, CY = 360 * K, 600 * K
BEZEL_OUT, BEZEL_IN, BAND_IN, RING_IN, DISC = (v * K for v in (300, 262, 248, 236, 224))
LENS_RIM, LENS_R = 98 * K, 92 * K
rng = np.random.default_rng(9000)
y, x = np.mgrid[0:H, 0:W].astype(np.float32)
dx, dy = x - CX, y - CY
r = np.hypot(dx, dy)
ux, uy = dx / np.maximum(r, 1), dy / np.maximum(r, 1)
angle = np.degrees(np.arctan2(dy, dx)) % 360


def smooth(e0, e1, v):
    t = np.clip((v - e0) / (e1 - e0), 0.0, 1.0)
    return t * t * (3 - 2 * t)


def annulus(r0, r1, soft=1.5):
    return smooth(r0 - soft, r0 + soft, r) * (1 - smooth(r1 - soft, r1 + soft, r))


def put(img, mask, colour):
    return img * (1 - mask[..., None]) + mask[..., None] * colour


def layer(draw_fn):
    """A white shape drawn with PIL as a 0..1 mask."""
    im = Image.new("L", (W, H), 0)
    draw_fn(ImageDraw.Draw(im))
    return np.asarray(im).astype(np.float32) / 255


SKY, FLOOR = np.array([0.92, 0.96, 1.04]), np.array([1.0, 0.95, 0.88])

img = np.zeros((H, W, 3), np.float32)

# Polished chrome: crisp concentric bands that run all the way round, lit softly from the top left, plus window glints.
t = np.clip((r - BEZEL_IN) / (BEZEL_OUT - BEZEL_IN), 0, 1)
profile = np.select([t < 0.10, t < 0.38, t < 0.46, t < 0.74, t < 0.84], [0.28, 0.58, 0.12, 0.45, 1.0], 0.55)
light = 0.45 + 0.55 * (0.5 - 0.5 * (ux * 0.5 + uy * 0.85))
env = profile * light
windows = ((angle > 205) & (angle < 228) | (angle > 236) & (angle < 250) | (angle > 300) & (angle < 312)) & (t > 0.46)
env = np.where(windows, np.maximum(env, 1.1), env)
glint = (angle > 40) & (angle < 52) & (t > 0.46) & (t < 0.84)
env = np.where(glint, np.maximum(env, 0.9), env)
tint = smooth(-0.8, 0.8, -uy)[..., None]
chrome = env[..., None] * (tint * SKY + (1 - tint) * FLOOR)
img = put(img, annulus(BEZEL_IN, BEZEL_OUT), np.clip(chrome, 0, 1.2))

band = 0.07 + 0.12 * smooth(0.6, -0.9, uy) * (1 - (r - BAND_IN) / (BEZEL_IN - BAND_IN))
img = put(img, annulus(BAND_IN, BEZEL_IN), np.stack([band] * 3, -1))
img = put(img, annulus(RING_IN, BAND_IN), np.array([0.035, 0.035, 0.037]))
img = put(img, annulus(DISC, RING_IN), np.array([0.0, 0.0, 0.0]))
depth = 0.02 + 0.03 * (1 - r / DISC)
img = put(img, 1 - smooth(DISC - 1.5, DISC + 1.5, r), np.stack([depth, depth * 0.1, depth * 0.12], -1))

# Dark red metal rim around the lens, lit from the lower right.
rim_light = np.clip(0.5 + 0.5 * (ux * 0.6 + uy * 0.8), 0, 1) ** 3
rim = np.stack([0.12 + 0.45 * rim_light, 0.03 + 0.22 * rim_light, 0.03 + 0.2 * rim_light], -1)
img = put(img, annulus(LENS_R, LENS_RIM, 1.0), rim)
img = put(img, 1 - smooth(LENS_R - 1, LENS_R + 1, r), np.array([0.05, 0.0, 0.0]))


# Crisp reflections on the glass, added on top: a two-pane window, edge crescents, glints on the lens.
def window(d):
    for x0, x1 in ((-150, -62), (-52, 30)):
        top, bottom = -192 + abs(x0 + 60) * 0.25, -112 + abs(x0 + 60) * 0.12
        d.polygon([(CX + x0 * K, CY + top * K), (CX + x1 * K, CY + (top - 10) * K),
                   (CX + (x1 - 6) * K, CY + (bottom - 6) * K), (CX + (x0 + 4) * K, CY + bottom * K)], fill=255)


pane = layer(window) * (1 - smooth(CY - 190 * K, CY - 100 * K, y) * 0.6)
crescent = layer(lambda d: d.arc((CX - 214 * K, CY - 214 * K, CX + 214 * K, CY + 214 * K), 195, 262, fill=255, width=5 * K))
crescent_low = layer(lambda d: d.arc((CX - 210 * K, CY - 210 * K, CX + 210 * K, CY + 210 * K), 20, 70, fill=255, width=3 * K))


def lens_glints(d):
    d.polygon([(CX - 44 * K, CY - 74 * K), (CX - 6 * K, CY - 82 * K), (CX - 8 * K, CY - 62 * K), (CX - 40 * K, CY - 56 * K)], fill=255)
    d.polygon([(CX + 2 * K, CY - 83 * K), (CX + 30 * K, CY - 78 * K), (CX + 26 * K, CY - 62 * K), (CX, CY - 63 * K)], fill=255)
    d.ellipse((CX + 50 * K, CY + 44 * K, CX + 58 * K, CY + 52 * K), fill=255)


glints = layer(lens_glints)
chrome_flash = layer(lambda d: d.polygon([(CX - 236 * K, CY - 168 * K), (CX - 224 * K, CY - 182 * K),
                                         (CX - 186 * K, CY - 214 * K), (CX - 196 * K, CY - 198 * K)], fill=255))
inside = (r < DISC).astype(np.float32)
img += ((pane * 0.2 + crescent * 0.45 + crescent_low * 0.15) * inside + glints * 0.75)[..., None]
img += (chrome_flash * annulus(BEZEL_IN, BEZEL_OUT))[..., None] * 0.8

out = Image.fromarray((np.clip(img, 0, 1) * 255).astype(np.uint8)).resize((W // K, H // K), Image.LANCZOS)
grain = np.asarray(out).astype(np.float32) / 255 + rng.normal(0, 0.006, (H // K, W // K, 1))
Image.fromarray((np.clip(grain, 0, 1) * 255).astype(np.uint8)).save(sys.argv[1])
