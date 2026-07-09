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
from mso5202d import decode_la as _decode_la


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
# Delay between waveform polls. Each poll blocks the GUI thread while it reads
# USB, so this gap is when the window stays responsive (drag/resize/close). Bigger
# = smoother GUI + slower trace refresh; smaller = faster trace + laggier window.
POLL_INTERVAL_MS = 100

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

# --- logic analyzer (D0..D15) ----------------------------------------------------
# LA acquires as 3840 16-bit words (02 01 05, 2 bytes/sample); bit N = channel
# D(N). When LA is on the enabled channels are stacked as evenly spaced digital
# rows across the graticule (lowest Dn at the bottom), overlaid on any analog.
#
# DISABLED BY DEFAULT: the raw `02 01 05` LA read is a half-wired firmware path.
# It returns unreliable data (2-state garbage at most timebases; only partially
# coherent at slow ones) AND — critically — corrupts the scope's own on-screen LA
# display while reading. Until a safe LA readout is found (likely the 0x20
# framebuffer, which is how the vendor's virtual panel shows LA), we do NOT poll
# it. Flip LA_READ_ENABLED to True only for controlled RE experiments.
LA_READ_ENABLED = False
LA_COLOR = '#2ec27e'

def la_enabled(state):
    """Enabled digital channels from LA-CHANNEL-STATE (bit N = D(N))."""
    return [n for n in range(16) if (int(state) >> n) & 1]

def la_geometry(enabled):
    """Place the enabled D-channels as evenly spaced rows in the graticule,
    lowest Dn at the bottom. Returns {n: (row_center_div, amplitude_div)}."""
    k = len(enabled)
    if not k:
        return {}
    top, bot = V_DIVS / 2 - 0.25, -V_DIVS / 2 + 0.25
    gap = (top - bot) / k
    amp = min(gap * 0.55, 0.45)
    return {n: (bot + (i + 0.5) * gap, amp) for i, n in enumerate(enabled)}

def la_trace_y(words, n, yc, amp):
    """0/1 trace for Dn in its row: bit N of each 16-bit word → low/high level."""
    return yc - amp / 2 + ((words >> n) & 1) * amp

def draw_la(la_lines, la_labels, s, words):
    """Update the 16 pre-created LA row artists (line + label per Dn) from the
    latest sample words. Enabled channels get a stacked digital trace; the rest
    are cleared. Returns the sample count drawn (0 if LA off / no data)."""
    if not (s.get('LA-SWI') and words):
        for ln in la_lines: ln.set_data([], [])
        for tx in la_labels: tx.set_text('')
        return 0
    w = np.asarray(words, dtype=np.uint16)
    x = x_divs(len(w))
    geom = la_geometry(la_enabled(s.get('LA-CHANNEL-STATE', 0)))
    for n in range(16):
        if n in geom:
            yc, amp = geom[n]
            la_lines[n].set_data(x, la_trace_y(w, n, yc, amp))
            la_labels[n].set_position((0.1, yc)); la_labels[n].set_text(f"D{n}")
        else:
            la_lines[n].set_data([], []); la_labels[n].set_text('')
    return len(w)

def make_la_artists(ax):
    """Pre-create the 16 LA line + label artists (empty)."""
    la_lines = [ax.plot([], [], lw=1.0, color=LA_COLOR, solid_capstyle='round')[0]
                for _ in range(16)]
    la_labels = [ax.text(0.1, 0, '', color=LA_COLOR, fontsize=6, va='center',
                         ha='left', clip_on=True) for _ in range(16)]
    return la_lines, la_labels

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

def fmt_time(ns):
    """Plain duration (no /div), e.g. total time across the screen."""
    if ns is None: return '?'
    for unit, scale in (('s', 1e9), ('ms', 1e6), ('µs', 1e3), ('ns', 1)):
        if ns >= scale:
            return f"{ns/scale:.3g} {unit}"
    return f"{ns:g} ns"

def fmt_span(s, n):
    """Total time drawn on screen = samples × sample-interval (the full block spans
    n/200 divisions)."""
    dt = s.get('SAMPLE-INTERVAL-ns')
    return fmt_time(dt * n) if (dt and n) else '?'

