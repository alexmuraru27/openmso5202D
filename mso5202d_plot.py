#!/usr/bin/env python3
"""
MSO5202D live waveform viewer — a small standalone "scope on your PC" built on the
reverse-engineered driver (mso5202d.py).

Usage:
    python3 mso5202d_plot.py                # live GUI window (needs a display; run
                                            #   as your user with the udev rule)
    python3 mso5202d_plot.py --png out.png  # headless: grab a few frames, save a PNG
    python3 mso5202d_plot.py --frames 200   # live: stop after N frames

Y axis is raw 8-bit ADC counts (0..255) until the counts->volts calibration table
is recovered; the title shows the live decoded V/div index, timebase and trigger.
X axis is sample index (sample rate not yet calibrated). CH2 is omitted because the
device currently returns identical data for both channels (channel-select unsolved).
"""
import argparse, sys, time
import numpy as np
import matplotlib

def label(settings):
    return (f"MSO5202D  |  V/div idx CH1={settings.get('vdiv_ch1')} "
            f"CH2={settings.get('vdiv_ch2')}  |  timebase={settings.get('timebase')}  "
            f"|  trig={settings.get('trigpos')}  status={settings.get('status')}")

def grab(scope):
    from mso5202d import decode_settings
    w = scope.read_waveform(0)
    s = decode_settings(scope.read_settings())
    return np.frombuffer(w, dtype=np.uint8), s

def run_png(path, frames):
    matplotlib.use('Agg')
    import matplotlib.pyplot as plt
    from mso5202d import Scope
    sc = Scope()
    y, s = grab(sc)
    for _ in range(max(0, frames - 1)):        # warm up / latest frame
        y, s = grab(sc)
    fig, ax = plt.subplots(figsize=(10, 4.5))
    ax.plot(np.arange(len(y)), y, lw=0.8, color='#0a84ff')
    ax.set_ylim(0, 255); ax.set_xlim(0, len(y))
    ax.set_xlabel("sample"); ax.set_ylabel("ADC counts (0-255)")
    ax.set_title(label(s), fontsize=9)
    ax.grid(True, alpha=0.3)
    fig.tight_layout(); fig.savefig(path, dpi=110)
    print(f"[+] saved {path}  ({len(y)} samples, min={int(y.min())} max={int(y.max())})")
    sc.close()

def run_live(max_frames):
    # Use Tk (install: sudo apt install python3-tk); fall back if unavailable.
    for be in ('TkAgg', 'QtAgg', 'GTK3Agg'):
        try:
            matplotlib.use(be); break
        except Exception:
            continue
    import matplotlib.pyplot as plt
    print(f"[+] backend: {matplotlib.get_backend()}")
    from matplotlib.animation import FuncAnimation
    from mso5202d import Scope
    sc = Scope()
    fig, ax = plt.subplots(figsize=(11, 5))
    (line,) = ax.plot([], [], lw=0.8, color='#0a84ff')
    ax.set_ylim(0, 255)
    ax.set_xlabel("sample"); ax.set_ylabel("ADC counts (0-255)")
    ax.grid(True, alpha=0.3)
    state = {'n': 0}

    def update(_):
        try:
            y, s = grab(sc)
        except Exception as e:
            ax.set_title(f"read error: {e}", fontsize=9); return line,
        line.set_data(np.arange(len(y)), y)
        ax.set_xlim(0, len(y))
        ax.set_title(label(s), fontsize=9)
        state['n'] += 1
        if max_frames and state['n'] >= max_frames:
            plt.close(fig)
        return line,

    # Keep a reference — a discarded FuncAnimation gets garbage-collected and
    # never renders.
    anim = FuncAnimation(fig, update, interval=50, blit=False, cache_frame_data=False)
    plt.show()
    del anim
    sc.close()

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--png', metavar='PATH', help="headless: save a PNG instead of live GUI")
    ap.add_argument('--frames', type=int, default=0, help="stop after N frames (0=infinite)")
    a = ap.parse_args()
    if a.png:
        run_png(a.png, a.frames or 4)
    else:
        run_live(a.frames)

if __name__ == '__main__':
    main()
