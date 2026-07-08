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
is recovered; the title shows the live decoded V/div (real units), time/div and
trigger state/level/frequency. X axis is sample index (sample rate not yet
calibrated). Both channels are shown when displayed on the scope (channel select
= the acquire value byte, solved 2026-07-08).
"""
import argparse, sys, time
import numpy as np
import matplotlib

def fmt_vdiv(mv, vb=None):
    if mv is None: return '?'
    if vb == 0: return "2 mV or 10 V/div"   # VB=0 wraps: both extremes share it
    return f"{mv/1000:g} V/div" if mv >= 1000 else f"{mv:g} mV/div"

def fmt_tdiv(ns):
    if ns is None: return '?'
    for unit, scale in (('s', 1e9), ('ms', 1e6), ('µs', 1e3), ('ns', 1)):
        if ns >= scale:
            return f"{ns/scale:g} {unit}/div"
    return f"{ns:g} ns/div"

def fmt_level(mv):
    if mv is None: return '?'
    return f"{mv/1000:g} V" if abs(mv) >= 1000 else f"{mv:g} mV"

def label(s):
    return (f"MSO5202D  |  CH1 {fmt_vdiv(s.get('CH1-VDIV-mV'), s.get('VERT-CH1-VB'))}  "
            f"CH2 {fmt_vdiv(s.get('CH2-VDIV-mV'), s.get('VERT-CH2-VB'))}  "
            f"|  {fmt_tdiv(s.get('TDIV-ns'))}  |  trig state={s.get('TRIG-STATE')} "
            f"level={fmt_level(s.get('TRIG-LEVEL-mV'))} f={s.get('TRIG-FREQUENCY', 0)/1000:g} Hz")

CH_COLORS = ('#e6b400', '#0a84ff')          # CH1 yellow, CH2 blue (like the scope)

def grab(scope):
    """Read settings + one waveform block per displayed channel.
    Returns ({ch: samples}, settings)."""
    from mso5202d import decode_settings
    s = decode_settings(scope.read_settings())
    waves = {}
    for ch, disp in ((0, s['VERT-CH1-DISP']), (1, s['VERT-CH2-DISP'])):
        if disp:
            waves[ch] = np.frombuffer(scope.read_waveform(ch), dtype=np.uint8)
    return waves, s

def run_png(path, frames):
    matplotlib.use('Agg')
    import matplotlib.pyplot as plt
    from mso5202d import Scope
    sc = Scope()
    waves, s = grab(sc)
    for _ in range(max(0, frames - 1)):        # warm up / latest frame
        waves, s = grab(sc)
    fig, ax = plt.subplots(figsize=(10, 4.5))
    n = 0
    for ch, y in waves.items():
        ax.plot(np.arange(len(y)), y, lw=0.8, color=CH_COLORS[ch], label=f"CH{ch+1}")
        n = max(n, len(y))
    # Raw counts are inverted vs the screen (higher count = lower trace) —
    # flip the Y axis so traces sit like they do on the scope display.
    ax.set_ylim(255, 0); ax.set_xlim(0, n or 1)
    ax.set_xlabel("sample"); ax.set_ylabel("ADC counts (0-255, screen-oriented)")
    ax.set_title(label(s), fontsize=9)
    ax.legend(loc='upper right', fontsize=8)
    ax.grid(True, alpha=0.3)
    fig.tight_layout(); fig.savefig(path, dpi=110)
    for ch, y in waves.items():
        print(f"[+] CH{ch+1}: {len(y)} samples, min={int(y.min())} max={int(y.max())}")
    print(f"[+] saved {path}")
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
    lines = [ax.plot([], [], lw=0.8, color=CH_COLORS[ch], label=f"CH{ch+1}")[0]
             for ch in (0, 1)]
    ax.set_ylim(255, 0)                  # inverted: match the scope's screen
    ax.set_xlabel("sample"); ax.set_ylabel("ADC counts (0-255, screen-oriented)")
    ax.legend(loc='upper right', fontsize=8)
    ax.grid(True, alpha=0.3)
    state = {'n': 0}

    def update(_):
        try:
            waves, s = grab(sc)
        except Exception as e:
            ax.set_title(f"read error: {e}", fontsize=9); return lines
        n = 0
        for ch, line in enumerate(lines):
            y = waves.get(ch)
            if y is None or len(y) == 0:     # off / empty read — keep last trace
                continue
            line.set_data(np.arange(len(y)), y)
            n = max(n, len(y))
        if n:
            ax.set_xlim(0, n)
        ax.set_title(label(s), fontsize=9)
        state['n'] += 1
        if max_frames and state['n'] >= max_frames:
            plt.close(fig)
        return lines

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
