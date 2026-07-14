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
LA_COLOR = '#2ec27e'                         # LA digital rows (green)
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
KEY_ACQUIRE = 13        # MENU-ACQU — Acquire menu (menuid 17; F5 cycles LongMem depth)
KEY_CH1_MENU = 24       # VT-CH1-MENU — CH1 button (toggles CH1 on/off + lights its LED)
KEY_CH2_MENU = 30       # VT-CH2-MENU — CH2 button (toggles CH2 on/off + lights its LED)
KEY_SINGLE  = 18        # CT-SINGLESEQ — arm one trigger-aligned acquisition
KEY_RUNSTOP = 19        # CT-RS — Run/Stop toggle
KEY_DEFAULT_SETUP = 21  # CT-DS — factory Default Setup (idempotent known state)
KEY_FORCE   = 47        # TG-FORCE — force trigger
MENU_ACQUIRE = 17       # CONTROL-MENUID when the Acquire menu is open
FN_LONGMEM = 5          # Acquire-menu softkey F5 — cycles store depth 4K→40K→512K→1M→(4K)
# ACQURIE-STORE-DEPTH codes (mso5202d.ACQ_DEPTH_NAMES): 0=4K 4=40K 6=512K 7=1M(1ch).
DEEP_DEPTHS = [('4K', 0), ('40K', 4), ('512K', 6), ('1M (1-ch)', 7)]
# Store-depth ring the Acquire-menu F5 softkey walks, in cycle order (codes 0→4→6→7→0). F5
# advances one step per key EDGE (press and release each advance) — driven via single alternating
# edges + poll-until-change in `_set_depth_via_keys`, no render delay needed.
_DEPTH_RING = [0, 4, 6, 7]
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


def _prep_block(sc, depth_code, setup=True, la=False, tb_idx=None, set_depth=True):
    """ONE 0x11 settings-block write — the scope MUST be stopped first (changing store
    depth on a running scope crashes/reboots it — verified 2026-07-10). With `setup`,
    configure both channels' vertical params for a clean logic capture: 1× probe, DC
    coupling, invert off, full BW, **1 V/div** (3.3 V logic → ~3.3 divisions, no clip),
    CH1 centred / CH2 −2 div (separated), and Edge/CH1/**Auto** trigger. **Channel on/off
    (VERT-CHx-DISP) is NOT set here** — a 0x11 DISP write changes the field but does not light
    the channel's front-panel LED, so `prepare_capture` turns CH1/CH2 on/off via their MENU
    buttons instead (`_set_channels_via_keys`). `la` turns the logic pod on (all 16 channels) —
    but **LA forces the depth to 4K** (deep memory is not available with LA, verified 2026-07-11).
    `tb_idx` (0..31) sets the horizontal timebase (HORIZ-WIN-TB = idx, HORIZ-TB = max(idx, 6)) —
    used to spread a deep record over more time. Without `setup`, only depth (+LA, +tb) change.
    `set_depth=False` leaves the store-depth byte untouched — used when the depth is set via the
    Acquire-menu F5 key instead (which also updates the on-screen LongMem radio; a bare 0x11
    depth write does not)."""
    from mso5202d_decode import _field_off, _raw_settings
    original = bytes(_raw_settings(sc)[1:])            # 213-byte block (drop 0x81 echo)
    block = bytearray(original)

    def put(name, val):
        off, w = _field_off(name)
        block[off:off + w] = int(val).to_bytes(w, 'little', signed=name in _POS_SIGNED)

    if setup:
        for n in (1, 2):                               # configure both channels' vertical params
            put(f'VERT-CH{n}-PROBE', 0)                # 1×
            put(f'VERT-CH{n}-COUP', 0)                 # DC
            put(f'VERT-CH{n}-RPHASE', 0)               # invert off
            put(f'VERT-CH{n}-20MHZ', 0)                # full bandwidth
            put(f'VERT-CH{n}-VB', 8)                   # 1 V/div
        put('VERT-CH1-POS', 0)                         # CH1 centred
        put('VERT-CH2-POS', -50)                       # CH2 −2 div (separated, no clip)
        # Edge trigger on CH1, Auto. Trigger level ≈ +1.6 V (mid of 3.3 V logic): TRIG-VPOS in
        # 1/25-div, level_V = (VPOS−POS_src)·Vdiv/25 = 40·1000mV/25 = 1.6 V with CH1 POS=0. Without
        # a level ON the signal, SINGLE arms forever (never crosses) — verified. (TRIG-SRC is not
        # writable via 0x11 anyway — verified 2026-07-11 — so it stays on CH1/the DS default.)
        put('TRIG-TYPE', 0); put('TRIG-SRC', 0); put('TRIG-MODE', 0)
        put('TRIG-VPOS', 40)
    if la:
        put('LA-SWI', 1); put('LA-CHANNEL-STATE', 0xFFFF)   # pod on, all 16 channels
    elif setup:
        put('LA-SWI', 0)                                     # pure-analog capture → LA off
    if set_depth:
        put('ACQURIE-STORE-DEPTH', depth_code)
    if tb_idx is not None:                              # spread the record over more/less time
        put('HORIZ-WIN-TB', int(tb_idx)); put('HORIZ-TB', max(int(tb_idx), 6))
    # Only write if something actually changed. A 0x11 write makes the scope busy ~3.4 s
    # while it reapplies the whole 213-byte block (the next read blocks until it's done), so
    # skipping a no-op write is the single biggest capture speed-up — on a repeat capture the
    # scope is already configured and prep becomes free (state-based, not a blind re-write).
    if bytes(block) != original:
        sc.transact(b'\x11' + bytes(block)); time.sleep(0.5)


def _default_setup(sc, status=lambda m: None):
    """Factory **Default Setup** (keyid 21) → reset the scope to a KNOWN state so a capture
    never depends on how the panel was left (idempotent). Verified 2026-07-11: card-safe
    (a Save right after DS writes a file) and it resets the CSV **Source to CH1** — so a deep
    multi-channel save then cycles CH1→CH2 deterministically (not from an unknown Source). DS
    also turns CH2 off / depth 4K / a default timebase, which the subsequent `_prep_block`
    re-configures. Closed-loop: press until CONTROL-MENUID == 25 (the DefaultSetup menu),
    then let the reset settle. Returns True if it landed."""
    status("Default Setup — resetting the scope to a known state…")
    ok = _press_for_menu(sc, KEY_DEFAULT_SETUP, 25, status)
    time.sleep(1.5)                                    # let the whole reset apply
    return ok


def _depth_now(sc):
    """Read ACQURIE-STORE-DEPTH. **Reliable only at 4K/40K** — at 512K/1M the field lies while the
    deep record loads (it reads transient 4K / wrong codes), so the depth-set walk does NOT trust it
    at deep; it's informational there."""
    from mso5202d import decode_settings
    from mso5202d_decode import _raw_settings
    try: return decode_settings(_raw_settings(sc)).get('ACQURIE-STORE-DEPTH')
    except Exception:
        sc._resync(); return None


