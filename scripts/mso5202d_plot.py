#!/usr/bin/env python3
"""
MSO5202D triggered protocol decoder.

This is **not** a live scope. You choose an acquisition depth (4K / 40K / 512K / 1M)
and (optionally) a serial protocol, hit **Trigger & Capture**, and the tool:

  1. arms a **SINGLE-sequence** acquisition at that depth (one full-depth,
     trigger-aligned record; Force-Trig if no edge arrives),
  2. pulls the record off the scope as a front-panel **Save→CSV read back over USB**
     — there is no deep sample stream over USB (docs/MSO5202D-protocol.md §10.7),
  3. plots the captured waveform, and
  4. if a protocol is selected, **thresholds + decodes** it and draws the decoded
     bytes beneath the part of the wave you're looking at (pan/zoom to read them).

No live data is involved — only the trigger, the CSV download, and offline decode.

    python3 mso5202d_plot.py                       # GUI (needs display + python3-tk)
    python3 mso5202d_plot.py --load WaveData.csv --proto uart --png out.png   # headless

UART needs one channel; SPI/I²C need two (SCLK+data / SCL+SDA) — save CH1 then CH2
from the same frozen acquisition so the two files are index-aligned. Decoders live
in serial_decode.py; the CSV parser + deep-capture helpers in mso5202d.py / here.
"""
import argparse
import os
import queue
import threading
import time
import numpy as np
import matplotlib
from mso5202d import SAMPLES_PER_DIV, DIV_UNIT

# --- rendering model (docs/MSO5202D-rendering.md) --------------------------------
# to_divs/x_divs are the scope's byte→division model, kept here because
# serial_decode.threshold() and mso5202d_decode.py import them for the 3840-sample
# SCREEN-buffer decode path. This tool itself works on deep-capture CSVs (volts).
BASELINE_OFFSET = 16             # byte baseline = (VERT-CHx-POS + 16) mod 256
V_DIVS          = 8              # graticule is 8 divisions tall (-4 … +4)
CH_COLORS = ('#e6b400', '#0a84ff')          # CH1 yellow, CH2 blue (like the scope)
GRID, GRID_MINOR, AXIS = '#274427', '#182a19', '#3f6b3f'
BG, FG = '#080a08', '#9fb0a0'

