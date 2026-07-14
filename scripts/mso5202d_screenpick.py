#!/usr/bin/env python3
"""Screen coordinate picker for the MSO5202D.

Grabs the scope's LCD (the `0x20` framebuffer) — or loads a saved PNG — shows it large,
and lets you drag a rectangle to read off pixel coordinates (x1,y1,x2,y2 + width/height)
and the mean RGB inside the box. Use it to calibrate on-screen UI regions (menu radios,
banners, softkey labels, …) instead of eyeballing them.

Coordinates map directly to the framebuffer array: a box (x1,y1,x2,y2) is `img[y1:y2, x1:x2]`
— the same indexing `_grab_fb`/`_read_csv_source` use — so what you pick here can be pasted
straight into the code.

Run from scripts/ (like the other tools):

    python3 mso5202d_screenpick.py                  # live grab from the scope
    python3 mso5202d_screenpick.py --load shot.png  # pick on a saved image (no scope needed)
    python3 mso5202d_screenpick.py --save shot.png  # grab, show, and also save that PNG

Mouse:  drag to draw the box; drag its edges/inside to resize/move it.
Keys:   r = re-grab from scope   s = save a PNG of the current screen   c = clear the box   q = quit
Both the title/overlay and the console show the current box; the bottom-left readout shows the
pixel + RGB under the cursor.
"""
import argparse
import sys
import time

import matplotlib
matplotlib.use('TkAgg')                     # interactive backend (set before pyplot import)
import matplotlib.pyplot as plt
from matplotlib.widgets import RectangleSelector
import numpy as np

W, H = 800, 480


def _connect():
    """Open the scope (reset=False so we don't disturb it), with a few retries. Returns None on
    failure so the picker can still run in --load mode."""
    from mso5202d import Scope
    for a in range(8):
        try:
            sc = Scope(reset=False); sc._resync(); return sc
        except Exception as e:
            print(f"[connect retry {a}] {e}", file=sys.stderr); time.sleep(1.5)
    return None


def _grab(sc):
    """Grab the current framebuffer as an (H,W,3) uint8 array, or None."""
    from mso5202d_plot import _grab_fb
    return _grab_fb(sc)


def main():
    ap = argparse.ArgumentParser(description="MSO5202D screen coordinate picker")
    ap.add_argument('--load', metavar='PNG', help="pick on a saved PNG instead of grabbing live")
    ap.add_argument('--save', metavar='PNG', help="also save the grabbed screen to this PNG")
    a = ap.parse_args()

    sc = None
    if a.load:
        img = plt.imread(a.load)
        if img.dtype != np.uint8:                       # imread gives float 0..1 for PNG
            img = (img[..., :3] * 255).astype(np.uint8)
        else:
            img = img[..., :3]
        title_src = a.load
    else:
        sc = _connect()
        if sc is None:
            print("no scope — pass --load <png> to pick on a saved image", file=sys.stderr)
            sys.exit(1)
        img = _grab(sc)
        if img is None:
            print("framebuffer grab failed", file=sys.stderr); sys.exit(1)
        title_src = "live scope"
        if a.save:
            plt.imsave(a.save, img); print(f"[+] saved {a.save}")

    state = {'img': img, 'box': None}

    fig, ax = plt.subplots(figsize=(13, 8))
    fig.canvas.manager.set_window_title("MSO5202D screen picker")
    im = ax.imshow(state['img'], interpolation='nearest')
    ax.set_xlim(-0.5, W - 0.5); ax.set_ylim(H - 0.5, -0.5)

    HELP = "drag = box   ·   r = re-grab   ·   s = save PNG   ·   c = clear   ·   q = quit"
    def set_title(extra=""):
        ax.set_title(f"{title_src}   ·   {HELP}" + (f"\n{extra}" if extra else "\n(no box)"),
                     fontsize=9)

    # cursor readout: integer pixel + the RGB at that pixel
    def format_coord(x, y):
        xi, yi = int(round(x)), int(round(y))
        if 0 <= xi < W and 0 <= yi < H:
            r, g, b = state['img'][yi, xi][:3]
            return f"x={xi}  y={yi}   rgb=({r},{g},{b})"
        return f"x={xi}  y={yi}"
    ax.format_coord = format_coord

    def on_select(eclick, erelease):
        x1, x2 = sorted((eclick.xdata, erelease.xdata))
        y1, y2 = sorted((eclick.ydata, erelease.ydata))
        x1, x2 = int(round(x1)), int(round(x2))
        y1, y2 = int(round(y1)), int(round(y2))
        x1, x2 = max(0, x1), min(W, x2)
        y1, y2 = max(0, y1), min(H, y2)
        state['box'] = (x1, y1, x2, y2)
        region = state['img'][y1:y2, x1:x2].reshape(-1, 3).astype(float)
        mr, mg, mb = (region.mean(axis=0) if len(region) else (0, 0, 0))
        line = (f"box  x=[{x1},{x2}]  y=[{y1},{y2}]   (w={x2-x1}, h={y2-y1})   "
                f"mean RGB=({mr:.0f},{mg:.0f},{mb:.0f})")
        set_title(line)
        fig.canvas.draw_idle()
        # paste-ready console output (matches the code's coord styles)
        print("\n" + line)
        print(f"    img[{y1}:{y2}, {x1}:{x2}]      # slice")
        print(f"    x0, x1 = {x1}, {x2}")
        print(f"    y0, y1 = {y1}, {y2}")
        print(f"    region = ({x1}, {y1}, {x2}, {y2})")

    selector = RectangleSelector(ax, on_select, useblit=True, button=[1],
                                 minspanx=1, minspany=1, spancoords='data', interactive=True)

    def on_key(ev):
        if ev.key == 'r' and sc is not None:
            new = _grab(sc)
            if new is not None:
                state['img'] = new; im.set_data(new); fig.canvas.draw_idle()
                print("[re-grabbed]")
            else:
                print("[re-grab failed]")
        elif ev.key == 's':
            name = a.save or f"scope_{time.strftime('%H%M%S')}.png"
            plt.imsave(name, state['img']); print(f"[+] saved {name}")
        elif ev.key == 'c':
            state['box'] = None; selector.set_visible(False); set_title(); fig.canvas.draw_idle()

    fig.canvas.mpl_connect('key_press_event', on_key)
    set_title()
    print(HELP)
    try:
        plt.show()
    finally:
        if sc is not None:
            sc.close()


if __name__ == '__main__':
    main()