def _set_depth_via_keys(sc, depth_code, status=lambda m: None, timeout=10):
    """Set the store depth via the Acquire-menu F5 softkey (keyid 5) — the scope stays **running
    the whole time** (we never stop; the only stop is the capture single-seq). Why the menu key,
    not a 0x11 depth write: a 0x11 write updates the field + acquisition but NOT the on-screen
    LongMem radio (stays stale at 4K), and a 0x11 depth change on a *running* scope reboots it. F5
    walks the same visible menu, so the display matches, and it's safe while running.

    F5 advances the ring 4K→40K→512K→1M→(4K) (codes 0→4→6→7) **one step per key EDGE** — the press
    (`0x13 05 01`) and the release (`0x13 05 00`) EACH advance one step (verified 2026-07-11; a
    press+release tap 100 ms apart merges into one click/one step, but if the two edges get
    stretched apart in time they count as two → the old intermittent 'depth skip'). So drive it with
    **single alternating edges** and, after each, **poll `ACQURIE-STORE-DEPTH` until it reaches the
    next step** — no fixed render delay; the field settles to the new value within ~1–2 s and the
    poll catches it (4K→40K→512K→1M in ~4 s total). **Self-correcting**: if an edge causes no change
    (F5 was already at that level from a prior call), flip the edge and resend. From the DS-known 4K
    start it takes exactly the ring distance. Returns True once the field reaches the target. 1M is
    single-channel (the DS baseline CH1-only satisfies that)."""
    from mso5202d import ACQ_DEPTH_NAMES
    from mso5202d_decode import _key
    if depth_code not in _DEPTH_RING:
        status(f"  depth {depth_code} not on the F5 ring — leaving depth as-is"); return False
    _key(sc, KEY_ACQUIRE); time.sleep(0.8)                 # one press opens Acquire (menuid 17)
    if _menuid(sc) != MENU_ACQUIRE:
        _key(sc, KEY_ACQUIRE); time.sleep(0.8)             # one retry (single-slot mailbox can drop)
    cur = _depth_now(sc)
    at = _DEPTH_RING.index(cur) if cur in _DEPTH_RING else 0
    edge = 0x01                                            # first F5 edge = press; then alternate
    while _DEPTH_RING[at] != depth_code:
        nxt = _DEPTH_RING[(at + 1) % len(_DEPTH_RING)]
        landed = False
        for _ in range(2):                                 # self-correct the level if an edge no-ops
            sc.transact(bytes([0x13, FN_LONGMEM, edge]))
            t0 = time.time()
            while time.time() - t0 < timeout:              # poll until the depth advances one step
                if _depth_now(sc) == nxt:
                    landed = True; break
                time.sleep(0.2)
            edge ^= 0x01                                    # the next edge is the opposite level
            if landed:
                break
        status(f"  F5 → {ACQ_DEPTH_NAMES.get(nxt)}{'' if landed else ' (timeout)'}")
        if not landed:
            return False
        at = (at + 1) % len(_DEPTH_RING)
    return True


def _channel_enabled(sc, ch):
    """True if analog channel `ch` (0=CH1, 1=CH2) is enabled, checked the only reliable way — its
    **4K wave data** (the user's method; `VERT-CHx-DISP` is decoupled and can't be trusted). A
    disabled channel's `0x02` acquire returns EMPTY; an enabled one returns ~3200 samples. Double-
    read to defeat the one-deep `0x02` channel pipeline (the first read after a channel switch
    returns the PREVIOUS channel). None on error. Only meaningful at 4K with the scope running."""
    try:
        sc.read_waveform(ch, retries=1, timeout=1500)      # discard — pipeline returns prev channel
        return bool(sc.read_waveform(ch, retries=1, timeout=1500))
    except Exception:
        sc._resync(); return None


def _set_channels_via_keys(sc, channels, status=lambda m: None, tries=3):
    """Enable/disable CH1/CH2 via their front-panel MENU buttons (keyid 24/30). Each button is a
    **toggle**: one `0x13 keyid` frame flips the channel shown↔hidden (the 2nd byte is a don't-care).
    Buttons, not a 0x11 `VERT-CHx-DISP` write, so the LED lights *and* the acquisition actually
    serves the channel. **Closed-loop on 4K wave data** (`_channel_enabled`) — read the state and
    send the toggle only when it does not match the target, re-checking after each press (~1 s
    button-to-state latency). Run at 4K, scope running, before the depth walk (1M needs CH2 off
    first)."""
    keys = {1: KEY_CH1_MENU, 2: KEY_CH2_MENU}
    want = set(channels)
    for n in (1, 2):
        want_on = n in want
        for _ in range(tries):
            if _channel_enabled(sc, n - 1) == want_on:
                break
            sc.transact(bytes([0x13, keys[n], 0x01]))      # toggle CHn on/off (one flip per frame)
            time.sleep(1.0)                                # button-to-state latency ~0.5–1 s
        state = _channel_enabled(sc, n - 1)
        status(f"  CH{n} {'on' if want_on else 'off'}"
               + ("" if state == want_on else " (UNVERIFIED — no wave-data match)"))


# TRIG-STATE values where the scope is STOPPED with a frozen record (RUN/STOP button red):
# 0 = STOP (manual), 5 = SINGLE (single-seq captured + stopped). **5 is NOT "armed"** — a
# single-seq that has captured sits at 5 with the button red (verified 2026-07-11 by watching
# the settings while saving a 512K record: state stayed SINGLE the whole time the button was
# red and the saves succeeded). Armed-waiting is 1 (WAIT) / 6 (re-arm); running is 2/3/4.
_STOPPED_STATES = (0, 5)


def _trig_state(sc):
    from mso5202d_decode import _state
    try: return _state(sc)['TRIG-STATE']
    except Exception: return None


def _is_stopped(sc):
    st = _trig_state(sc)
    return st is not None and st in _STOPPED_STATES


def _run_stop(sc, want_run, tries=8):
    """Press Run/Stop (a toggle) until the scope is running (want_run) or stopped. Treats a
    captured single-seq (state 5) as STOPPED — otherwise a stop request on a single-seq record
    would toggle the scope back into RUN (that was the '512K scope kept running' bug)."""
    for _ in range(tries):
        st = _trig_state(sc)
        if st is None:
            from mso5202d_decode import _key
            _key(sc, KEY_RUNSTOP); time.sleep(0.35); continue
        is_running = st not in _STOPPED_STATES
        if is_running == want_run:
            return True
        from mso5202d_decode import _key
        _key(sc, KEY_RUNSTOP); time.sleep(0.35)
    return _is_stopped(sc) != want_run


