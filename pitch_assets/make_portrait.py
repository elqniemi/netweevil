#!/usr/bin/env python
"""Crop the speaker photo to an antialiased circle with a mint ring.

Reads pitch_assets/portrait_src.jpeg, writes pitch_assets/maps/portrait_circle.png
(RGBA, transparent outside the circle) for the FOSS4G title slide.
"""

import os
from PIL import Image, ImageDraw

ASSETS = os.path.dirname(os.path.abspath(__file__))
SRC = os.path.join(ASSETS, "portrait_src.jpeg")
OUT = os.path.join(ASSETS, "maps", "portrait_circle.png")

MINT = "#2DE5A0"
SIZE = 840          # output px
SS = 4              # supersample factor for smooth edges
RING = 10           # ring width at output size

im = Image.open(SRC).convert("RGB")
w, h = im.size
# Square crop centred on the face (full width, head roughly centred).
side = w
top = max(0, min(h - side, (h - side) // 2 - 30))
im = im.crop((0, top, side, top + side))
big = SIZE * SS
im = im.resize((big, big), Image.LANCZOS)

mask = Image.new("L", (big, big), 0)
d = ImageDraw.Draw(mask)
d.ellipse([0, 0, big - 1, big - 1], fill=255)

out = Image.new("RGBA", (big, big), (0, 0, 0, 0))
out.paste(im, (0, 0), mask)

ring = ImageDraw.Draw(out)
rw = RING * SS
ring.ellipse([rw // 2, rw // 2, big - 1 - rw // 2, big - 1 - rw // 2],
             outline=MINT, width=rw)

out = out.resize((SIZE, SIZE), Image.LANCZOS)
out.save(OUT)
print(f"wrote {OUT} ({SIZE}x{SIZE})")