def to_divs(y_bytes, pos):
    """Waveform byte + the channel's VERT-POS → vertical divisions (up = positive).
    Unwraps the sample around the POS-referenced baseline (undoes the 8-bit wrap near
    screen centre and the reversed sense). Used by serial_decode.threshold() and the
    screen-buffer viewer in mso5202d_decode.py."""
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
    minor subdivisions per division, a bold centre line, on a dark face. Kept for
    the screen-buffer viewer in mso5202d_decode.py."""
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

def fmt_rate(hz):
    if not hz: return '?'
    for unit, scale in (('GSa/s', 1e9), ('MSa/s', 1e6), ('kSa/s', 1e3), ('Sa/s', 1)):
        if hz >= scale:
            return f"{hz/scale:g} {unit}"
    return f"{hz:g} Sa/s"

# --- deep capture (the triggered-decoder acquisition) ----------------------------
# Front-panel key ids (0-indexed /keyprotocol.inf; MSO5202D-protocol.md §9/§10.3).
KEY_SR_MENU = 11        # MENU-SR-KEY (Save/Recall)
KEY_SINGLE  = 18        # CT-SINGLESEQ — arm one trigger-aligned acquisition
KEY_RUNSTOP = 19        # CT-RS — Run/Stop toggle
KEY_FORCE   = 47        # TG-FORCE — force trigger
# ACQURIE-STORE-DEPTH codes (mso5202d.ACQ_DEPTH_NAMES): 0=4K 4=40K 6=512K 7=1M(1ch).
DEEP_DEPTHS = [('4K', 0), ('40K', 4), ('512K', 6), ('1M (1-ch)', 7)]
# 2-byte signed settings fields we write in the prep block (little-endian, signed).
_POS_SIGNED = {'VERT-CH1-POS', 'VERT-CH2-POS', 'TRIG-VPOS'}


def _wavedata_num(name):
    import re
    m = re.search(r'(\d+)', name)
    return int(m.group(1)) if m else -1


def _list_wavedata(sh, retries=4):
    """Names of WaveData*.csv currently on the inserted card, via the 0x43 shell. The
    shell `ls` occasionally returns empty/garbled (a one-behind race); since the card in
    use always holds files, retry an empty result so callers get a reliable baseline."""
    for _ in range(retries):
        try:
            out = sh.run("ls -1 /mnt/udisk 2>/dev/null")
        except Exception:
            time.sleep(0.3); continue
        files = {ln.strip() for ln in out.splitlines()
                 if ln.strip().lower().startswith('wavedata')
                 and ln.strip().lower().endswith('.csv')}
        if files:
            return files
        time.sleep(0.3)                    # empty is almost certainly a flaky read — retry
    return set()


def _prep_block(sc, depth_code, setup=True):
    """ONE 0x11 settings-block write — the scope MUST be stopped first (changing store
    depth on a running scope crashes/reboots it — verified 2026-07-10). With `setup`,
    also configure both channels for a clean logic capture: display on, 1× probe, DC
    coupling, invert off, full BW, **1 V/div** (3.3 V logic → ~3.3 divisions, no clip),
    CH1 centred / CH2 −2 div (separated), and Edge/CH1/**Auto** trigger. Without
    `setup`, only the store depth changes (e.g. restoring 4K)."""
    from mso5202d_decode import _field_off, _raw_settings
    block = bytearray(_raw_settings(sc)[1:])           # 213-byte block (drop 0x81 echo)

    def put(name, val):
        off, w = _field_off(name)
        block[off:off + w] = int(val).to_bytes(w, 'little', signed=name in _POS_SIGNED)

    if setup:
        for n in (1, 2):
            put(f'VERT-CH{n}-DISP', 1)                 # channel on
            put(f'VERT-CH{n}-PROBE', 0)                # 1×
            put(f'VERT-CH{n}-COUP', 0)                 # DC
            put(f'VERT-CH{n}-RPHASE', 0)               # invert off
            put(f'VERT-CH{n}-20MHZ', 0)                # full bandwidth
            put(f'VERT-CH{n}-VB', 8)                   # 1 V/div
        put('VERT-CH1-POS', 0)                         # CH1 centred
        put('VERT-CH2-POS', -50)                       # CH2 −2 div (separated, no clip)
        put('TRIG-TYPE', 0); put('TRIG-SRC', 0); put('TRIG-MODE', 0)   # Edge / CH1 / Auto
        # Trigger level ≈ +1.6 V (mid of 3.3 V logic): TRIG-VPOS in 1/25-div,
        # level_V = (VPOS−POS_src)·Vdiv/25 = 40·1000mV/25 = 1.6 V with CH1 POS=0, 1 V/div.
        # Without a level ON the signal, SINGLE arms forever (never crosses) — verified.
        put('TRIG-VPOS', 40)
    put('ACQURIE-STORE-DEPTH', depth_code)
    sc.transact(b'\x11' + bytes(block)); time.sleep(0.5)


def _trig_state(sc):
    from mso5202d_decode import _state
    try: return _state(sc)['TRIG-STATE']
    except Exception: return None


def _run_stop(sc, want_run, tries=8):
    """Press Run/Stop (a toggle) until the scope is running (want_run) or stopped."""
    for _ in range(tries):
        st = _trig_state(sc)
        if (st not in (0, None)) == want_run and st is not None:
            return True
        from mso5202d_decode import _key
        _key(sc, KEY_RUNSTOP); time.sleep(0.35)
    st = _trig_state(sc)
    return st is not None and (st not in (0, None)) == want_run


def trigger_capture(sc, depth_code, status=lambda m: None, setup=True, wait_trig=25):
    """Capture one full-depth, trigger-aligned record via **SINGLE SEQ** — don't manually
    stop. STOP → configure channels + trigger (Edge/CH1/Auto with the level set mid-logic
    so the signal actually crosses it) + depth, all while stopped (a 0x11 depth write on a
    running scope reboots it). Then arm **SINGLE** and let the scope gather data: it
    triggers on the signal edge and stops itself with the record. Poll TRIG-STATE until it
    reaches STOP (0); Force-Trig only as a last-resort fallback. Per hardware guidance
    2026-07-11: a manual RUN→STOP freezes an empty screen; single-seq captures real data."""
    from mso5202d import ACQ_DEPTH_NAMES
    from mso5202d_decode import _key
    status("stopping the scope…"); _run_stop(sc, False)
    status(f"configuring channels + trigger + depth {ACQ_DEPTH_NAMES.get(depth_code, depth_code)}…")
    _prep_block(sc, depth_code, setup)
    status("arming SINGLE — letting the scope gather + trigger on the signal…")
    _key(sc, KEY_SINGLE); time.sleep(0.5)
    # SINGLE captures on the first signal edge; on this firmware TRIG-STATE does not
    # always settle to exactly 0 afterwards, but the record IS captured (verified: full
    # 40K/512K records save correctly). So wait briefly, nudge once with Force if nothing
    # has triggered, then proceed — the acquisition memory holds the record either way.
    t0 = time.time(); forced = False
    while time.time() - t0 < wait_trig:
        if _trig_state(sc) == 0:
            status("triggered — full-depth record captured."); return
        if not forced and time.time() - t0 > 4:
            _key(sc, KEY_FORCE); forced = True          # ensure a trigger even with no edge
        time.sleep(0.6)
    status("record captured (single-seq).")


# Save/Recall → CSV softkey ids (verified 2026-07-10 by screenshotting the menu):
# key 11 opens S/R (Ref=1, SetUp=2, CSV=3); in the CSV menu Source=1 (cycles
# CH1→CH2→LA), Save=2, Recall=3, delete=4 (NEVER press — erases card files),
# FileList=5, Back=6. See MSO5202D-protocol.md §9.
FN_CSV, FN_SAVE, FN_SOURCE = 3, 2, 1


def _menuid(sc):
    """Current CONTROL-MENUID (which on-screen menu is open), or None. Poll this between
    key presses to see the scope's real state — the basis of closed-loop navigation."""
    from mso5202d import decode_settings
    from mso5202d_decode import _raw_settings
    try:
        return decode_settings(_raw_settings(sc)).get('CONTROL-MENUID')
    except Exception:
        return None


def _press_for_menu(sc, keyid, want, status=lambda m: None, tries=4):
    """Press `keyid`, then poll CONTROL-MENUID until it reaches `want` (int/set). Never
    fires blind — verifies the scope actually landed on the expected menu, retrying the
    press if not. Returns True on success."""
    from mso5202d_decode import _key
    wants = set(want) if isinstance(want, (set, tuple, list)) else {want}
    for _ in range(tries):
        _key(sc, keyid); time.sleep(0.35)
        for _ in range(6):
            m = _menuid(sc)
            if m in wants:
                return True
            time.sleep(0.2)
    return _menuid(sc) in wants


def _save_csv(sc, source_cycles=0, status=lambda m: None):
    """CLOSED-LOOP drive of S/R → CSV → (Source ×N) → Save: verify CONTROL-MENUID after
    each menu key so a stray starting screen can't send us into the wrong menu (e.g. into
    SETUP and bumping its Location). First close any open menu (main screen), then S/R
    base (menuid 47) → CSV page (menuid 48) → **Save twice** (1st opens the FileList, 2nd
    writes). `source_cycles`: 0=CH1, 1=CH2, 2=LA. Needs a mounted SD card. Returns True
    if it reached the CSV menu and pressed Save."""
    from mso5202d_decode import _key
    # NOTE: no 0x11 writes here — the vendor app never issues 0x11 during a save, and a
    # 0x11 write appears to disturb the scope's USB/card detection. Reach a clean menu with
    # KEY presses only (MENU-SR switches to the S/R base from wherever we are).
    if not _press_for_menu(sc, KEY_SR_MENU, 47, status):
        status("couldn't reach S/R base (menuid 47) — aborting save"); return False
    if not _press_for_menu(sc, FN_CSV, 48, status):
        status("couldn't reach CSV menu (menuid 48) — aborting save"); return False
    for _ in range(source_cycles):
        _key(sc, FN_SOURCE); time.sleep(0.3)           # cycle Source CH1→CH2→LA
    _key(sc, FN_SAVE); time.sleep(0.8)                 # 1st press: opens the FileList
    _key(sc, FN_SAVE); time.sleep(1.5)                 # 2nd press: writes WaveData<n>.csv
    return True


def _close_menu(sc):
    """Return to the main screen — write CONTROL-DISP-MENU=0 (verified 2026-07-10: hides
    the open side menu, e.g. the CH1 menu the prep write pops up)."""
    from mso5202d_decode import _field_off, _raw_settings
    try:
        od, _ = _field_off('CONTROL-DISP-MENU')
        block = bytearray(_raw_settings(sc)[1:]); block[od] = 0
        sc.transact(b'\x11' + bytes(block)); time.sleep(0.2)
    except Exception:
        pass


def _csv_size(sh, name):
    """Byte size of /mnt/udisk/<name>, or -1 if absent/unreadable (via the 0x43 shell)."""
    try:
        parts = sh.run(f"ls -la /mnt/udisk/{name} 2>/dev/null").split()
        return int(parts[4]) if len(parts) >= 5 else -1
    except Exception:
        return -1


def _wait_new_csv(sh, before, seen, status, hard_timeout):
    """Poll the card for a NEW WaveData*.csv, then wait for its size to STOP changing
    before declaring it done. A deep CSV is generated then written to the (slow) card
    over many seconds, and the file is visible while still growing — so first-appearance
    is not "complete"; a stable size is. Returns the finished filename, or None."""
    t0 = time.time(); target = None
    while time.time() - t0 < hard_timeout:                # (1) wait for the file to appear
        new = sorted(_list_wavedata(sh) - before - seen, key=_wavedata_num)
        if new:
            target = new[-1]; break
        time.sleep(1.0)
    if not target:
        return None
    last = -1; stable = 0                                 # (2) wait for the size to settle
    while time.time() - t0 < hard_timeout:
        sz = _csv_size(sh, target)
        if sz > 0 and sz == last:
            stable += 1
            if stable >= 2:                               # unchanged twice → fully written
                status(f"{target} written ({sz // 1024} KB)")
                return target
        else:
            if sz > 0 and sz != last:
                status(f"writing {target}… {sz // 1024} KB")
            stable = 0; last = sz
        time.sleep(1.0)
    return target                                         # timed out; return best-effort


def deep_capture(sc, depth_code, status=lambda m: None, setup=True,
                 save_sources=(0,), wait_s=None):
    """Trigger + download one deep record. (1) `trigger_capture` freezes a full-depth
    record (stop → prep → RUN→STOP). (2) For each entry in `save_sources` (a Source
    cycle count from CH1: 0=CH1, 1=CH2), drive Save→CSV via the mapped softkeys and
    read the resulting WaveData CSV back over USB (0x10). **Save needs the SD card
    mounted at /mnt/udisk** — it is a silent no-op otherwise. Restores depth to 4K and
    closes the menu afterwards. Returns parsed-CSV dicts in save order (index 0 = CH1).
    Worker-thread only."""
    from mso5202d import parse_wavedata_csv
    from mso5202d_shell import Shell

    # A deep CSV is generated + written to the card on-instrument; the file only appears
    # once complete, and a 512K (~7.7 MB) / 1M (~19 MB) write takes many seconds — so the
    # detection window scales with depth.
    if wait_s is None:
        wait_s = {0: 30, 4: 45, 6: 130, 7: 220}.get(depth_code, 60)
    trigger_capture(sc, depth_code, status=status, setup=setup)
    sh = Shell(scope=sc)                                 # share our USB handle for `ls`
    found = []; seen = set()
    try:
        before = _list_wavedata(sh)
        for cyc in save_sources:
            status(f"Save→CSV (source {'CH1' if cyc == 0 else 'CH'+str(cyc+1)})…")
            _save_csv(sc, cyc, status)
            name = _wait_new_csv(sh, before, seen, status, wait_s)
            if not name:
                status("no CSV appeared — is the SD card inserted? Save needs a mounted disk.")
                continue
            status(f"reading /mnt/udisk/{name} back over USB…")
            try:
                raw = sc.read_file('/mnt/udisk/' + name, timeout=30000)
                r = parse_wavedata_csv(raw); r['file'] = name
                found.append(r); seen.add(name)
                dt = r.get('dt_s')
                status(f"{name}: {r['size']} samples" + (f", {dt*1e9:.1f} ns/sample" if dt else ""))
            except Exception as e:
                status(f"failed to read {name}: {e}"); seen.add(name)
    finally:
        try:                       # back to 4K (stopped) + close the menu (main screen)
            _run_stop(sc, False); _prep_block(sc, 0, setup=False); _close_menu(sc)
        except Exception: pass
        sh.close()
    return found


# --- decode + render (pure; shared by GUI and headless) --------------------------
def decode_capture(results, params):
    """Threshold each channel's volts (threshold_volts) and run the selected decoder.
    `results` are parsed-CSV dicts in save order; channel index = list index (CH1=0,
    CH2=1). Returns (events, dt_seconds_per_sample, used_channel_indices)."""
    from serial_decode import threshold_volts, decode_uart, decode_spi, decode_i2c
    if not results:
        return [], None, []
    dt = results[0].get('dt_s')
    digs = [threshold_volts(r['volts']) for r in results]

    def ch(i):
        return digs[i] if 0 <= i < len(digs) else digs[0]

    proto = params.get('proto', 'none')
    if proto == 'uart':
        line = params.get('line', 0)
        ev = decode_uart(ch(line), sample_interval_ns=(dt * 1e9 if dt else None),
                         baud=params.get('baud'))
        return ev, dt, [line]
    if proto == 'spi':
        clk, data = params.get('clk', 0), params.get('data', 1)
        ev = decode_spi(ch(clk), ch(data), cpol=params.get('cpol', 0), cpha=params.get('cpha', 0))
        return ev, dt, [clk, data]
    if proto == 'i2c':
        scl, sda = params.get('scl', 0), params.get('sda', 1)
        ev = decode_i2c(ch(scl), ch(sda))
        return ev, dt, [scl, sda]
    return [], dt, []


def pick_time_scale(span_s):
    for unit, s in (('s', 1.0), ('ms', 1e-3), ('µs', 1e-6), ('ns', 1e-9)):
        if span_s >= s:
            return unit, s
    return 'ns', 1e-9


def render_wave(ax, results):
    """Plot the captured channels (volts vs time) on a dark axis. Returns
    (x_scale_seconds, x_unit, annotation_y) for the annotation layer."""
    ax.set_facecolor(BG); ax.tick_params(colors=FG, labelsize=7)
    for sp in ax.spines.values(): sp.set_color(GRID)
    ax.grid(True, color=GRID, lw=0.5)
    r0 = results[0]; dt = r0.get('dt_s') or 1e-9
    span = (r0.get('size') or len(r0['volts'])) * dt
    unit, scl = pick_time_scale(span)
    vmin, vmax = 1e9, -1e9
    for i, r in enumerate(results):
        t = r['time_s'] if len(r.get('time_s', [])) else np.arange(len(r['volts'])) * dt
        ax.plot(t / scl, r['volts'], lw=0.6, color=CH_COLORS[i % 2],
                label=f"ch{i} {r.get('file', '')}")
        if len(r['volts']):
            vmin = min(vmin, float(r['volts'].min())); vmax = max(vmax, float(r['volts'].max()))
    if vmin > vmax:
        vmin, vmax = -1.0, 1.0
    rng = max(vmax - vmin, 0.1)
    ax.set_ylim(vmin - 0.28 * rng, vmax + 0.08 * rng)      # headroom below for decode text
    ax.set_xlabel(f"time ({unit})", color=FG); ax.set_ylabel("volts", color=FG)
    leg = ax.legend(fontsize=8, facecolor=BG, edgecolor=GRID, loc='upper right')
    for t in leg.get_texts(): t.set_color(FG)
    return scl, unit, vmin - 0.06 * rng


def render_anno(ax, events, dt, xscale, anno_y, xlim=None, cap=400):
    """Draw decode annotations (a marker + rotated byte text) for events whose start
    falls in `xlim`. Capped at `cap` so a deep capture doesn't draw thousands of texts
    at once — zoom in to read a region. Returns the created artists (to remove later)."""
    arts = []
    if not events or not dt:
        return arts
    if xlim is None:
        xlim = ax.get_xlim()
    x0, x1 = xlim

    def ex(i):
        return i * dt / xscale

    vis = [e for e in events if e['kind'] in ('byte', 'addr', 'start', 'stop')
           and x0 <= ex(e['start']) <= x1]
    if len(vis) > cap:
        arts.append(ax.text(0.5, 0.02, f"{len(vis)} decoded items in view — zoom in to read",
                            transform=ax.transAxes, color='#e6b400', fontsize=9,
                            ha='center', va='bottom'))
        return arts
    for e in vis:
        x = ex(e['start'])
        col = {'start': '#2ec27e', 'stop': '#e01b24'}.get(e['kind'], '#e6b400')
        arts.append(ax.axvline(x, color=col, lw=0.4, alpha=0.35))
        arts.append(ax.text(x, anno_y, e['text'], color=col, fontsize=7, rotation=90,
                            va='top', ha='center', clip_on=True))
    return arts


# --- background worker (no live polling; single-threaded USB) --------------------
class CaptureWorker(threading.Thread):
    """Owns the Scope on its own thread. Drains a command queue — 'capture' (trigger +
    deep CSV download), 'key' (e.g. Force-Trig), 'decode' (pure, on already-captured
    results) — and publishes status/results through an event queue the GUI drains.
    No continuous polling; it only runs on demand, so nothing touches USB unbidden.
    `scope` may be None (offline: only 'decode' works)."""

    def __init__(self, scope):
        super().__init__(daemon=True)
        self.sc = scope
        self._cmds = queue.Queue()
        self.events = queue.Queue()
        self._halt = threading.Event()
        self.busy = False

    def submit(self, kind, **kw):
        self._cmds.put((kind, kw))

    def stop(self):
        self._halt.set()

    def _emit(self, kind, payload=None):
        self.events.put((kind, payload))

    def _status(self, msg):
        self._emit('status', msg)

    def _run_cmd(self, kind, kw):
        self.busy = True; self._emit('busy', True)
        try:
            getattr(self, '_cmd_' + kind)(kw)
        except Exception as e:
            self._status(f"{kind} failed: {e}")
            try: self.sc and self.sc._resync()
            except Exception: pass
        finally:
            self.busy = False; self._emit('busy', False)

    def _cmd_capture(self, kw):
        if not self.sc:
            self._status("no scope connected"); self._emit('capture', []); return
        res = deep_capture(self.sc, kw['depth_code'], status=self._status,
                           setup=kw.get('setup', True), save_sources=kw.get('save_sources', (0,)))
        self._emit('capture', res)

    def _cmd_key(self, kw):
        if not self.sc:
            self._status("no scope connected"); return
        from mso5202d_decode import _key
        _key(self.sc, kw['keyid']); self._status(kw.get('label', 'key sent'))

    def _cmd_decode(self, kw):
        ev, dt, used = decode_capture(kw['results'], kw['params'])
        proto = kw['params'].get('proto', 'none')
        nb = sum(e['kind'] in ('byte', 'addr') for e in ev)
        self._emit('decode', {'events': ev, 'dt': dt, 'used': used, 'proto': proto})
        self._status(f"{proto.upper()}: decoded {nb} bytes" if proto != 'none' else "decode cleared")

    def run(self):
        while not self._halt.is_set():
            try:
                kind, kw = self._cmds.get(timeout=0.2)
            except queue.Empty:
                continue
            self._run_cmd(kind, kw)


# --- GUI -------------------------------------------------------------------------
class DecoderApp:
    """Tk app: an embedded matplotlib canvas showing the captured deep waveform with
    the decode drawn beneath the visible region, plus a side panel (depth, Trigger &
    Capture, protocol + channels, Decode, Load CSV, Save PNG). All scope I/O goes
    through the CaptureWorker; the GUI only drains its event queue."""

    def __init__(self, worker, scope_present):
        import tkinter as tk
        from tkinter import ttk
        matplotlib.use('TkAgg')
        from matplotlib.figure import Figure
        from matplotlib.backends.backend_tkagg import FigureCanvasTkAgg
        self.tk, self.ttk = tk, ttk
        self.worker = worker
        self.scope_present = scope_present
        self.results = []            # parsed-CSV channel dicts
        self.events = []             # decoded events
        self.dt = None               # seconds per sample
        self.proto = 'none'
        self._xscale = 1.0; self._xunit = 's'; self._anno_y = 0.0
        self.anno = []
        self._xlim_cid = None
        self._in_anno = False

        self.root = tk.Tk()
        self.root.title("MSO5202D — triggered protocol decoder")
        self.root.configure(bg=BG)
        self.status = tk.StringVar(
            value="ready — set depth + protocol, then Trigger & Capture"
            if scope_present else "no scope — Load CSV to decode offline")

        self.fig = Figure(figsize=(12, 5.5), facecolor=BG)
        self.ax = self.fig.add_subplot(111)
        self.canvas = FigureCanvasTkAgg(self.fig, master=self.root)
        self.canvas.get_tk_widget().pack(side=tk.LEFT, fill=tk.BOTH, expand=True)

        self._build_panel()
        self._build_statusbar()
        self._plot()
        self.root.protocol("WM_DELETE_WINDOW", self._quit)

    # -- panel ----------------------------------------------------------------
    def _build_panel(self):
        tk, ttk = self.tk, self.ttk
        panel = tk.Frame(self.root, bg=BG)
        panel.pack(side=tk.RIGHT, fill=tk.Y, padx=6, pady=6)
        self.action_btns = []

        def group(title):
            g = ttk.LabelFrame(panel, text=title); g.pack(fill=tk.X, pady=3); return g

        def btn(parent, text, cmd):
            b = ttk.Button(parent, text=text, command=cmd, width=17)
            b.pack(side=tk.TOP, fill=tk.X, pady=1)
            self.action_btns.append(b); return b

        g = group("Capture")
        tk.Label(g, text="depth", bg=BG, fg=FG, font=('TkDefaultFont', 7)).pack(anchor='w')
        self.v_depth = tk.StringVar(value='40K')
        ttk.Combobox(g, textvariable=self.v_depth, state='readonly',
                     values=[d[0] for d in DEEP_DEPTHS], width=12).pack(fill=tk.X)
        self.v_setup = tk.BooleanVar(value=True)
        ttk.Checkbutton(g, text="prep CH1/CH2 (on, 1×, DC, 1 V/div)",
                        variable=self.v_setup).pack(anchor='w')
        btn(g, "▶ Trigger & Capture", self._do_capture)
        btn(g, "Force Trig", lambda: self.worker.submit('key', keyid=KEY_FORCE, label="Force trig"))
        btn(g, "Load CSV…", self._load_csv)

        g = group("Decode")
        self.v_proto = tk.StringVar(value='none')
        cb = ttk.Combobox(g, textvariable=self.v_proto, state='readonly',
                          values=['none', 'uart', 'spi', 'i2c'], width=12)
        cb.pack(fill=tk.X)
        cb.bind('<<ComboboxSelected>>', lambda e: self._do_decode())
        row = tk.Frame(g, bg=BG); row.pack(fill=tk.X, pady=1)
        self.v_chA = tk.StringVar(value='CH1'); self.v_chB = tk.StringVar(value='CH2')
        tk.Label(row, text="A", bg=BG, fg=FG, font=('TkDefaultFont', 7)).pack(side=tk.LEFT)
        ttk.Combobox(row, textvariable=self.v_chA, state='readonly',
                     values=['CH1', 'CH2'], width=4).pack(side=tk.LEFT)
        tk.Label(row, text="B", bg=BG, fg=FG, font=('TkDefaultFont', 7)).pack(side=tk.LEFT)
        ttk.Combobox(row, textvariable=self.v_chB, state='readonly',
                     values=['CH1', 'CH2'], width=4).pack(side=tk.LEFT)
        er = tk.Frame(g, bg=BG); er.pack(fill=tk.X)
        tk.Label(er, text="baud/mode", bg=BG, fg=FG, font=('TkDefaultFont', 7)).pack(side=tk.LEFT)
        self.v_extra = tk.StringVar(value='')
        ttk.Entry(er, textvariable=self.v_extra, width=8).pack(side=tk.LEFT)
        tk.Label(g, text="A=UART line / SPI SCLK / I²C SCL\nB=SPI data / I²C SDA\n"
                         "ch index = save order (CH1 then CH2)",
                 bg=BG, fg=FG, font=('TkDefaultFont', 7), justify='left').pack(anchor='w')
        btn(g, "Decode", self._do_decode)

        g = group("Output")
        btn(g, "Save PNG…", self._save_png)

    def _build_statusbar(self):
        tk = self.tk
        from matplotlib.backends.backend_tkagg import NavigationToolbar2Tk
        bar = tk.Frame(self.root, bg=BG); bar.pack(side=tk.BOTTOM, fill=tk.X)
        tbf = tk.Frame(bar, bg=BG); tbf.pack(side=tk.LEFT)
        NavigationToolbar2Tk(self.canvas, tbf).update()    # pan / zoom / save
        tk.Label(bar, textvariable=self.status, bg=BG, fg=FG, anchor='w'
                 ).pack(side=tk.LEFT, fill=tk.X, expand=True, padx=8)

    # -- handlers -------------------------------------------------------------
    def _decode_params(self):
        proto = self.v_proto.get()
        a = 0 if self.v_chA.get() == 'CH1' else 1
        b = 0 if self.v_chB.get() == 'CH1' else 1
        extra = self.v_extra.get().strip()
        p = {'proto': proto}
        if proto == 'uart':
            p['line'] = a
            p['baud'] = float(extra) if extra else None
        elif proto == 'spi':
            p['clk'] = a; p['data'] = b
            mode = extra or '00'
            p['cpol'] = int(mode[0]) if mode[:1] in '01' else 0
            p['cpha'] = int(mode[1]) if len(mode) > 1 and mode[1] in '01' else 0
        elif proto == 'i2c':
            p['scl'] = a; p['sda'] = b
        return p

    def _do_capture(self):
        if not self.scope_present:
            self.status.set("no scope connected — use Load CSV instead"); return
        code = dict(DEEP_DEPTHS)[self.v_depth.get()]
        proto = self.v_proto.get()
        a = 0 if self.v_chA.get() == 'CH1' else 1
        b = 0 if self.v_chB.get() == 'CH1' else 1
        # Source-cycle counts to Save→CSV (0=CH1, 1=CH2): the channel(s) the decode needs.
        if proto == 'uart':
            sources = (a,)
        elif proto in ('spi', 'i2c'):
            sources = tuple(dict.fromkeys([a, b]))          # unique, in order
        else:
            sources = (0,)                                  # no protocol → just grab CH1
        self.status.set("capturing…")
        self.worker.submit('capture', depth_code=code, setup=self.v_setup.get(),
                           save_sources=sources)

    def _load_csv(self):
        from tkinter import filedialog
        from mso5202d import parse_wavedata_csv
        paths = filedialog.askopenfilenames(
            title="Load WaveData CSV (select CH1 first, then CH2 for SPI/I²C)",
            filetypes=[('CSV', '*.csv'), ('all', '*')])
        if not paths:
            return
        res = []
        for p in paths:
            try:
                r = parse_wavedata_csv(open(p, 'rb').read()); r['file'] = os.path.basename(p); res.append(r)
            except Exception as e:
                self.status.set(f"failed to load {os.path.basename(p)}: {e}")
        if res:
            self.results = res; self.events = []; self.dt = res[0].get('dt_s')
            self._plot()
            self.status.set(f"loaded {len(res)} file(s), {res[0].get('size')} samples")
            if self.v_proto.get() != 'none':
                self._do_decode()

    def _do_decode(self):
        self.proto = self.v_proto.get()
        if not self.results:
            self.status.set("no capture to decode — Trigger & Capture or Load CSV"); return
        if self.proto == 'none':
            self.events = []; self._update_decode(); self.status.set("decode cleared"); return
        self.status.set("decoding…")
        self.worker.submit('decode', results=self.results, params=self._decode_params())

    def _save_png(self):
        from tkinter import filedialog
        path = filedialog.asksaveasfilename(defaultextension='.png', filetypes=[('PNG', '*.png')])
        if path:
            self.fig.savefig(path, dpi=110, facecolor=BG)
            self.status.set(f"saved {path}")

    def _set_busy(self, busy):
        st = 'disabled' if busy else 'normal'
        for b in self.action_btns:
            try: b.configure(state=st)
            except Exception: pass

    # -- drawing --------------------------------------------------------------
    def _title(self):
        r0 = self.results[0]; dt = self.dt or r0.get('dt_s'); rate = (1 / dt) if dt else None
        nb = sum(e['kind'] in ('byte', 'addr') for e in self.events)
        self.ax.set_title(
            f"{r0.get('size')} samples @ {fmt_rate(rate)}  ·  "
            + (f"{self.proto.upper()}: {nb} bytes  (pan/zoom to read the decode)"
               if self.events else "no decode"), color=FG, fontsize=9)

    def _plot(self):
        self.ax.clear(); self.anno = []
        if not self.results:
            self.ax.set_facecolor(BG); self.ax.tick_params(colors=FG, labelsize=7)
            for sp in self.ax.spines.values(): sp.set_color(GRID)
            self.ax.grid(True, color=GRID, lw=0.5)
            self.ax.set_title("no capture — Trigger & Capture (or Load CSV…)", color=FG, fontsize=10)
            self.ax.set_xlabel("time", color=FG); self.ax.set_ylabel("volts", color=FG)
            self.canvas.draw_idle(); return
        self._xscale, self._xunit, self._anno_y = render_wave(self.ax, self.results)
        self._title()
        if self._xlim_cid is not None:
            try: self.ax.callbacks.disconnect(self._xlim_cid)
            except Exception: pass
        self._xlim_cid = self.ax.callbacks.connect('xlim_changed', self._on_xlim)
        self.anno = render_anno(self.ax, self.events, self.dt, self._xscale, self._anno_y)
        self.canvas.draw_idle()

    def _redraw_anno(self):
        for a in self.anno:
            try: a.remove()
            except Exception: pass
        self.anno = render_anno(self.ax, self.events, self.dt, self._xscale,
                                self._anno_y, self.ax.get_xlim())

    def _update_decode(self):
        """Refresh the title + annotations without re-plotting the (large) waveform."""
        if self.results:
            self._title()
        self._redraw_anno(); self.canvas.draw_idle()

    def _on_xlim(self, ax):
        if self._in_anno:
            return
        self._in_anno = True
        try:
            self._redraw_anno(); self.canvas.draw_idle()
        finally:
            self._in_anno = False

    # -- event loop -----------------------------------------------------------
    def _drain(self):
        try:
            while True:
                kind, payload = self.worker.events.get_nowait()
                if kind == 'status':
                    self.status.set(payload)
                elif kind == 'busy':
                    self._set_busy(payload)
                elif kind == 'capture':
                    self.results = payload or []
                    self.events = []
                    self.dt = self.results[0].get('dt_s') if self.results else None
                    self._plot()
                    if self.results and self.v_proto.get() != 'none':
                        self._do_decode()
                    elif not self.results:
                        self.status.set("capture: no CSV retrieved — save Save→CSV on the scope, retry")
                elif kind == 'decode':
                    self.events = payload['events']; self.dt = payload['dt']; self.proto = payload['proto']
                    self._update_decode()
        except queue.Empty:
            pass

    def _tick(self):
        self._drain()
        self.root.after(120, self._tick)

    def run(self):
        self.root.after(50, self._tick)
        self.root.mainloop()

    def _quit(self):
        try: self.root.quit(); self.root.destroy()
        except Exception: pass


def run_gui():
    from mso5202d import Scope
    try:
        # reset=False is CRITICAL for deep capture: a USB dev.reset() disturbs the
        # scope's USB host controller (which also hosts the SD card), dropping the card
        # so Save→CSV fails with "USB device undetected" (verified 2026-07-10 against the
        # vendor app, which never resets). No reset → the card stays detected.
        sc = Scope(reset=False); present = True
    except Exception as e:
        print(f"[!] no scope ({e}) — offline mode: Load CSV to decode.")
        sc = None; present = False
    worker = CaptureWorker(sc); worker.start()
    try:
        DecoderApp(worker, present).run()
    finally:
        worker.stop()
        try: worker.join(timeout=2)
        except Exception: pass
        if sc:
            try: sc.close()
            except Exception: pass


def run_headless(paths, params, png):
    """Load WaveData CSV file(s), decode, and save a labelled PNG — no scope needed.
    Great for testing against scope_dump/WaveData*.csv."""
    matplotlib.use('Agg')
    import matplotlib.pyplot as plt
    from mso5202d import parse_wavedata_csv
    results = []
    for p in paths:
        r = parse_wavedata_csv(open(p, 'rb').read()); r['file'] = os.path.basename(p); results.append(r)
    if params.get('proto', 'none') != 'none':
        events, dt, _ = decode_capture(results, params)
    else:
        events, dt = [], results[0].get('dt_s')
    fig, ax = plt.subplots(figsize=(12, 5.5)); fig.patch.set_facecolor(BG)
    scl, unit, anno_y = render_wave(ax, results)
    render_anno(ax, events, dt, scl, anno_y, xlim=ax.get_xlim(), cap=100000)
    nb = sum(e['kind'] in ('byte', 'addr') for e in events)
    r0 = results[0]; rate = (1 / dt) if dt else None
    ax.set_title(f"{r0.get('size')} samples @ {fmt_rate(rate)}  ·  "
                 + (f"{params['proto'].upper()}: {nb} bytes" if events else "no decode"),
                 color=FG, fontsize=9)
    fig.tight_layout(); fig.savefig(png, dpi=110, facecolor=BG)
    print(f"[+] {len(results)} channel(s), {r0.get('size')} samples; "
          f"{params.get('proto', 'none').upper()} → {nb} bytes; saved {png}")
    if events:
        vals = [e['text'] for e in events if e['kind'] in ('byte', 'addr')]
        print("    " + ' '.join(vals[:64]) + (" …" if len(vals) > 64 else ""))


def main():
    ap = argparse.ArgumentParser(description="MSO5202D triggered protocol decoder")
    ap.add_argument('--load', nargs='+', metavar='CSV',
                    help="headless: decode WaveData CSV file(s) (CH1 [CH2]) → PNG")
    ap.add_argument('--proto', choices=['none', 'uart', 'spi', 'i2c'], default='none')
    ap.add_argument('--baud', type=float, default=None, help="UART baud (default auto)")
    ap.add_argument('--mode', default='00', help="SPI cpol/cpha, e.g. 00")
    ap.add_argument('--png', metavar='PATH', help="output PNG for --load (default decode.png)")
    a = ap.parse_args()
    if a.load:
        params = {'proto': a.proto}
        if a.proto == 'uart':
            params.update(line=0, baud=a.baud)
        elif a.proto == 'spi':
            params.update(clk=0, data=1, cpol=int(a.mode[0]),
                          cpha=int(a.mode[1]) if len(a.mode) > 1 else 0)
        elif a.proto == 'i2c':
            params.update(scl=0, sda=1)
        run_headless(a.load, params, a.png or 'decode.png')
    else:
        run_gui()


if __name__ == '__main__':
    main()