def prepare_capture(sc, depth_code, status=lambda m: None, setup=True, channels=(1, 2), la=False,
                    reset=True, auto_tb=False):
    """**Prepare** the scope for capture — the slow, idempotent SETUP half (run once). The scope
    stays **RUNNING the whole time** — we never stop it here; the only stop is the capture
    single-seq. Default Setup (if `reset`) → a known state (CSV Source = CH1, depth 4K) →
    configure channels + trigger + timebase (one 0x11 write, depth byte left alone so it can't
    reboot a running scope) → set the store depth by walking the Acquire-menu F5 softkey (which
    the scope handles cleanly while running, and which updates the on-screen LongMem radio too).
    `auto_tb` (deep only) briefly freezes the live signal at 4K to measure it and pick a slower
    timebase (more bit-periods per deep record), then resumes running. Leaves the scope running
    and ready; call `capture_prepared()` (or press Capture). Returns the chosen tb_idx (or None)."""
    from mso5202d import ACQ_DEPTH_NAMES, TB_TO_NS
    if reset:
        _default_setup(sc, status)                     # known state → idempotent capture (4K)
    tb_idx = None
    if auto_tb and depth_code != 0 and not la:
        status("probing the signal to pick a timebase…")
        pulse = _probe_pulse_ns(sc, status)            # transient 4K freeze to measure, then resumes
        if pulse:
            tb_idx = _pick_deep_tb(pulse, _DEEP_SAMPLES.get(depth_code, 40064))
            status(f"finest pulse ≈ {pulse/1000:.1f} µs → timebase {TB_TO_NS[tb_idx]/1e3:.0f} "
                   f"µs/div (record ≈ {19.2*TB_TO_NS[tb_idx]/1e6:.0f} ms)")
        else:
            status("no signal to probe — keeping the current timebase")
        _run_stop(sc, True)                            # the probe froze the scope — resume RUN
    status(f"configuring channels + trigger + depth {ACQ_DEPTH_NAMES.get(depth_code, depth_code)}"
           + (" + LA pod" if la else "") + "…")
    # Configure channels/trigger/timebase with ONE 0x11 block write, **set_depth=False so the depth
    # byte is untouched** — a 0x11 depth *change* on a running scope reboots it, but a write that
    # leaves the depth alone is safe while running (verified 2026-07-11). The store depth is then
    # set below by cycling the Acquire-menu F5 softkey: it's safe while running, and unlike a 0x11
    # depth write it also updates the on-screen LongMem radio (a 0x11 write leaves it stale at 4K).
    # LA forces the depth to 4K (the pod clamps deep memory), so LA needs no F5 cycling — after DS
    # the depth is already 4K and the pod holds it there.
    _prep_block(sc, depth_code, setup, la=la, tb_idx=tb_idx, set_depth=False)
    # Channel ON/OFF via the CH1/CH2 buttons (LED-accurate, unlike a 0x11 DISP write), starting from
    # the Default-Setup baseline (CH1 on, CH2 off). Must come BEFORE the depth F5 walk: 1M is
    # single-channel-only, so CH2 has to be off before 1M is even reachable. Needs `reset` (the DS
    # gives the known baseline); without it we leave the channels as the panel had them.
    if setup and not la and reset:
        status("setting channels via the CH1/CH2 buttons…")
        _set_channels_via_keys(sc, channels, status)
    ok = True
    if not la and depth_code != 0:
        status(f"setting store depth {ACQ_DEPTH_NAMES.get(depth_code)} via the Acquire menu (F5)…")
        ok = False
        for _ in range(3):                             # retry the whole menu walk if it can't land
            if _set_depth_via_keys(sc, depth_code, status):
                ok = True; break
            sc._resync()
    # Trust the F5 walk's own post-tap confirmation — do NOT re-poll the depth here: a deep record
    # is still populating and the field reads a transient 4K the whole time it loads (re-reading it
    # would falsely report '4K'). `_set_depth_via_keys` already verified the target in the reliable
    # window right after the tap.
    status(f"depth {'confirmed' if ok else 'NOT confirmed'}: "
           f"{ACQ_DEPTH_NAMES.get(0 if la else depth_code)}"
           + ("  (LA clamps deep memory to 4K)" if la and depth_code else ""))
    status("ready (running) — press Capture to grab a single-seq record")
    return tb_idx


def _trigger_record(sc, use_single, status=lambda m: None, wait_trig=25):
    """Grab ONE record from an already-prepared + stopped scope (the fast half). No reset/prep.

    * `use_single=True` (deep): arm **SINGLE SEQ** — the scope triggers on the signal edge and
      stops itself with a full-depth, trigger-aligned record; Force-Trig only as a last resort.
    * `use_single=False` (4K live serial): **RUN → STOP freeze** — fills the window with the
      continuous stream and freezes ONE simultaneous 2-channel acquisition (aligned CH1/CH2)."""
    from mso5202d_decode import _key
    if not use_single:
        status("capturing live signal (run → stop freeze)…")
        _freeze(sc)
        status("record captured (freeze).")
        return
    status("arming SINGLE — triggering on the signal…")
    _key(sc, KEY_SINGLE); time.sleep(0.5)
    # A single-seq captures on the first edge and **stops itself with the record** — the RUN/STOP
    # button goes red and TRIG-STATE reads 5 (SINGLE) or 0 (STOP). Wait for one of those. If it
    # stays armed (WAIT/re-arm) with no edge, nudge once with Force. **Do NOT press Run/Stop
    # afterwards** — it has already stopped; a toggle would start it running (the '512K kept
    # running / only CH1' bug). Verified against the manual save flow 2026-07-11.
    t0 = time.time(); forced = False; captured = False
    while time.time() - t0 < wait_trig:
        st = _trig_state(sc)
        if st in _STOPPED_STATES:                       # captured + stopped (button red)
            captured = True; break
        if not forced and time.time() - t0 > 4:
            _key(sc, KEY_FORCE); forced = True          # no edge yet — force one
        time.sleep(0.4)
    status("record captured (single-seq)." if captured
           else "record may be incomplete (single-seq didn't confirm stop).")


def trigger_capture(sc, depth_code, status=lambda m: None, setup=True, channels=(1, 2), la=False,
                    wait_trig=25, use_single=True, tb_idx=None, reset=True):
    """Backward-compatible one-shot = prepare + trigger (used by tests/headless). The GUI
    splits these into a Prepare button and a Capture button instead."""
    prepare_capture(sc, depth_code, status, setup=setup, channels=channels, la=la, reset=reset)
    if tb_idx is not None:                               # explicit tb override (tests)
        _prep_block(sc, depth_code, setup, la=la, tb_idx=tb_idx)
    _trigger_record(sc, use_single, status, wait_trig)


# Save/Recall → CSV softkey ids (verified 2026-07-10 by screenshotting the menu):
# key 11 opens S/R (Ref=1, SetUp=2, CSV=3); in the CSV menu Source=1 (cycles
# CH1→CH2→LA), Save=2, Recall=3, delete=4 (NEVER press — erases card files),
# FileList=5, Back=6. See MSO5202D-protocol.md §9.
FN_CSV, FN_SAVE, FN_SOURCE, FN_DELETE, FN_BACK = 3, 2, 1, 4, 6


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