def fmt_level(mv):
    if mv is None: return '?'
    return f"{mv/1000:g} V" if abs(mv) >= 1000 else f"{mv:g} mV"

def fmt_rate(hz):
    if not hz: return '?'
    for unit, scale in (('GSa/s', 1e9), ('MSa/s', 1e6), ('kSa/s', 1e3), ('Sa/s', 1)):
        if hz >= scale:
            return f"{hz/scale:g} {unit}"
    return f"{hz:g} Sa/s"

def label(s, n=None):
    from mso5202d import (TRIG_STATE_NAMES, TRIG_TYPE_NAMES, TRIG_SRC_NAMES,
                          TRIG_MODE_NAMES)
    st = TRIG_STATE_NAMES.get(s.get('TRIG-STATE'), f"?{s.get('TRIG-STATE')}")
    ty = TRIG_TYPE_NAMES.get(s.get('TRIG-TYPE'), f"?{s.get('TRIG-TYPE')}")
    src = TRIG_SRC_NAMES.get(s.get('TRIG-SRC'), f"?{s.get('TRIG-SRC')}")
    mode = TRIG_MODE_NAMES.get(s.get('TRIG-MODE'), '')
    slope = {0: '↑', 1: '↓'}.get(s.get('TRIG-EDGE-SLOPE'), '')
    return (f"MSO5202D  |  CH1 {fmt_vdiv(s.get('CH1-VDIV-mV'), s.get('VERT-CH1-VB'))}  "
            f"CH2 {fmt_vdiv(s.get('CH2-VDIV-mV'), s.get('VERT-CH2-VB'))}  "
            f"|  {fmt_tdiv(s.get('TDIV-ns'))} ({fmt_rate(s.get('SAMPLERATE-HZ'))})  "
            f"| screen {fmt_span(s, n)}  |  "
            f"trig {st} {ty}{slope} {src} {mode} "
            f"level={fmt_level(s.get('TRIG-LEVEL-mV'))} f={s.get('TRIG-FREQUENCY', 0)/1000:g} Hz")

def clip_note(s):
    """Flag any displayed channel whose position parks it off the 8-div screen."""
    flags = [f"CH{ch+1} off-screen" for ch in (0, 1)
             if s.get(f'VERT-CH{ch+1}-DISP') and off_screen(s.get(f'VERT-CH{ch+1}-POS', 0))]
    return ("  |  ⚠ " + ", ".join(flags)) if flags else ""

# --- acquisition -----------------------------------------------------------------
# Live reads fail fast: if the scope misses a response (it does so occasionally,
# and while a front-panel knob is being turned), we skip that frame and keep the
# last trace instead of blocking the GUI on seconds of nested timeouts/retries.
LIVE_TIMEOUT_MS = 400
LIVE_RETRIES = 0

def read_settings(scope, timeout=3000, retries=2):
    from mso5202d import decode_settings
    return decode_settings(scope.read_settings(timeout, retries))

def read_waves(scope, s, timeout=2000, retries=2):
    """One waveform block per displayed channel, using the (cached) settings to
    know which channels are on."""
    waves = {}
    for ch, disp in ((0, s['VERT-CH1-DISP']), (1, s['VERT-CH2-DISP'])):
        if disp:
            waves[ch] = np.frombuffer(
                scope.read_waveform(ch, retries=retries, timeout=timeout), dtype=np.uint8)
    return waves

def grab(scope):
    """Settings + waveforms + LA block (one-shot PNG path — generous timeouts)."""
    s = read_settings(scope)
    la = _decode_la(scope.read_la()) if (s.get('LA-SWI') and LA_READ_ENABLED) else []
    return read_waves(scope, s), s, la


def _title(ax, s, n=None):
    ax.set_title(label(s, n) + clip_note(s), fontsize=8, color=FG)

