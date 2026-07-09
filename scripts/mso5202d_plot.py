#!/usr/bin/env python3
"""
MSO5202D live waveform viewer — a small standalone "scope on your PC" built on the
reverse-engineered driver (mso5202d.py).

Usage:
    python3 mso5202d_plot.py                # live GUI window (needs a display; run
                                            #   as your user with the udev rule)
    python3 mso5202d_plot.py --png out.png  # headless: grab a few frames, save a PNG
    python3 mso5202d_plot.py --frames 200   # live: stop after N frames

Rendering follows the scope's own drawing model (see docs/MSO5202D-rendering.md):
the trace is drawn on a fixed **8×10 division graticule**, never auto-fit to the
data. The vertical axis is in **divisions** at a fixed **25 counts/div** (each
channel's volts/div differs, so divisions are the honest shared axis, exactly as
on the scope face); the horizontal axis is in **divisions** at 200 samples/div
(a 3840-sample block spans 19.2 div). Samples pinned to the rails are off-screen
and the trace is **broken** there rather than drawn as a flat line. The title
carries the real units (V/div, time/div, sample rate, trigger state/level/freq).
"""
import argparse
import atexit
import os
import subprocess
import tempfile
from shutil import which
import numpy as np
import matplotlib
from mso5202d import SAMPLES_PER_DIV, DIV_UNIT