def _close_menu(sc):
    """Return to the main screen: press Back (close any FileList/back out of the submenu) then
    write CONTROL-DISP-MENU = 0 to hide the side menu. **Call this only while the scope is
    RUNNING** — a full-block 0x11 write while STOPPED (single-seq state 5) crash-reboots it,
    but the same write while running is fine (verified 2026-07-11). Best-effort."""
    from mso5202d_decode import _key, _field_off, _raw_settings
    try:
        _key(sc, FN_BACK); time.sleep(0.35)              # close the FileList / back out one level
        off, _ = _field_off('CONTROL-DISP-MENU')
        block = bytearray(_raw_settings(sc)[1:]); block[off] = 0
        sc.transact(b'\x11' + bytes(block)); time.sleep(0.4)
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


# --- CSV Source selection (framebuffer-verified) ---------------------------------
# The CSV menu's Source radio (CH1/CH2/LA) is NOT in the settings blob, so read which is
# selected off the 0x20 framebuffer: the selected row is orange (coords _SRC_ROW_Y /
# _SRC_DOT_X below, calibrated from screenshots 2026-07-11).
_SRC_NAMES = ('CH1', 'CH2', 'LA')


def _grab_fb(sc):
    """Grab the 800×480 framebuffer as an (H,W,3) uint8 array, or None. Resync-retries."""
    for _ in range(4):
        try:
            frame = sc.transact(b'\x20', timeout=4000)
        except Exception:
            sc._resync(); time.sleep(0.4); continue
        data = bytearray()
        for _ in range(2000):
            st = frame[1] if len(frame) > 1 else 0xFF
            if st == 0x01: data += frame[2:]
            elif st == 0x02: break
            try: frame = sc._recv(3000)
            except Exception: break
        if len(data) >= 800 * 480 * 2:
            px = np.frombuffer(bytes(data[:800*480*2]), dtype='<u2').reshape(480, 800)
            r = ((px >> 11) & 0x1f) << 3; g = ((px >> 5) & 0x3f) << 2; b = (px & 0x1f) << 3
            sc._resync()                     # clear any framebuffer tail so the next key is clean
            return np.dstack([r, g, b]).astype(np.uint8)
        sc._resync(); time.sleep(0.4)
    return None


# CSV Source radio rows (y-bands) and the orange-dot x-column — the selected row's radio is
# filled orange (verified against 0x20 grabs 2026-07-11).
_SRC_ROW_Y = {0: (58, 72), 1: (80, 94), 2: (102, 116)}
_SRC_DOT_X = (656, 676)


def _read_csv_source(sc):
    """Which CSV Source is selected — 0=CH1, 1=CH2, 2=LA — read off the framebuffer (the
    selected radio is filled orange). None if unreadable. The CSV menu must be on screen."""
    img = _grab_fb(sc)
    if img is None:
        return None
    x0, x1 = _SRC_DOT_X
    best, bestf = None, 0.03                              # need ≥3 % orange in the dot column
    for src, (y0, y1) in _SRC_ROW_Y.items():
        band = img[y0:y1, x0:x1].reshape(-1, 3).astype(int)
        frac = float(((band[:, 0] > 150) & (band[:, 2] < 110)).mean())
        if frac > bestf:
            bestf, best = frac, src
    return best


def _select_source(sc, target, status=lambda m: None, tries=6):
    """Cycle the CSV Source to `target` (0/1/2), VERIFYING via the framebuffer after each press
    (order CH1→CH2→LA, wraps). CSV menu must be on screen. Returns True on success.

    The Source softkey is keyid 1 (`FN_SOURCE`) sent as a plain `0x13` key event — one inject per
    press cycles the radio one step (verified on hardware 2026-07-14: CH1→CH2→LA→CH1 in every
    run-state — running, stopped, and single-seq; the `0x13` second byte is ignored by the firmware,
    it is NOT a press/release, so press-only is a full click). It cycles through all three sources
    regardless of which channels are enabled. NOTE: reliability needs a HEALTHY link — a desynced
    scope (mid-reboot / stuck menu from a prior bad sequence) can swallow the key; a clean
    Default-Setup (see `prepare_capture(reset=True)`) restores it."""
    from mso5202d_decode import _key
    ok = False
    for _ in range(tries):
        if _read_csv_source(sc) == target:
            ok = True; break
        _key(sc, FN_SOURCE); time.sleep(1.0)             # one 0x13 keyid-1 inject = one radio step
    sc._resync(); time.sleep(0.4)
    if not ok:
        status(f"couldn't verify Source={_SRC_NAMES[target]}")
    return ok


def _clear_wavedata(sc, sh, status=lambda m: None, rounds=4):
    """Delete ALL WaveData*.csv off the SD card via the front-panel CSV delete key (F4 /
    keyid 4). Efficient (avoids an `ls` per delete): count the files with ONE `ls`, then
    press delete **N+1 times** — the first press opens the FileList, each further press
    deletes the selected file (no confirm dialog — verified 2026-07-11) — then ONE `ls` to
    verify; repeat only if some presses were dropped (the scope's key mailbox is
    single-slot). Front-panel keys + read-only `ls` only; **NO shell rm**."""
    from mso5202d_decode import _key
    if not _press_for_menu(sc, KEY_SR_MENU, 47, status):
        return
    if not _press_for_menu(sc, FN_CSV, 48, status):
        return
    n0 = len(_list_wavedata(sh))
    if not n0:
        status("card already has no WaveData CSVs"); return
    for _ in range(rounds):
        n = len(_list_wavedata(sh))                       # ONE ls to count this round
        if not n:
            break
        status(f"deleting {n} WaveData CSV(s) — F4 ×{n + 1}…")
        for _ in range(n + 1):                            # 1 press opens FileList, n delete
            _key(sc, FN_DELETE); time.sleep(0.6)
    left = len(_list_wavedata(sh))                        # ONE ls to verify
    status(f"card cleared ({n0 - left} deleted)" if not left else f"{left} CSV(s) still remain")


def clear_wavedata(sc, status=lambda m: None):
    """Standalone: delete all WaveData*.csv off the SD card via the front-panel CSV delete
    (no shell rm). Opens its own shell for the read-only `ls` checks."""
    from mso5202d_shell import Shell
    sh = Shell(scope=sc)
    try:
        _clear_wavedata(sc, sh, status)
    finally:
        sh.close()