# --- outputs ---------------------------------------------------------------------
def run_png(path, frames, capture=True):
    matplotlib.use('Agg')
    import matplotlib.pyplot as plt
    from mso5202d import Scope
    sc = Scope()
    cap = ScopeCapture().start() if capture else None
    try:
        waves, s, la = grab(sc)
        for _ in range(max(0, frames - 1)):        # warm up / latest frame
            waves, s, la = grab(sc)
        n = max([len(y) for y in waves.values()] + [len(la)], default=SAMPLES_PER_DIV)
        n = n or SAMPLES_PER_DIV
        fig, ax = plt.subplots(figsize=(11, 5)); fig.patch.set_facecolor(BG)
        style_scope(ax, n / SAMPLES_PER_DIV)
        for ch, y in waves.items():
            ax.plot(x_divs(len(y)), to_divs(y, s[f'VERT-CH{ch+1}-POS']), lw=1.0,
                    color=CH_COLORS[ch], label=f"CH{ch+1}", solid_capstyle='round')
        la_lines, la_labels = make_la_artists(ax)
        draw_la(la_lines, la_labels, s, la)
        _title(ax, s, n)
        if waves:                                     # only legend labelled analog traces
            leg = ax.legend(loc='upper right', fontsize=8, facecolor=BG, edgecolor=GRID)
            for t in leg.get_texts(): t.set_color(FG)
        fig.tight_layout(); fig.savefig(path, dpi=110, facecolor=BG)
        for ch, y in waves.items():
            print(f"[+] CH{ch+1}: {len(y)} samples, min={int(y.min())} max={int(y.max())}")
        if s.get('LA-SWI') and la:
            print(f"[+] LA: {len(la)} samples, {len(la_enabled(s.get('LA-CHANNEL-STATE',0)))} channels on")
        print(f"[+] saved {path}")
    finally:
        if cap: cap.stop()
        sc.close()

import threading, time as _time

