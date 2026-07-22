#!/usr/bin/env python3
"""Generate the openmso5202D application icon set.

The icon is the app's own plot area in miniature: a dark scope screen with a faint
graticule and a CH1-yellow digital pulse train, so it reads as "signal capture" rather
than a generic window. Colours are taken from `frontend/src/theme.css`.

Everything is drawn on a 4x supersampled canvas and downsampled with Lanczos, which is
what keeps the 32px icon's 2px trace clean — drawing directly at 32px gives stair-stepped
diagonals and a muddy grid.

Run from anywhere; it writes next to itself:

    python3 frontend/src-tauri/icons/make-icons.py
"""

from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter

# --- palette (frontend/src/theme.css) ---------------------------------------
SCREEN_TOP = (13, 16, 23)  # --bg-plot, lifted slightly for a vertical gradient
SCREEN_BOTTOM = (18, 22, 31)
BEZEL = (36, 46, 60)
GRID = (28, 38, 50)
AXIS = (44, 58, 74)
TRACE = (245, 197, 66)  # --ch1
GLOW = (245, 197, 66)

# --- geometry, in units of the final canvas ---------------------------------
SS = 4  # supersample factor
BASE = 512  # design canvas
CORNER = 0.19  # corner radius as a fraction of the canvas
INSET = 0.085  # screen inset from the canvas edge
TRACE_W = 0.062  # trace stroke width as a fraction of the canvas
GRID_W = 0.006
DIVISIONS = 4  # graticule cells per axis

# The digital pattern the trace draws: one entry per bit, left to right.
# Chosen to look like data rather than a clock — a clock's even pulses read as a barcode.
BITS = [0, 1, 0, 1, 1, 0]
HIGH = 0.30  # trace levels, as a fraction of the screen height
LOW = 0.70


def rounded_mask(size: int, radius: float) -> Image.Image:
    """An antialiased rounded-square alpha mask."""
    mask = Image.new("L", (size, size), 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        (0, 0, size - 1, size - 1), radius=radius, fill=255
    )
    return mask


def vertical_gradient(size: int, top: tuple, bottom: tuple) -> Image.Image:
    """A one-pixel-wide gradient stretched to the full canvas."""
    strip = Image.new("RGB", (1, size))
    pixels = strip.load()
    for y in range(size):
        t = y / max(1, size - 1)
        pixels[0, y] = tuple(round(a + (b - a) * t) for a, b in zip(top, bottom))
    return strip.resize((size, size), Image.Resampling.BILINEAR)


def trace_points(x0: float, y0: float, w: float, h: float) -> list:
    """The pulse train as a polyline, spanning the screen rect."""
    step = w / len(BITS)
    y_for = {1: y0 + h * HIGH, 0: y0 + h * LOW}
    points = []
    previous = None
    for index, bit in enumerate(BITS):
        x = x0 + index * step
        y = y_for[bit]
        # A transition is a vertical edge at the bit boundary: land on the new level at
        # the same x the previous level ends, which is what makes the corners square.
        if previous is not None and bit != previous:
            points.append((x, y_for[previous]))
        points.append((x, y))
        points.append((x + step, y))
        previous = bit
    return points


def render(size: int) -> Image.Image:
    """Draw the icon at `size`, via a supersampled canvas."""
    canvas = size * SS
    unit = canvas  # geometry fractions are of the whole canvas

    # Screen body: gradient clipped to a rounded square.
    icon = Image.new("RGBA", (canvas, canvas), (0, 0, 0, 0))
    body = vertical_gradient(canvas, SCREEN_TOP, SCREEN_BOTTOM).convert("RGBA")
    icon.paste(body, (0, 0), rounded_mask(canvas, CORNER * unit))

    draw = ImageDraw.Draw(icon)

    # Graticule, inside the screen rect.
    inset = INSET * unit
    x0, y0 = inset, inset
    x1, y1 = canvas - inset, canvas - inset
    w, h = x1 - x0, y1 - y0
    grid_w = max(1, round(GRID_W * unit))
    for i in range(1, DIVISIONS):
        x = x0 + w * i / DIVISIONS
        y = y0 + h * i / DIVISIONS
        colour = AXIS if i == DIVISIONS // 2 else GRID
        draw.line([(x, y0), (x, y1)], fill=colour, width=grid_w)
        draw.line([(x0, y), (x1, y)], fill=colour, width=grid_w)

    # Trace, with a phosphor glow underneath it.
    points = trace_points(x0, y0, w, h)
    stroke = max(2, round(TRACE_W * unit))

    glow = Image.new("RGBA", (canvas, canvas), (0, 0, 0, 0))
    ImageDraw.Draw(glow).line(points, fill=GLOW + (120,), width=round(stroke * 1.9), joint="curve")
    icon.alpha_composite(glow.filter(ImageFilter.GaussianBlur(stroke * 0.9)))

    draw.line(points, fill=TRACE + (255,), width=stroke, joint="curve")
    # Square off the stroke ends, which PIL leaves flush and slightly thin.
    for end in (points[0], points[-1]):
        draw.ellipse(
            [end[0] - stroke / 2, end[1] - stroke / 2, end[0] + stroke / 2, end[1] + stroke / 2],
            fill=TRACE + (255,),
        )

    # Bezel: a hairline inside the outer edge, so the icon keeps its shape on a dark
    # taskbar where the body would otherwise bleed into the background.
    draw.rounded_rectangle(
        (0, 0, canvas - 1, canvas - 1),
        radius=CORNER * unit,
        outline=BEZEL,
        width=max(1, round(0.012 * unit)),
    )

    return icon.resize((size, size), Image.Resampling.LANCZOS)


def main() -> None:
    out = Path(__file__).resolve().parent
    # Sizes Tauri's bundler and the Linux hicolor theme want.
    for name, size in [
        ("32x32.png", 32),
        ("128x128.png", 128),
        ("128x128@2x.png", 256),
        ("icon.png", 512),
    ]:
        render(size).save(out / name)
        print(f"wrote {name}")

    # Windows .ico, so a future Windows build has one; harmless on Linux.
    render(256).save(
        out / "icon.ico",
        sizes=[(16, 16), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)],
    )
    print("wrote icon.ico")


if __name__ == "__main__":
    main()