def _direct_acquire(sc, channels, status=lambda m: None):
    """Read each analog channel (0=CH1, 1=CH2) DIRECTLY from the one frozen buffer over
    `0x02 01 <ch>` — the two reads hit the same stopped acquisition so CH1/CH2 are
    inter-channel **aligned** (verified: SPI clk+data decode cleanly this way). This is
    the reliable capture path for a screen-depth (4K) record: no SD card, no CSV, no
    fragile Source-cycling. Deep records (40K+) and LA still need the CSV route — `0x02`
    only serves the 3840-sample screen buffer and `02 01 05` (LA) is broken. Returns
    result dicts (`source`/`volts`/`dt_s`/`size`/`is_la=False`), same shape as the CSV path."""
    from mso5202d import decode_settings
    from mso5202d_decode import _raw_settings
    s = decode_settings(_raw_settings(sc))
    dt = (s.get('SAMPLE-INTERVAL-ns') or 0) * 1e-9
    out = []
    for ch in channels:
        try:
            # The 0x02 acquire is one-deep pipelined: the FIRST read after switching the
            # channel byte returns the *previously selected* channel's buffer, the second
            # returns this channel (verified 2026-07-11 — without this CH1/CH2 come back
            # byte-identical). Read twice on a stopped scope, keep the second.
            sc.read_waveform(ch)
            w = np.frombuffer(sc.read_waveform(ch), dtype=np.uint8)
        except Exception as e:
            status(f"  {_SRC_NAMES[ch]}: read failed ({e})"); continue
        if not len(w):
            status(f"  {_SRC_NAMES[ch]}: empty read"); continue
        pos = s.get(f'VERT-CH{ch + 1}-POS') or 0
        vdiv_mv = s.get(f'CH{ch + 1}-VDIV-mV') or 1000
        volts = to_divs(w, pos) * (vdiv_mv / 1000.0)       # divisions → volts
        out.append({'source': _SRC_NAMES[ch], 'volts': volts, 'dt_s': dt or None,
                    'size': len(w), 'is_la': False})
        status(f"  {_SRC_NAMES[ch]}: {len(w)} samples direct"
               + (f", {dt * 1e9:.1f} ns/sample" if dt else ""))
    return out


def _freeze(sc, min_fill_s=0.6):
    """RUN → (let one full record fill) → STOP: freeze one live 2-channel acquisition. Waits
    at least one record-window (= 19.2 × sec/div, read from HORIZ-TB) so a DEEP record at a
    slow timebase actually fills before we stop — otherwise the freeze grabs a partial record.
    STOP is confirmed via TRIG-STATE (in `_run_stop`). This is used for every capture (4K and
    deep): SINGLE-SEQ stalls forever/armed at slow timebases even with Force (verified
    2026-07-11), whereas a RUN→STOP freeze reliably reaches STOP and catches the live stream."""
    from mso5202d import decode_settings, TB_TO_NS
    from mso5202d_decode import _raw_settings
    _run_stop(sc, True)
    try:
        tdiv_ns = TB_TO_NS.get(decode_settings(_raw_settings(sc)).get('HORIZ-TB'), 0)
        fill = max(min_fill_s, 19.2 * tdiv_ns / 1e9 * 1.3)   # one window + 30% margin
    except Exception:
        fill = min_fill_s
    time.sleep(min(fill, 6.0))                                # cap the wait at 6 s
    _run_stop(sc, False)


# deep record sample counts per depth code (verified: 40K→40064, 512K→400064; 1M inferred)
_DEEP_SAMPLES = {0: 4064, 4: 40064, 6: 400064, 7: 4000064}


def _probe_pulse_ns(sc, status=lambda m: None):
    """Quick 4K freeze + direct read to measure the signal's **finest pulse** (ns). Used to
    pick a deep timebase that spreads the record over many bit-periods. Returns None if no
    channel carries a decodable signal."""
    from mso5202d import decode_settings
    from mso5202d_decode import _raw_settings
    from serial_decode import threshold_volts
    _run_stop(sc, False); _prep_block(sc, 0, setup=True); _freeze(sc)
    si_ns = decode_settings(_raw_settings(sc)).get('SAMPLE-INTERVAL-ns') or 0
    if not si_ns:
        return None
    shortest = None
    for r in _direct_acquire(sc, [0, 1], lambda m: None):
        d = threshold_volts(np.asarray(r['volts'])).astype(int)
        edges = np.flatnonzero(np.diff(d))
        if len(edges) < 4:
            continue
        runs = np.diff(edges)
        if len(runs):
            p = max(int(np.percentile(runs, 5)), 1) * si_ns   # robust shortest run → ns
            shortest = p if shortest is None else min(shortest, p)
    return shortest


def _pick_deep_tb(pulse_ns, deep_samples, target_samples=15):
    """Choose the TB index so the DEEP record puts ~target_samples on the finest pulse. The
    deep record spans the same window as the screen (≈19.2·TDIV) but with `deep_samples`
    points, so deep_dt = 19.2·TDIV/deep_samples; set deep_dt = pulse/target → ideal TDIV =
    (pulse/target)·deep_samples/19.2. Pick the nearest available index (≥6)."""
    from mso5202d import TB_TO_NS
    ideal_tdiv = (pulse_ns / target_samples) * deep_samples / 19.2
    cands = {i: v for i, v in TB_TO_NS.items() if i >= 6}
    return min(cands, key=lambda i: abs(cands[i] - ideal_tdiv))


def _wait_save_done(sc, status=lambda m: None, timeout=120):
    """After a Save the scope shows an orange **"Operation is in progress! Please Wait……"**
    banner over the FileList and IGNORES key presses until the write finalizes. For a big file
    (512K ≈ 7.7 MB) the card `ls` sees the file ~40 s before the scope is ready, so a Source
    cycle pressed then is silently dropped (→ the next save re-saves the same channel = "only
    CH1"). Poll the framebuffer until that orange banner clears. Verified 2026-07-11 against the
    on-screen state. Returns True when clear."""
    t0 = time.time()
    while time.time() - t0 < timeout:
        try:
            fb = _grab_fb(sc)
        except Exception:
            time.sleep(1.0); continue
        if fb is None:
            time.sleep(1.0); continue
        band = fb[230:245, 160:535].reshape(-1, 3).astype(int)   # the banner strip
        orange = ((band[:, 0] > 160) & (band[:, 2] < 100)
                  & (band[:, 1] > 60) & (band[:, 1] < 190))
        if float(orange.mean()) < 0.04:                          # banner gone → scope ready
            time.sleep(0.5)
            return True
        status("  save finishing (scope busy)…")
        time.sleep(1.5)
    return False


