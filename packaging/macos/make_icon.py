#!/usr/bin/env python3
"""Render Tether's app icon and pack it into an .icns.

The design has one job: say "your pointer moves between machines" at 16px as
clearly as at 1024px. That rules out anything finely detailed — two screens and
a cursor crossing the gap between them is about as much as survives the small
sizes, and the cursor is drawn oversized and pinned to the seam so it stays the
thing you notice.

Everything is drawn at 4x and downsampled, which is cheaper than fighting PIL
for antialiased edges.

Usage:  python3 packaging/macos/make_icon.py [output.icns]
"""

import math
import os
import shutil
import subprocess
import sys
import tempfile

from PIL import Image, ImageDraw, ImageFilter

CANVAS = 1024
SS = 4  # supersampling factor
S = CANVAS * SS

# macOS leaves the icon shape inset in its canvas rather than full-bleed.
MARGIN = 100 * SS
BODY = S - 2 * MARGIN

# Indigo through violet. Dark enough that the white glyph carries the contrast
# on both light and dark Dock backgrounds.
GRAD_TOP = (99, 102, 241)
GRAD_BOTTOM = (79, 70, 229)
GRAD_BOTTOM_DEEP = (67, 56, 202)


def squircle(size, n=5.0, points=1440):
    """Apple's rounded-rect is a superellipse, not a circular-cornered rect.

    A plain rounded rectangle reads as subtly wrong next to system icons — the
    corners meet the edges at a visible seam. n=5 is the usual approximation of
    the shape macOS actually uses.
    """
    a = b = size / 2.0
    pts = []
    for i in range(points):
        t = 2.0 * math.pi * i / points
        ct, st = math.cos(t), math.sin(t)
        x = a * math.copysign(abs(ct) ** (2.0 / n), ct)
        y = b * math.copysign(abs(st) ** (2.0 / n), st)
        pts.append((x + a, y + b))
    return pts


def vertical_gradient(w, h, top, bottom):
    grad = Image.new("RGB", (1, h))
    px = grad.load()
    for y in range(h):
        f = y / max(1, h - 1)
        px[0, y] = tuple(round(top[i] + (bottom[i] - top[i]) * f) for i in range(3))
    return grad.resize((w, h), Image.BICUBIC)


def rounded(draw, box, radius, fill):
    draw.rounded_rectangle(box, radius=radius, fill=fill)


def build():
    icon = Image.new("RGBA", (S, S), (0, 0, 0, 0))

    # ---- body: gradient clipped to the squircle -----------------------------
    mask = Image.new("L", (S, S), 0)
    ImageDraw.Draw(mask).polygon(
        [(x + MARGIN, y + MARGIN) for (x, y) in squircle(BODY)], fill=255
    )

    grad = vertical_gradient(S, S, GRAD_TOP, GRAD_BOTTOM_DEEP).convert("RGBA")
    icon.paste(grad, (0, 0), mask)

    # A soft highlight across the top third, which is what stops a flat
    # gradient looking like a placeholder.
    sheen = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    sd = ImageDraw.Draw(sheen)
    sd.ellipse(
        [MARGIN - BODY * 0.15, MARGIN - BODY * 0.55, MARGIN + BODY * 1.15, MARGIN + BODY * 0.52],
        fill=(255, 255, 255, 46),
    )
    sheen = sheen.filter(ImageFilter.GaussianBlur(BODY * 0.05))
    icon = Image.alpha_composite(icon, Image.composite(sheen, Image.new("RGBA", (S, S)), mask))

    d = ImageDraw.Draw(icon)

    def px(fx, fy):
        return (MARGIN + fx * BODY, MARGIN + fy * BODY)

    # ---- two screens --------------------------------------------------------
    # Solid fills, not outlines. A 2%-of-body stroke is under two pixels once
    # this is downsampled to 16x16 and simply disappears; a filled shape keeps
    # its silhouette all the way down. No stands either — at small sizes they
    # turn into detached specks that read as artefacts.
    #
    # Left screen larger, and the two offset vertically: the asymmetry is what
    # stops the pair reading as one wide blob when the gap closes up.
    left = (*px(0.09, 0.325), *px(0.45, 0.61))
    right = (*px(0.585, 0.39), *px(0.91, 0.64))

    d.rounded_rectangle(left, radius=BODY * 0.045, fill=(255, 255, 255, 255))
    d.rounded_rectangle(right, radius=BODY * 0.040, fill=(255, 255, 255, 214))

    # ---- the cursor, crossing the gap --------------------------------------
    # Sits mostly in the gap so it has the indigo background for contrast,
    # overlapping each screen just enough to say it travels between them.
    arrow = [
        (0.00, 0.00), (0.00, 1.02), (0.25, 0.77), (0.41, 1.14),
        (0.59, 1.06), (0.43, 0.70), (0.72, 0.68),
    ]
    ah = BODY * 0.46
    aw = ah * 0.72
    ax, ay = MARGIN + BODY * 0.375, MARGIN + BODY * 0.268
    pts = [(ax + fx * aw, ay + fy * ah) for (fx, fy) in arrow]

    # The rim does the real work: white-on-white where the arrow laps the
    # screens would otherwise merge into one shape. Drawn as a fat stroke
    # under a clean fill so the corners stay sharp.
    rim = max(3, int(BODY * 0.030))
    d.line(pts + [pts[0]], fill=(55, 48, 163, 255), width=rim, joint="curve")
    d.polygon(pts, fill=(55, 48, 163, 255))
    inner = [
        (ax + fx * aw, ay + fy * ah)
        for (fx, fy) in arrow
    ]
    d.polygon(inner, fill=(255, 255, 255, 255))
    d.line(pts + [pts[0]], fill=(55, 48, 163, 255), width=rim, joint="curve")

    return icon.resize((CANVAS, CANVAS), Image.LANCZOS)


def write_icns(icon, out_path):
    tmp = tempfile.mkdtemp()
    iconset = os.path.join(tmp, "tether.iconset")
    os.makedirs(iconset)

    for size in (16, 32, 128, 256, 512):
        icon.resize((size, size), Image.LANCZOS).save(
            os.path.join(iconset, f"icon_{size}x{size}.png")
        )
        icon.resize((size * 2, size * 2), Image.LANCZOS).save(
            os.path.join(iconset, f"icon_{size}x{size}@2x.png")
        )

    subprocess.run(["iconutil", "-c", "icns", iconset, "-o", out_path], check=True)
    shutil.rmtree(tmp)


if __name__ == "__main__":
    out = sys.argv[1] if len(sys.argv) > 1 else "packaging/macos/Tether.icns"
    art = build()

    preview = os.path.splitext(out)[0] + "-1024.png"
    art.save(preview)
    write_icns(art, out)
    print(f"wrote {out} and {preview}")
