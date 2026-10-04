#!/usr/bin/env python3
"""Derive the SmolLLM Studio icon set from the master logo.

Reads assets/brand/logo.png (1024px, transparent rounded corners) and writes
every size Tauri bundles, plus the small mark the sidebar uses. Re-run after
replacing the master:

    python3 scripts/render_icons.py
"""
import os
import shutil
import subprocess

from PIL import Image

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
MASTER = os.path.join(ROOT, "assets", "brand", "logo.png")
ICONS = os.path.join(ROOT, "desktop", "src-tauri", "icons")
UI_MARK = os.path.join(ROOT, "desktop", "src", "assets")

PNG_SIZES = [
    ("32x32.png", 32),
    ("32x32@2x.png", 64),
    ("128x128.png", 128),
    ("128x128@2x.png", 256),
    ("icon.png", 512),
]
ICO_SIZES = [(16, 16), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)]
# iconutil wants a directory of named sizes, not a single file.
ICONSET = [
    (16, "16x16"), (32, "16x16@2x"), (32, "32x32"), (64, "32x32@2x"),
    (128, "128x128"), (256, "128x128@2x"), (256, "256x256"), (512, "256x256@2x"),
    (512, "512x512"), (1024, "512x512@2x"),
]


def scaled(source: Image.Image, px: int) -> Image.Image:
    return source.resize((px, px), Image.LANCZOS)


def main() -> None:
    if not os.path.exists(MASTER):
        raise SystemExit(f"missing master logo: {MASTER}")
    logo = Image.open(MASTER).convert("RGBA")
    if logo.width != logo.height:
        raise SystemExit("the master logo must be square")

    os.makedirs(ICONS, exist_ok=True)
    os.makedirs(UI_MARK, exist_ok=True)

    for name, px in PNG_SIZES:
        scaled(logo, px).save(os.path.join(ICONS, name))
    logo.save(os.path.join(ICONS, "icon_1024.png"))
    # The sidebar renders this at 36px, so 256 is plenty and keeps the bundle small.
    scaled(logo, 256).save(os.path.join(UI_MARK, "logo.png"))

    # Windows: one .ico carrying every size.
    logo.save(
        os.path.join(ICONS, "icon.ico"),
        sizes=[size for size in ICO_SIZES if size[0] <= logo.width],
    )

    # macOS: build an .iconset in a temp dir, then let iconutil pack it.
    iconset = os.path.join(ICONS, "app.iconset")
    os.makedirs(iconset, exist_ok=True)
    for px, suffix in ICONSET:
        scaled(logo, px).save(os.path.join(iconset, f"icon_{suffix}.png"))
    target = os.path.join(ICONS, "icon.icns")
    subprocess.run(["iconutil", "-c", "icns", iconset, "-o", target], check=True)
    shutil.rmtree(iconset)

    print(f"icons written from {os.path.relpath(MASTER, ROOT)} to {os.path.relpath(ICONS, ROOT)}")


if __name__ == "__main__":
    main()