def _save_file_only(sc, sh, before, seen, wait_s, status, filelist_open=False):
    """Save one WaveData CSV on the CSV page (menuid 48), wait for the write to finish, return
    its NAME. Does **not** read the file back — deferred so a big read doesn't sit between
    Source changes. Returns None if no file appeared (e.g. card save disturbed).

    Save is **one keypress when the FileList is already open** (it writes directly), but **two
    when it's closed** (1st opens the FileList, 2nd writes). The FileList stays OPEN after any
    save, so the 1st channel saves with two presses and every later channel with ONE — pressing
    twice with the FileList open writes a SECOND file (that was the spurious "3rd waveform").

    Then wait **patiently**: a big write takes tens of seconds and only exposes WaveData<n>.csv
    when the scope renames its temp file at the END (~40 s for 512K). Do NOT re-press during the
    write (corrupts it / desyncs the Source) — only re-press if nothing appears after a long
    grace (a genuinely dropped press). Verified 2026-07-11."""
    from mso5202d_decode import _key
    def do_save():                               # write once (open the FileList first if closed)
        if not filelist_open:
            _key(sc, FN_SAVE); time.sleep(0.8)   # open FileList
        _key(sc, FN_SAVE)                        # write
    name = None; t0 = time.time()
    do_save()
    grace = min(wait_s, 45); last = time.time()  # wait ~45 s (covers a 512K write) before retry
    while time.time() - t0 < wait_s:
        if _list_wavedata(sh) - before - seen:
            name = _wait_new_csv(sh, before, seen, status, wait_s); break
        if time.time() - last > grace:           # nothing after the grace → the write press dropped
            _key(sc, FN_SAVE); last = time.time()   # FileList is open now → a single re-press writes
        time.sleep(0.8)
    return name


def deep_capture(sc, depth_code, status=lambda m: None, setup=True, channels=(1, 2), la=False,
                 save_sources=None, wait_s=None, delete_after=False, reset=True, auto_tb=False):
    """One-shot capture = `prepare_capture()` then `capture_prepared()` (the GUI calls those
    two separately — a Prepare button + a re-pressable Capture button). Brings **every enabled
    channel** back to the PC tagged `r['source']` = 'CH1'/'CH2'/'LA', via the unified
    SINGLE-SEQ → Save→CSV → read-back mechanism (see `capture_prepared`).

    `reset` (default True) does a **Default Setup** first so the capture is idempotent (known
    state, independent of how the panel was left). `auto_tb` (deep only) probes the signal and
    picks a slower timebase so the deep record spreads over many bit-periods (more frames to
    scroll). `save_sources` forces an explicit list of source indices (0=CH1,1=CH2,2=LA); with
    `delete_after`, clears the card once everything is read back. **Needs the SD card mounted.**
    Worker-thread only."""
    prepare_capture(sc, depth_code, status, setup=setup, channels=channels, la=la, reset=reset,
                    auto_tb=auto_tb)
    return capture_prepared(sc, depth_code, status, save_sources=save_sources, la=la,
                            wait_s=wait_s, delete_after=delete_after)


def capture_prepared(sc, depth_code, status=lambda m: None, save_sources=None, la=False,
                     wait_s=None, delete_after=False):
    """**Capture** one record from an already-`prepare_capture()`d scope and bring **every
    enabled channel** back to the PC (tagged `r['source']`). Re-pressable — no reset, no
    re-configure. **One unified mechanism for every depth** (4K included): SINGLE-SEQ trigger
    → for each enabled channel select its **Source** (framebuffer-verified, deterministic
    CH1→CH2→…) → two-press **Save** (once; wait out the async "Operation in progress" write)
    → read the CSVs back over `0x10`. Labels are the channel we actually selected, so there's
    no order-guessing or "CH2 twice" on a re-Capture."""
    from mso5202d import parse_wavedata_csv, decode_settings
    from mso5202d_decode import _raw_settings
    from mso5202d_shell import Shell

    # A deep CSV is written to the card on-instrument; it only appears once complete, and a
    # 512K (~7.7 MB) / 1M (~19 MB) write takes many seconds — detection window scales.
    if wait_s is None:
        wait_s = {0: 30, 4: 45, 6: 130, 7: 220}.get(depth_code, 60)
    # ONE unified mechanism for every depth (4K included): SINGLE-SEQ trigger → Save→CSV per
    # Source → read back over 0x10. The single-seq self-stops with a trigger-aligned record
    # (TRIG-STATE → 5/SINGLE, button red); we leave it STOPPED (don't toggle Run/Stop) so the
    # CSV saves read a stable frozen buffer and the Source cycles cleanly across CH1/CH2. (The
    # old 4K direct-0x02 fast path was removed for consistency — 4K now also goes via the card.)
    _trigger_record(sc, use_single=True, status=status)

    sh = Shell(scope=sc)                                 # share our USB handle for `ls`
    found = []; seen = set()
    try:
        before = _list_wavedata(sh)
        # which channels to bring back: explicit list, else every ENABLED channel
        if save_sources is not None:
            enabled = list(save_sources)
        else:
            s = decode_settings(_raw_settings(sc))
            enabled = [c for c, f in ((0, 'VERT-CH1-DISP'), (1, 'VERT-CH2-DISP'), (2, 'LA-SWI'))
                       if s.get(f)]
        if not enabled:
            status("no channels enabled to save"); return found
        status("bringing back: " + ", ".join(_SRC_NAMES[t] for t in enabled))
        if not (_press_for_menu(sc, KEY_SR_MENU, 47, status)
                and _press_for_menu(sc, FN_CSV, 48, status)):
            status("couldn't open the CSV menu"); return found
        # DETERMINISTIC save: for each enabled channel, **select its Source explicitly**
        # (verified via the 0x20 framebuffer — the selected radio is orange) then Save ONCE.
        # Always CH1 first, then CH2 (…, LA), regardless of where the Source was left by a prior
        # capture — no blind cycling, no "CH2 twice"/label-swap. The write is async: after each
        # Save the scope shows "Operation in progress" and ignores keys, so wait for that banner
        # to clear (`_wait_save_done`) before the NEXT Source select. Read-back is DEFERRED to a
        # 2nd pass so a 7.7 MB read doesn't sit between Source changes.
        picked = []                                      # (ch, filename), in enabled order
        fl_open = False                                  # the FileList stays open after any save
        for i, ch in enumerate(enabled):
            if i > 0:                                    # let the previous save's banner clear
                _wait_save_done(sc, status); sc._resync(); time.sleep(1.5)
            if not _select_source(sc, ch, status):       # fb-verified Source select
                status(f"  couldn't select {_SRC_NAMES[ch]} — skipping"); continue
            status(f"Save→CSV {_SRC_NAMES[ch]} ({i + 1}/{len(enabled)})…")
            name = _save_file_only(sc, sh, before, seen, wait_s, status, filelist_open=fl_open)
            if name is None:
                status(f"  {_SRC_NAMES[ch]}: no file (card save disturbed? a panel key press "
                       "re-detects it)"); continue
            picked.append((ch, name)); seen.add(name); fl_open = True
        _wait_save_done(sc, status)                      # last save fully done before reads/keys
        # 2nd pass — read + parse each saved file, tagged by the channel we selected for it.
        saved = []
        for ch, name in picked:
            try:
                r = parse_wavedata_csv(sc.read_file('/mnt/udisk/' + name, timeout=60000))
                r['file'] = name; r['source'] = _SRC_NAMES[ch]
                saved.append(r)
                dt = r.get('dt_s')
                status(f"  {name} = {_SRC_NAMES[ch]}: {r['size']} samples"
                       + (f", {dt*1e9:.1f} ns/sample" if dt else ""))
            except Exception as e:
                status(f"failed to read {name}: {e}")
        if not saved:
            status("no CSV saved — the card save may be disturbed (a front-panel key press "
                   "re-detects it)")
        found[:] = saved
        status("brought back: " + ", ".join(f"{r.get('source')}({'LA' if r['is_la'] else r['size']})"
                                             for r in found))
        if delete_after and found:      # all captures are on the PC now → free the card
            _clear_wavedata(sc, sh, status)
    finally:
        # Leave the scope clean + live. Order matters: RESUME RUN **first** (from the single-seq
        # STOP), THEN close the menu — the menu-close does a 0x11 write, which crash-reboots the
        # scope while STOPPED but is safe while RUNNING (verified 2026-07-11). Resync first — the
        # big file read-backs can leave a trailing chunk that would desync these key presses.
        try:
            sc._resync()
            status("resuming run…"); _run_stop(sc, True)   # single-seq STOP → running
            _close_menu(sc)                                 # Back + CONTROL-DISP-MENU=0 (running-safe)
            _run_stop(sc, True)                             # the 0x11 menu-close can stop it → re-run
            sc._resync()
        except Exception:
            pass
        sh.close()
    return found


