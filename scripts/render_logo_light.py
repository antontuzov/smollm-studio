#!/usr/bin/env python3
"""Derive the light brand mark from the same parameters as the app icon.

Writes assets/brand/logo-light.png: the eight-lobed mark in the app's
indigo/cyan palette on a white card with a hairline border, which is the
version that belongs in the README and on GitHub. The dark tile at
assets/brand/logo.png stays the app icon -- a white tile is washed out in a
dark Dock -- so this file is a companion to it, not a replacement.

Re-run after changing the palette or the shape:

    python3 scripts/render_logo_light.py
"""
import os

import numpy as np
from PIL import Image, ImageDraw, ImageFilter

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
OUT = os.path.join(ROOT, "assets", "brand", "logo-light.png")

# The canvas is drawn at 2x and downsampled once, which is what keeps the
# lobes smooth; resizing a mask any other way rings.
SIZE = 2048
FINAL = 1024

# The app icon's palette: indigo through cyan, corner to corner.
VIOLET = (139, 108, 248)
CYAN = (46, 201, 236)
CARD = (255, 255, 255)
BORDER = (229, 232, 240)

LOBES = 8
BASE_RADIUS = 0.232
LOBE_DEPTH = 0.0295
PHASE = 0.42


def silhouette(size: int) -> Image.Image:
    """The eight-lobed outline as a mask, drawn as one closed curve."""
    steps = 1440
    theta = np.linspace(0, 2 * np.pi, steps, endpoint=False)
    angle = theta - PHASE
    radius = (
        BASE_RADIUS
        + LOBE_DEPTH * np.cos(LOBES * angle)
        + 0.008 * np.cos(3 * angle + 0.7)
    ) * size
    centre = size / 2
    points = [(centre + r * np.cos(a), centre + r * np.sin(a)) for r, a in zip(radius, angle)]
    mask = Image.new("L", (size, size), 0)
    ImageDraw.Draw(mask).polygon(points, fill=255)
    return mask


def paint(mask: Image.Image) -> Image.Image:
    """Fill the silhouette with the brand gradient plus a soft top-left sheen."""
    size = mask.width
    grid_y, grid_x = np.mgrid[0:size, 0:size].astype(np.float32)
    centre = size / 2
    dx = (grid_x - centre) / size
    dy = (grid_y - centre) / size

    along = np.clip(0.5 + (dx * 0.9 - dy * 0.55) / 0.42, 0, 1)[..., None]
    violet, cyan = np.array(VIOLET, np.float32), np.array(CYAN, np.float32)
    color = violet * (1 - along) + cyan * along

    def blob(offset_x: float, offset_y: float, spread: float) -> np.ndarray:
        squared = ((grid_x - centre - offset_x * size) ** 2 + (grid_y - centre - offset_y * size) ** 2)
        return np.exp(-squared / (2 * (spread * size) ** 2))[..., None]

    highlight = blob(0.06, -0.08, 0.13) * 0.28
    color = color * (1 - highlight) + 255.0 * highlight
    color = color * (1 - blob(-0.09, 0.11, 0.12) * 0.10)

    rgba = np.dstack([np.clip(color, 0, 255), np.asarray(mask, np.float32)])
    return Image.fromarray(rgba.astype(np.uint8), "RGBA")


def halo(mask: Image.Image) -> Image.Image:
    """A faint coloured glow, violet on the left and cyan on the right."""
    size = mask.width
    glow = np.asarray(mask.filter(ImageFilter.GaussianBlur(70)), np.float32) / 255.0
    grid_y, grid_x = np.mgrid[0:size, 0:size].astype(np.float32)
    along = np.clip((grid_x - size * 0.20) / (size * 0.62), 0, 1)[..., None]
    left, right = np.array([176, 152, 252], np.float32), np.array([130, 232, 246], np.float32)
    layers = np.dstack([left * (1 - along) + right * along, glow * 0.22 * 255])
    return Image.fromarray(np.clip(layers, 0, 255).astype(np.uint8), "RGBA")


def card(size: int) -> Image.Image:
    """White with a hairline edge, so the mark keeps its shape on a dark page."""
    image = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    margin = size // 170
    ImageDraw.Draw(image).rounded_rectangle(
        [margin, margin, size - 1 - margin, size - 1 - margin],
        radius=int(size * 0.166),
        fill=(*CARD, 255),
        outline=(*BORDER, 255),
        width=max(2, size // 256),
    )
    return image


def main() -> None:
    mask = silhouette(SIZE)
    composed = Image.alpha_composite(Image.alpha_composite(card(SIZE), halo(mask)), paint(mask))
    composed.resize((FINAL, FINAL), Image.LANCZOS).save(OUT)
    print(f"light brand mark written to {os.path.relpath(OUT, ROOT)}")


if __name__ == "__main__":
    main()