class Reader(threading.Thread):
    """Owns all scope I/O on its own thread so the GUI never blocks on USB.
    Continuously reads settings (throttled) and one displayed channel per cycle
    (round-robin), publishing the latest (settings, {ch: bytes}) under a lock. The
    GUI just snapshots this — a ~100 ms acquire or a missed frame stalls only this
    thread, never the window."""
    SETTINGS_TTL = 2.0

    def __init__(self, scope):
        super().__init__(daemon=True)
        self.sc = scope
        self._lock = threading.Lock()
        self._halt = threading.Event()
        self.s = None
        self.waves = {}
        self.la = []                                       # latest LA sample words
        self.reconnecting = False

    def snapshot(self):
        with self._lock:
            return self.s, dict(self.waves), list(self.la)

    def stop(self):
        self._halt.set()

    @staticmethod
    def _dead(e):
        """A dead handle (scope unplugged/rebooted → re-enumerated) vs a mere
        busy-timeout: timeouts are recoverable, any other USBError means the
        device we had is gone and we must reconnect."""
        import usb.core
        return isinstance(e, usb.core.USBError) and not isinstance(e, usb.core.USBTimeoutError)

    def _reconnect(self):
        """Scope disappeared — drop the old handle and keep trying to open the
        re-enumerated device (new USB address) until it's back or we're stopped."""
        from mso5202d import Scope
        self.reconnecting = True
        try: self.sc.close()
        except Exception: pass
        while not self._halt.is_set():
            try:
                self.sc = Scope()                          # re-find + reset + claim
                self.reconnecting = False
                return
            except Exception:
                self._halt.wait(1.0)                        # scope not back yet

    def run(self):
        rot = 0
        last_settings = 0.0
        empties = 0
        while not self._halt.is_set():
            now = _time.time()
            # Poll settings on the TTL, or sooner if waveform reads keep coming back
            # empty (which may mean the handle died) — a dead-handle error here
            # triggers a reconnect; a plain timeout is just the scope being busy.
            if self.s is None or now - last_settings > self.SETTINGS_TTL or empties >= 8:
                try:
                    s = read_settings(self.sc, timeout=LIVE_TIMEOUT_MS, retries=LIVE_RETRIES)
                    with self._lock:
                        self.s = s
                    empties = 0
                except Exception as e:
                    if self._dead(e):
                        self._reconnect(); last_settings = _time.time(); empties = 0; continue
                last_settings = now
            s = self.s
            if s is None:
                self._halt.wait(0.1); continue
            disp = [ch for ch in (0, 1) if s[f'VERT-CH{ch+1}-DISP']]
            la_on = bool(s.get('LA-SWI')) and LA_READ_ENABLED   # raw LA read disabled (corrupts scope)
            with self._lock:                              # forget channels turned off
                for c in list(self.waves):
                    if c not in disp:
                        self.waves.pop(c, None)
                if not la_on:
                    self.la = []
            # Round-robin the enabled analog channels and (if on) the LA block, so
            # each cycle costs one acquire and the refresh rate degrades gracefully.
            items = list(disp) + (['LA'] if la_on else [])
            if not items:
                self._halt.wait(0.1); continue
            item = items[rot % len(items)]; rot += 1
            if item == 'LA':
                try:
                    words = _decode_la(self.sc.read_la(retries=LIVE_RETRIES,
                                                        timeout=LIVE_TIMEOUT_MS))
                except Exception:
                    words = []
                if words:
                    with self._lock:
                        self.la = words
                    empties = 0
                else:
                    empties += 1
            else:
                try:
                    raw = self.sc.read_waveform(item, retries=LIVE_RETRIES,
                                                timeout=LIVE_TIMEOUT_MS)
                except Exception:
                    raw = b''
                y = np.frombuffer(raw, dtype=np.uint8)
                if len(y):
                    with self._lock:
                        self.waves[item] = y
                    empties = 0
                else:
                    empties += 1
            self._halt.wait(POLL_INTERVAL_MS / 1000.0)    # gentle pacing between acquires


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
    reader = Reader(sc); reader.start()
    fig, ax = plt.subplots(figsize=(11, 5)); fig.patch.set_facecolor(BG)
    style_scope(ax, SAMPLES_PER_DIV and 3840 / SAMPLES_PER_DIV or 10)
    lines = [ax.plot([], [], lw=1.0, color=CH_COLORS[ch], label=f"CH{ch+1}",
                     solid_capstyle='round')[0] for ch in (0, 1)]
    la_lines, la_labels = make_la_artists(ax)
    leg = ax.legend(loc='upper right', fontsize=8, facecolor=BG, edgecolor=GRID)
    for t in leg.get_texts(): t.set_color(FG)
    st = {'n': 0}

    def update(_):
        # GUI-thread only: snapshot the latest data from the reader and redraw.
        # No USB here, so this never blocks — the window stays smooth regardless of
        # how slow the scope is.
        if reader.reconnecting:                           # scope rebooted — waiting for it
            ax.set_title("scope disconnected — reconnecting…",
                         color='#e6b400', fontsize=11, pad=10)
            return lines
        s, waves, la = reader.snapshot()
        if s is None:
            return lines                                  # reader hasn't produced yet
        nmax = 0
        for ch, line in enumerate(lines):
            y = waves.get(ch)
            if y is None or not len(y):
                if not s.get(f'VERT-CH{ch+1}-DISP'):
                    line.set_data([], [])                 # channel off — clear it
                continue
            line.set_data(x_divs(len(y)), to_divs(y, s[f'VERT-CH{ch+1}-POS']))
            nmax = max(nmax, len(y))
        nmax = max(nmax, draw_la(la_lines, la_labels, s, la))  # LA rows (if on)
        if nmax:
            ax.set_xlim(0, nmax / SAMPLES_PER_DIV)
        _title(ax, s, nmax or None)
        st['n'] += 1
        if max_frames and st['n'] >= max_frames:
            plt.close(fig)
        return lines + la_lines

    # The GUI can refresh fast now (33 ms ≈ 30 fps) since update() does no I/O.
    anim = FuncAnimation(fig, update, interval=33, blit=False, cache_frame_data=False)
    try:
        plt.show()
    finally:
        del anim
        reader.stop(); reader.join(timeout=2)
        if cap: cap.stop()
        try: reader.sc.close()                            # current handle (may be a reconnect)
        except Exception: pass

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