# --- decode + render (pure; shared by GUI and headless) --------------------------
def decode_capture(results, params):
    """Threshold each analog channel's volts and run the selected decoder. Channels are
    matched by **`r['source']`** ('CH1'/'CH2') when present (a capture brings back every
    enabled channel, in enable order — not necessarily [CH1, CH2]); falls back to list
    order for CSVs loaded without a source tag. Channel index 0=CH1, 1=CH2. LA results are
    ignored here (analog decoders). Returns (events, dt_seconds_per_sample, used_indices)."""
    from serial_decode import threshold_volts, decode_uart, decode_spi, decode_i2c
    analog = [r for r in results if not r.get('is_la') and r.get('volts') is not None]
    if not analog:
        return [], (results[0].get('dt_s') if results else None), []
    dt = analog[0].get('dt_s')
    # source name → thresholded digital trace (fall back to list order if untagged)
    by_name = {}
    for i, r in enumerate(analog):
        name = r.get('source') or _SRC_NAMES[i] if i < 2 else None
        if name:
            by_name[name] = threshold_volts(r['volts'])

    def ch(idx):                                          # 0→CH1, 1→CH2
        name = _SRC_NAMES[idx] if idx < 2 else None
        return by_name.get(name) if name in by_name else threshold_volts(analog[min(idx, len(analog)-1)]['volts'])

    proto = params.get('proto', 'none')
    ns = dt * 1e9 if dt else None

    def n_bytes(ev):
        return sum(e['kind'] in ('byte', 'addr') for e in ev)

    if proto == 'uart':
        line = params.get('line', 0)
        ev = decode_uart(ch(line), sample_interval_ns=ns, baud=params.get('baud'))
        return ev, dt, [line]
    if proto == 'spi':
        clk, data = params.get('clk', 0), params.get('data', 1)
        a = decode_spi(ch(clk), ch(data), cpol=params.get('cpol', 0), cpha=params.get('cpha', 0))
        # channel labels can be imperfect (Source-cycle order) — also try swapping clk/data
        # and keep whichever decodes more bytes, so SCLK/data can't be silently swapped.
        b = decode_spi(ch(data), ch(clk), cpol=params.get('cpol', 0), cpha=params.get('cpha', 0))
        return (a, dt, [clk, data]) if n_bytes(a) >= n_bytes(b) else (b, dt, [data, clk])
    if proto == 'i2c':
        scl, sda = params.get('scl', 0), params.get('sda', 1)
        a = decode_i2c(ch(scl), ch(sda))
        b = decode_i2c(ch(sda), ch(scl))
        return (a, dt, [scl, sda]) if n_bytes(a) >= n_bytes(b) else (b, dt, [sda, scl])
    return [], dt, []


def pick_time_scale(span_s):
    for unit, s in (('s', 1.0), ('ms', 1e-3), ('µs', 1e-6), ('ns', 1e-9)):
        if span_s >= s:
            return unit, s
    return 'ns', 1e-9


def _res_len(r):
    v = r.get('volts'); w = r.get('words')
    return len(v) if v is not None else (len(w) if w is not None else 0)


