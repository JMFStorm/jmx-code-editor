#!/usr/bin/env python3
"""Draw the JMX app icon and write assets/icon.png, assets/icon-64.png and assets/icon.ico.

Run from anywhere: python3 tools/make-icon.py  (needs Pillow and the DejaVu Sans Mono font).
The generated files are committed, so building the editor does not need Python.
"""
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "assets"
FONT = "/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf"

BG = (20, 22, 27)  # theme bg #14161b
BORDER = (43, 48, 59)
ACCENT = (97, 175, 239)  # theme accent #61afef
FG = (212, 212, 212)  # theme fg #d4d4d4


def draw(size: int) -> Image.Image:
    # Draw large and scale down for smooth edges.
    s = 1024
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    pad, radius = 40, 190
    d.rounded_rectangle((pad, pad, s - pad, s - pad), radius, fill=BG, outline=BORDER, width=18)
    # Accent bar along the top, like the active tab marker.
    d.rounded_rectangle((pad + 150, pad + 70, s - pad - 150, pad + 110), 20, fill=ACCENT)
    # Terminal prompt ">_".
    prompt = ImageFont.truetype(FONT, 330)
    d.text((150, 230), ">", font=prompt, fill=ACCENT)
    d.rectangle((400, 500, 600, 545), fill=ACCENT)
    # "JMX".
    label = ImageFont.truetype(FONT, 250)
    w = d.textlength("JMX", font=label)
    d.text(((s - w) / 2, 600), "JMX", font=label, fill=FG)
    return img.resize((size, size), Image.LANCZOS)


def main() -> None:
    ASSETS.mkdir(exist_ok=True)
    big = draw(256)
    big.save(ASSETS / "icon.png")
    draw(64).save(ASSETS / "icon-64.png")
    sizes = [16, 24, 32, 48, 64, 128, 256]
    big.save(ASSETS / "icon.ico", sizes=[(n, n) for n in sizes])
    print("wrote", ", ".join(str(p.relative_to(ROOT)) for p in sorted(ASSETS.glob("icon*"))))


if __name__ == "__main__":
    main()