class ScopeCapture:
    """For testing: record scope-only USB traffic for the plot session into a
    single git-ignored temp pcap (`<repo>/.plot_captures/scope.pcapng`),
    overwritten on every run. Best-effort — silently disables itself if tshark /
    usbmon access isn't available, so the viewer always still runs."""
    def __init__(self):
        root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
        self.dir = os.path.join(root, ".plot_captures")
        self.out = os.path.join(self.dir, "scope.pcapng")
        self.proc = self.raw = self.addr = None

    def start(self):
        if not which("tshark"):
            print("[capture] tshark not found — pcap disabled"); return self
        try:
            import usb.core
            d = usb.core.find(idVendor=0x049F, idProduct=0x505A)
            bus, self.addr = d.bus, d.address
        except Exception as e:
            print(f"[capture] scope not found for pcap: {e}"); return self
        dev = f"/dev/usbmon{bus}"
        if not os.path.exists(dev):
            print(f"[capture] {dev} missing — run:  sudo modprobe usbmon   (pcap disabled)")
            return self
        if not os.access(dev, os.R_OK):
            print(f"[capture] no read access to {dev} — run:  "
                  f"sudo setfacl -m u:$USER:r {dev}   (pcap disabled)")
            return self
        os.makedirs(self.dir, exist_ok=True)
        self.raw = tempfile.NamedTemporaryFile(suffix=".pcapng", delete=False).name
        try:
            self.proc = subprocess.Popen(
                ["tshark", "-i", f"usbmon{bus}", "-w", self.raw],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            atexit.register(self.stop)
            print(f"[capture] recording scope (bus {bus} dev {self.addr}) → {self.out}")
        except Exception as e:
            print(f"[capture] could not start: {e}"); self.proc = None
        return self

    def stop(self):
        if not self.proc:
            return
        p, self.proc = self.proc, None
        try:
            p.terminate(); p.wait(timeout=5)
        except Exception:
            try: p.kill()
            except Exception: pass
        try:                                   # filter to scope-only, overwrite the one file
            subprocess.run(["tshark", "-r", self.raw, "-Y",
                            f"usb.device_address == {self.addr}", "-w", self.out],
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=90)
            n = 0
            if os.path.exists(self.out):
                r = subprocess.run(["tshark", "-r", self.out, "-T", "fields", "-e", "frame.number"],
                                   capture_output=True, text=True)
                n = sum(1 for x in r.stdout.split() if x.strip())
            if n:
                print(f"[capture] saved {self.out}  ({n} scope packets)")
            else:
                print(f"[capture] 0 packets captured — usbmon not readable? "
                      f"(sudo modprobe usbmon; sudo setfacl -m u:$USER:r /dev/usbmon<bus>)")
        except Exception as e:
            print(f"[capture] filter failed: {e}")
        finally:
            try: os.unlink(self.raw)
            except OSError: pass

# --- rendering model (docs/MSO5202D-rendering.md) --------------------------------
# Measured from a CH1 position sweep with a 1-div cal signal: an on-screen trace
# is a constant ~28 counts/div (amplitude does NOT depend on position); the rails
# sit at ~8 (0x08) and ~242 (0xF2), and an off-screen/parked trace flat-lines
# near mid-code (~128). Frames that come back mostly rail-pinned are screen-edge
# transition artifacts ("hash"), not real waveforms — reject them.
# Vertical model (measured from a POS-correlated capture): the scope encodes each
# sample as (VERT-CHx-POS + 16 + signal) mod 256. So the raw byte both **reverses**
# (it rises as the trace moves up) and **wraps** at the 8-bit boundary as the trace
# nears screen centre — the cause of the "reverse movement" and the centre "hash".
# Recover the true trace by unwrapping each sample around the POS-derived baseline.
# DIV_UNIT (=25) is simultaneously counts-per-division and the POS unit (1/25 div).
BASELINE_OFFSET = 16             # byte baseline = (VERT-CHx-POS + 16) mod 256
V_DIVS          = 8              # graticule is 8 divisions tall (-4 … +4)
CH_COLORS = ('#e6b400', '#0a84ff')          # CH1 yellow, CH2 blue (like the scope)
GRID, GRID_MINOR, AXIS = '#274427', '#182a19', '#3f6b3f'
BG, FG = '#080a08', '#9fb0a0'

def to_divs(y_bytes, pos):
    """Waveform byte + the channel's VERT-POS → vertical divisions (up = positive).
    Unwraps the sample around the POS-referenced baseline, which undoes the 8-bit
    wrap near screen centre (fixes the centre "hash") and the reversed sense (fixes
    the reverse movement), and places the trace at its true division."""
    pos = int(pos)
    base = (pos + BASELINE_OFFSET) & 0xFF
    sig = ((y_bytes.astype(int) - base + 128) % 256) - 128   # signal AC, unwrapped
    return (pos + sig) / DIV_UNIT

def off_screen(pos):
    return abs(int(pos)) / DIV_UNIT > V_DIVS / 2

def x_divs(n):
    """Sample index → horizontal divisions (200 samples/div), block start = 0."""
    return np.arange(n) / SAMPLES_PER_DIV

def style_scope(ax, n_div_h):
    """Draw the scope-style graticule: 8 tall × n_div_h wide divisions, with 5
    minor subdivisions per division, a bold centre line, on a dark face."""
    ax.set_facecolor(BG)
    ax.set_xlim(0, n_div_h); ax.set_ylim(-V_DIVS / 2, V_DIVS / 2)
    ax.set_xticks(np.arange(0, n_div_h + 1e-6, 1))
    ax.set_yticks(np.arange(-V_DIVS / 2, V_DIVS / 2 + 1e-6, 1))
    ax.set_xticks(np.arange(0, n_div_h + 1e-6, 0.2), minor=True)
    ax.set_yticks(np.arange(-V_DIVS / 2, V_DIVS / 2 + 1e-6, 0.2), minor=True)
    ax.grid(True, which='major', color=GRID, lw=0.6)
    ax.grid(True, which='minor', color=GRID_MINOR, lw=0.4)
    ax.axhline(0, color=AXIS, lw=1.0)
    ax.tick_params(colors=FG, labelsize=7)
    for sp in ax.spines.values():
        sp.set_color(GRID)
    ax.set_xlabel("divisions (200 Sa/div)", color=FG, fontsize=8)
    ax.set_ylabel("divisions (25 counts/div)", color=FG, fontsize=8)

# --- title / status --------------------------------------------------------------
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

def fmt_rate(hz):
    if not hz: return '?'
    for unit, scale in (('GSa/s', 1e9), ('MSa/s', 1e6), ('kSa/s', 1e3), ('Sa/s', 1)):
        if hz >= scale:
            return f"{hz/scale:g} {unit}"
    return f"{hz:g} Sa/s"

def label(s):
    from mso5202d import (TRIG_STATE_NAMES, TRIG_TYPE_NAMES, TRIG_SRC_NAMES,
                          TRIG_MODE_NAMES)
    st = TRIG_STATE_NAMES.get(s.get('TRIG-STATE'), f"?{s.get('TRIG-STATE')}")
    ty = TRIG_TYPE_NAMES.get(s.get('TRIG-TYPE'), f"?{s.get('TRIG-TYPE')}")
    src = TRIG_SRC_NAMES.get(s.get('TRIG-SRC'), f"?{s.get('TRIG-SRC')}")
    mode = TRIG_MODE_NAMES.get(s.get('TRIG-MODE'), '')
    slope = {0: '↑', 1: '↓'}.get(s.get('TRIG-EDGE-SLOPE'), '')
    return (f"MSO5202D  |  CH1 {fmt_vdiv(s.get('CH1-VDIV-mV'), s.get('VERT-CH1-VB'))}  "
            f"CH2 {fmt_vdiv(s.get('CH2-VDIV-mV'), s.get('VERT-CH2-VB'))}  "
            f"|  {fmt_tdiv(s.get('TDIV-ns'))} ({fmt_rate(s.get('SAMPLERATE-HZ'))})  |  "
            f"trig {st} {ty}{slope} {src} {mode} "
            f"level={fmt_level(s.get('TRIG-LEVEL-mV'))} f={s.get('TRIG-FREQUENCY', 0)/1000:g} Hz")

def clip_note(waves, s):
    """Flag any displayed channel whose position parks it off the 8-div screen."""
    flags = [f"CH{ch+1} off-screen"
             for ch in sorted(waves) if off_screen(s.get(f'VERT-CH{ch+1}-POS', 0))]
    return ("  |  ⚠ " + ", ".join(flags)) if flags else ""

# --- acquisition -----------------------------------------------------------------
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


def _title(ax, s, waves):
    ax.set_title(label(s) + clip_note(waves, s), fontsize=8, color=FG)

# --- outputs ---------------------------------------------------------------------
def run_png(path, frames, capture=True):
    matplotlib.use('Agg')
    import matplotlib.pyplot as plt
    from mso5202d import Scope
    sc = Scope()
    cap = ScopeCapture().start() if capture else None
    try:
        waves, s = grab(sc)
        for _ in range(max(0, frames - 1)):        # warm up / latest frame
            waves, s = grab(sc)
        n = max((len(y) for y in waves.values()), default=SAMPLES_PER_DIV)
        fig, ax = plt.subplots(figsize=(11, 5)); fig.patch.set_facecolor(BG)
        style_scope(ax, n / SAMPLES_PER_DIV)
        for ch, y in waves.items():
            ax.plot(x_divs(len(y)), to_divs(y, s[f'VERT-CH{ch+1}-POS']), lw=1.0,
                    color=CH_COLORS[ch], label=f"CH{ch+1}", solid_capstyle='round')
        _title(ax, s, waves)
        leg = ax.legend(loc='upper right', fontsize=8, facecolor=BG, edgecolor=GRID)
        for t in leg.get_texts(): t.set_color(FG)
        fig.tight_layout(); fig.savefig(path, dpi=110, facecolor=BG)
        for ch, y in waves.items():
            print(f"[+] CH{ch+1}: {len(y)} samples, min={int(y.min())} max={int(y.max())}")
        print(f"[+] saved {path}")
    finally:
        if cap: cap.stop()
        sc.close()

def run_live(max_frames, capture=True):
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
    cap = ScopeCapture().start() if capture else None
    fig, ax = plt.subplots(figsize=(11, 5)); fig.patch.set_facecolor(BG)
    style_scope(ax, SAMPLES_PER_DIV and 3840 / SAMPLES_PER_DIV or 10)
    lines = [ax.plot([], [], lw=1.0, color=CH_COLORS[ch], label=f"CH{ch+1}",
                     solid_capstyle='round')[0] for ch in (0, 1)]
    leg = ax.legend(loc='upper right', fontsize=8, facecolor=BG, edgecolor=GRID)
    for t in leg.get_texts(): t.set_color(FG)
    state = {'n': 0}

    def update(_):
        try:
            waves, s = grab(sc)
        except Exception as e:
            ax.set_title(f"read error: {e}", fontsize=9, color=FG); return lines
        nmax = 0
        for ch, line in enumerate(lines):
            y = waves.get(ch)
            if y is None or len(y) == 0:     # off / empty read — keep last trace
                continue
            line.set_data(x_divs(len(y)), to_divs(y, s[f'VERT-CH{ch+1}-POS']))
            nmax = max(nmax, len(y))
        if nmax:
            ax.set_xlim(0, nmax / SAMPLES_PER_DIV)
        _title(ax, s, waves)
        state['n'] += 1
        if max_frames and state['n'] >= max_frames:
            plt.close(fig)
        return lines

    # Keep a reference — a discarded FuncAnimation gets GC'd and never renders.
    anim = FuncAnimation(fig, update, interval=50, blit=False, cache_frame_data=False)
    try:
        plt.show()
    finally:
        del anim
        if cap: cap.stop()
        sc.close()

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--png', metavar='PATH', help="headless: save a PNG instead of live GUI")
    ap.add_argument('--frames', type=int, default=0, help="stop after N frames (0=infinite)")
    ap.add_argument('--no-capture', action='store_true',
                    help="don't record the scope-only pcap to .plot_captures/scope.pcapng")
    a = ap.parse_args()
    if a.png:
        run_png(a.png, a.frames or 4, capture=not a.no_capture)
    else:
        run_live(a.frames, capture=not a.no_capture)

if __name__ == '__main__':
    main()
