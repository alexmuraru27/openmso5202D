#!/usr/bin/env python3
"""Shared front-end for the serial-protocol decoders (UART / SPI / I²C).

Everything the per-protocol decoders (`decoding.uart` / `.spi` / `.i2c`) have in common
lives here so each of those only holds its protocol logic:

  - analog → logic thresholding (local-envelope Schmitt trigger),
  - edge extraction and bit-period estimation,
  - a **bit grid**: fit a clean, drift-free sample grid to the edges (least-squares),
    then sample each bit by majority vote over its middle — the basis of robust
    UART framing and any fixed-rate decode,
  - `both_ways()`: run a decoder forward AND backward and keep the better result, so
    a capture whose START is garbage but whose END is clean still decodes from the
    end (and vice-versa).

Pure logic, no hardware. `from decoding import common as sc` in the decoders."""
import numpy as np


# --- analog → logic --------------------------------------------------------------
def _schmitt_arr(sig, lo_th, hi_th, start_high=None):
    """Schmitt trigger with per-sample (array) or scalar thresholds, vectorized.
    A sample above `hi_th` forces the state high, below `lo_th` forces it low, and in
    between the state holds — the logic trace is the forward-fill of the last forcing
    sample. O(n), no Python loop (matters for the array-threshold envelope path over a
    deep 100k+ sample capture)."""
    sig = np.asarray(sig, dtype=float)
    n = len(sig)
    out = np.zeros(n, dtype=bool)
    if n == 0:
        return out
    ev = np.zeros(n, dtype=np.int8)
    ev[sig > hi_th] = 1
    ev[sig < lo_th] = -1
    last = np.maximum.accumulate(np.where(ev != 0, np.arange(n), -1))
    have = last >= 0
    out[have] = ev[last[have]] == 1
    if not have.all():                    # before the first forcing sample
        lo0 = lo_th[0] if np.ndim(lo_th) else lo_th
        hi0 = hi_th[0] if np.ndim(hi_th) else hi_th
        out[~have] = (sig[0] > (lo0 + hi0) / 2) if start_high is None else start_high
    return out


def _schmitt_auto(sig, frac=0.5, hysteresis=0.3):
    """Global Schmitt: rails = glitch-robust full range (1st/99.9th pct ≈ min/max), trigger at
    `frac` between them (midpoint) with a `hysteresis` band. One fixed threshold for the whole
    record — correct for clean signals and the fallback for lines too short to gauge a period."""
    sig = np.asarray(sig, dtype=float)
    if not len(sig):
        return np.zeros(0, dtype=bool)
    lo, hi = np.percentile(sig, 0.1), np.percentile(sig, 99.9)
    if hi - lo < 1e-12:
        return np.zeros(len(sig), dtype=bool)
    mid = lo + (hi - lo) * frac
    band = (hi - lo) * hysteresis / 2
    return _schmitt_arr(sig, mid - band, mid + band)


def schmitt_local(sig, hyst_frac=0.2, floor_frac=0.12):
    """Digitize an analog array against its LOCAL envelope instead of one global level. A sliding
    max/min over ~1.5 bit periods tracks the signal's own high and low as they drift, triggering
    at the LOCAL midpoint (hysteresis band scaled to the local swing, floored so idle noise can't
    chatter). Recovers a fast line whose low droops during active bursts (AC coupling / limited
    bandwidth) — a single global threshold sits above that drooped low and drops ~20 % of edges;
    the local midpoint follows the droop and keeps every one. Falls back to the global midpoint
    for a line with too few transitions to gauge a period. Empty/flat input → flat False trace."""
    from scipy.ndimage import maximum_filter1d, minimum_filter1d
    sig = np.asarray(sig, dtype=float)
    n = len(sig)
    if n == 0:
        return np.zeros(0, dtype=bool)
    lo, hi = np.percentile(sig, 0.1), np.percentile(sig, 99.9)
    span = hi - lo
    if span < 1e-12:
        return np.zeros(n, dtype=bool)
    g = _schmitt_arr(sig, lo + span * 0.35, lo + span * 0.65)
    tr = np.flatnonzero(np.diff(g.astype(np.int8)) != 0)
    if len(tr) < 4:
        return _schmitt_auto(sig)
    period = float(np.median(np.diff(tr))) * 2.0
    win = max(3, int(round(period * 1.5)))
    loc_hi = maximum_filter1d(sig, win, mode='nearest')
    loc_lo = minimum_filter1d(sig, win, mode='nearest')
    mid = (loc_hi + loc_lo) * 0.5
    band = np.maximum((loc_hi - loc_lo) * hyst_frac, span * floor_frac)
    return _schmitt_arr(sig, mid - band, mid + band)


def threshold_volts(volts):
    """Threshold an already-in-volts analog array (a Save→CSV deep capture) into a boolean logic
    trace (True = high), digitizing against the local signal envelope."""
    return schmitt_local(volts)


def threshold(y_bytes, pos):
    """Raw scope waveform bytes + the channel's VERT-POS → boolean logic trace. Unwraps to
    divisions first (so the threshold sees the same up=higher-voltage trace the plotter draws)."""
    from mso5202d_plot import to_divs                 # lazy: avoids an import cycle
    y_bytes = np.asarray(y_bytes, dtype=np.uint8)
    if not len(y_bytes):
        return np.zeros(0, dtype=bool)
    return schmitt_local(to_divs(y_bytes, pos))