def render_wave(ax, results):
    """Plot the captured channels on a dark axis — analog channels as volts vs time, LA as
    stacked digital rows (only the D-lines that toggle) below them. Handles a mix (a capture
    can bring back CH1/CH2 and LA together). Returns (x_scale_seconds, x_unit, annotation_y)
    for the decode-annotation layer."""
    ax.set_facecolor(BG); ax.tick_params(colors=FG, labelsize=7)
    for sp in ax.spines.values(): sp.set_color(GRID)
    ax.grid(True, color=GRID, lw=0.5)
    if not results:
        ax.set_title("no data captured", color=FG); return 1.0, 's', 0.0
    r0 = results[0]; dt = r0.get('dt_s') or 1e-9
    span = (r0.get('size') or _res_len(r0)) * dt
    unit, scl = pick_time_scale(span)

    def times(r):
        return (r['time_s'] if len(r.get('time_s', [])) else np.arange(_res_len(r)) * dt) / scl

    analog = [r for r in results if not r.get('is_la') and r.get('volts') is not None]
    la = [r for r in results if r.get('is_la') and r.get('words') is not None]
    vmin, vmax = 1e9, -1e9
    for i, r in enumerate(analog):
        ax.plot(times(r), r['volts'], lw=0.6, color=CH_COLORS[i % 2],
                label=r.get('source') or f"ch{i}")
        if len(r['volts']):
            vmin = min(vmin, float(r['volts'].min())); vmax = max(vmax, float(r['volts'].max()))
    if vmin > vmax:
        vmin, vmax = -1.0, 1.0
    rng = max(vmax - vmin, 0.1)
    # LA rows stacked below the analog traces: one row per toggling D-line
    la_bottom = vmin - 0.20 * rng
    row_h = 0.11 * rng
    n_rows = 0
    for r in la:
        w = np.asarray(r['words'], dtype=np.uint16); tx = times(r)
        toggling = [n for n in range(16) if 0 < int(((w >> n) & 1).sum()) < len(w)]
        for n in toggling:
            base = la_bottom - n_rows * (row_h * 1.4)
            ax.plot(tx, base + ((w >> n) & 1) * row_h, lw=0.8, color=LA_COLOR, solid_capstyle='round')
            ax.text(tx[0] if len(tx) else 0, base + row_h / 2, f"D{n}", color=LA_COLOR,
                    fontsize=6, va='center', ha='right')
            n_rows += 1
    low = (la_bottom - n_rows * (row_h * 1.4)) if la else (vmin - 0.28 * rng)
    ax.set_ylim(low - 0.05 * rng, vmax + 0.08 * rng)
    ax.set_xlabel(f"time ({unit})", color=FG); ax.set_ylabel("volts", color=FG)
    if analog:
        leg = ax.legend(fontsize=8, facecolor=BG, edgecolor=GRID, loc='upper right')
        for t in leg.get_texts(): t.set_color(FG)
    anno_y = (vmin - 0.06 * rng) if not la else (low + 0.02 * rng)
    return scl, unit, anno_y


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
        arts.append(ax.text(x, anno_y, e['text'], color=col, fontsize=7,
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

    def _cmd_prepare(self, kw):
        if not self.sc:
            self._status("no scope connected"); return
        prepare_capture(self.sc, kw['depth_code'], status=self._status,
                        setup=kw.get('setup', True), channels=kw.get('channels', (1, 2)),
                        reset=kw.get('reset', True), auto_tb=kw.get('auto_tb', False))
        self._emit('prepared', kw['depth_code'])         # → enable the Capture button

    def _cmd_capture(self, kw):
        if not self.sc:
            self._status("no scope connected"); self._emit('capture', []); return
        res = capture_prepared(self.sc, kw['depth_code'], status=self._status,
                               save_sources=kw.get('save_sources', None),
                               delete_after=kw.get('delete_after', False))
        self._emit('capture', res)

    def _cmd_key(self, kw):
        if not self.sc:
            self._status("no scope connected"); return
        from mso5202d_decode import _key
        _key(self.sc, kw['keyid']); self._status(kw.get('label', 'key sent'))

    def _cmd_clear(self, kw):
        if not self.sc:
            self._status("no scope connected"); return
        clear_wavedata(self.sc, status=self._status)

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
        self.prepared_depth = None   # depth code the scope is currently prepared for (None=not)
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
        self.v_depth = tk.StringVar(value='4K')
        depth_cb = ttk.Combobox(g, textvariable=self.v_depth, state='readonly',
                                values=[d[0] for d in DEEP_DEPTHS], width=12)
        depth_cb.pack(fill=tk.X)
        # Changing the depth/reset invalidates the prepared state → must Prepare again.
        depth_cb.bind('<<ComboboxSelected>>', lambda e: self._set_prepared(False))
        self.v_reset = tk.BooleanVar(value=True)
        ttk.Checkbutton(g, text="reset scope first (idempotent)", variable=self.v_reset,
                        command=lambda: self._set_prepared(False)).pack(anchor='w')
        # Which channels to prepare + capture (on, 1×, DC, 1 V/div). Untick to leave a channel off.
        self.v_setup = tk.BooleanVar(value=True)          # configure channels at all (kept for API)
        ttk.Label(g, text="prepare channels (on, 1×, DC, 1 V/div):").pack(anchor='w')
        chrow = ttk.Frame(g); chrow.pack(anchor='w')
        self.v_ch1 = tk.BooleanVar(value=True)
        self.v_ch2 = tk.BooleanVar(value=True)
        # 1M store depth is single-channel only → re-Prepare when the ticks change.
        ttk.Checkbutton(chrow, text="CH1", variable=self.v_ch1,
                        command=lambda: self._set_prepared(False)).pack(side=tk.LEFT)
        ttk.Checkbutton(chrow, text="CH2", variable=self.v_ch2,
                        command=lambda: self._set_prepared(False)).pack(side=tk.LEFT, padx=(10, 0))
        self.v_delete = tk.BooleanVar(value=False)
        ttk.Checkbutton(g, text="delete CSV from card after read",
                        variable=self.v_delete).pack(anchor='w')
        btn(g, "① Prepare (reset + configure)", self._do_prepare)
        # Capture is enabled only after Prepare (managed separately from the busy toggle).
        self.capture_btn = btn(g, "② Capture (single-seq)", self._do_capture)
        self.capture_btn.configure(state='disabled')
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
        btn(g, "Clear card CSVs", self._clear_card)

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

    def _set_prepared(self, ok, depth=None):
        """Track whether the scope is prepared, and enable/disable the Capture button."""
        self.prepared_depth = depth if ok else None
        try:
            self.capture_btn.configure(state='normal' if ok else 'disabled')
        except Exception:
            pass

    def _do_prepare(self):
        """① Prepare — reset (idempotent) + configure the scope for the selected depth, and
        (auto) spread a deep record's timebase for the selected protocol. Enables Capture."""
        if not self.scope_present:
            self.status.set("no scope connected — use Load CSV instead"); return
        code = dict(DEEP_DEPTHS)[self.v_depth.get()]
        channels = tuple(n for n, v in ((1, self.v_ch1), (2, self.v_ch2)) if v.get())
        if not channels:
            self.status.set("tick at least one channel (CH1/CH2) to prepare"); return
        if code == 7 and len(channels) > 1:              # 1M store depth is single-channel only
            self.status.set("1M is single-channel only — untick CH1 or CH2 (or pick 512K)"); return
        self._set_prepared(False)
        self.status.set("preparing…")
        # auto_tb (deep only): spread the record over more time when decoding a protocol so
        # there are many frames to scroll through, instead of the same ~4 ms window as 4K.
        self.worker.submit('prepare', depth_code=code, setup=self.v_setup.get(), channels=channels,
                           reset=self.v_reset.get(),
                           auto_tb=(code != 0 and self.v_proto.get() != 'none'))

    def _do_capture(self):
        """② Capture — instantly grab a single-seq record from the prepared scope. Re-pressable."""
        if not self.scope_present:
            self.status.set("no scope connected — use Load CSV instead"); return
        if self.prepared_depth is None:
            self.status.set("press ① Prepare first"); return
        # save_sources=None → bring back EVERY enabled channel (CH1/CH2 if displayed, LA if
        # the pod is on). The decode picks which of those it needs by channel name.
        self.status.set("capturing…")
        self.worker.submit('capture', depth_code=self.prepared_depth,
                           save_sources=None, delete_after=self.v_delete.get())

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

    def _clear_card(self):
        from tkinter import messagebox
        if not self.scope_present:
            self.status.set("no scope connected"); return
        if messagebox.askokcancel("Clear card", "Delete ALL WaveData*.csv from the SD card?\n"
                                  "(front-panel delete — make sure you've read back what you need)"):
            self.status.set("clearing card…"); self.worker.submit('clear')

    def _set_busy(self, busy):
        st = 'disabled' if busy else 'normal'
        for b in self.action_btns:
            try: b.configure(state=st)
            except Exception: pass
        if not busy:                     # Capture stays disabled unless the scope is prepared
            try:
                self.capture_btn.configure(
                    state='normal' if self.prepared_depth is not None else 'disabled')
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
                elif kind == 'prepared':
                    self._set_prepared(True, payload)     # payload = prepared depth code
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