# --- edges & bit period ----------------------------------------------------------
def edges(d):
    """Indices where the level changes (edge lands on the new-level sample)."""
    return np.flatnonzero(np.diff(np.asarray(d).astype(np.int8)) != 0) + 1


def min_pulse(d):
    """Shortest constant run in samples ≈ one bit period (bit-rate auto-detect)."""
    e = edges(d)
    return int(np.min(np.diff(e))) if len(e) >= 2 else 0


def idle_level(d):
    """Detect a line's resting/idle level as the level held during its LONGEST constant run — an
    idle stretch (line at rest, or a multi-bit gap) is longer than any in-frame run. Used to
    auto-detect UART polarity (idle high vs low) and an SPI clock's idle level. Returns 0 or 1."""
    d = np.asarray(d).astype(np.int8)
    if len(d) < 2:
        return 1
    bounds = np.concatenate([[0], np.flatnonzero(np.diff(d) != 0) + 1, [len(d)]])
    k = int(np.argmax(np.diff(bounds)))
    return int(d[bounds[k]])


# --- bit grid --------------------------------------------------------------------
def refine_period(e, spb0):
    """Refine samples-per-bit + phase by least-squares fitting the edges to an integer grid
    e_i ≈ phase + k_i·spb. Every edge sits on a bit boundary (an integer number of bit periods
    from any other), so snapping each to its nearest grid index and regressing recovers the TRUE
    period from the data — eliminating the slow drift a nominal (baud-derived) period accumulates
    over a long record. Returns (spb, phase); falls back to (spb0, e[0]) if the fit is degenerate."""
    e = np.asarray(e, dtype=float)
    if len(e) < 3 or spb0 < 1:
        return float(spb0), (float(e[0]) if len(e) else 0.0)
    k = np.round((e - e[0]) / spb0)
    if k[-1] == k[0]:
        return float(spb0), float(e[0])
    A = np.vstack([k, np.ones_like(k)]).T
    (spb, phase), *_ = np.linalg.lstsq(A, e, rcond=None)
    if not (spb > 1):
        return float(spb0), float(e[0])
    return float(spb), float(phase)


def sample_grid(d, spb, phase, vote=True):
    """Sample every bit cell of `d` on the grid boundaries `phase + k·spb`, returning the bit
    array (one 0/1 per cell). With `vote`, each cell is a majority over its middle half (robust
    to edge jitter); else a single centre sample. Also returns the centre sample index per bit."""
    d = np.asarray(d).astype(np.int8)
    n = len(d)
    spb = float(spb)
    # Reduce phase to (-spb/2, spb/2] — plain `% spb` returns ≈spb (not 0) when phase is a
    # near-exact multiple of spb (float rounding), which would start the grid one cell late and
    # shift every bit by one. Centring lands the boundaries on the real bit edges.
    phase = ((phase + 0.5 * spb) % spb) - 0.5 * spb
    k0 = int(np.floor((0 - phase) / spb))
    k1 = int((n - 1 - phase) / spb)
    ks = np.arange(k0, k1)
    if not len(ks):
        return np.zeros(0, dtype=np.int8), np.zeros(0, dtype=int)
    centres = phase + (ks + 0.5) * spb
    ci = np.clip(np.round(centres).astype(int), 0, n - 1)
    if not vote or spb < 4:
        return d[ci], ci
    half = max(1, int(spb * 0.25))                   # middle half of the cell
    acc = np.zeros(len(ks), dtype=int)
    span = 0
    for off in range(-half, half + 1):
        idx = np.clip(ci + off, 0, n - 1)
        acc += d[idx]; span += 1
    return (acc * 2 >= span).astype(np.int8), ci


# --- forward/backward decode -----------------------------------------------------
def both_ways(decode_fn, digital, *, score, **kw):
    """Run `decode_fn` on the trace as-is AND on its time-reverse, and keep whichever result
    scores higher (`score(result) -> number`). The reverse pass re-maps sample indices back to
    the original frame, so a capture whose START is corrupt (triggered mid-byte) but whose END is
    clean still yields the tail — exactly when a purely-forward pass would desync and lose it.

    `decode_fn(digital, reverse=<bool>, **kw)` must accept a `reverse` flag telling it the trace
    is time-reversed (so it can flip start/stop-bit and edge-direction semantics and re-map
    indices). Returns the better result list."""
    fwd = decode_fn(digital, reverse=False, **kw)
    rev = decode_fn(digital, reverse=True, **kw)
    return rev if score(rev) > score(fwd) else fwd


# --- scoring / test helpers ------------------------------------------------------
def ramp_ratio(vals):
    """Fraction of adjacent byte pairs that step by +1 (mod 256) — how well a decode matches the
    generator's 0x00..0xFF ramp. 0.0 for fewer than two bytes."""
    if len(vals) < 2:
        return 0.0
    return sum(1 for i in range(1, len(vals)) if vals[i] == (vals[i - 1] + 1) & 0xFF) / (len(vals) - 1)


def to_bytes_trace(digital, pos=0, amp_div=1.0):
    """Wrap a 0/1 digital trace back into scope-style waveform bytes so a self-test can exercise the
    full analog→threshold()→decode path: byte = (pos + 16 + signal) mod 256, high/low at ±amp_div
    divisions (25 counts/div)."""
    d = np.asarray(digital).astype(int)
    sig = (d * 2 - 1) * (amp_div * 25)
    return ((pos + 16 + sig) % 256).astype(np.uint8)
