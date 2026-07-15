#!/usr/bin/env python3
"""UART decoder (asynchronous, idle-high, LSB-first, start/stop framed).

Robust to real, gapless (continuous-mode) captures: decodes on a least-squares **bit grid**
(drift-free, fit from the edges) and picks the byte phase by whichever framing validates the most
frames — so a solid back-to-back stream, where every data 1→0 edge mimics a start bit, still frames
correctly, and a capture that TRIGGERS MID-BYTE just drops the leading partial frame and decodes the
clean tail. Run `python3 -m decoding.uart` for the hardware-free self-test."""
import numpy as np
from decoding import common as sc


def _uart_one(digital, reverse=False, *, sample_interval_ns=None, baud=None,
              bits=8, parity='none', stops=1, idle=1):
    """One-direction UART decode on a bit grid. `reverse` decodes the time-reversed trace (frame
    cells run stop→data(reversed)→start) and re-maps indices to the original frame."""
    d = np.asarray(digital).astype(np.int8)
    n = len(d)
    if n < 2:
        return []
    if reverse:
        d = d[::-1]
    spb0 = (1e9 / baud) / sample_interval_ns if (baud and sample_interval_ns) else sc.min_pulse(d)
    if not spb0 or spb0 < 2:
        return []
    e = sc.edges(d)
    spb, phase = sc.refine_period(e, spb0) if len(e) >= 3 else (float(spb0), float(e[0]) if len(e) else 0.0)
    if spb < 2:
        return []
    pbit = 0 if parity == 'none' else 1
    flen = 1 + bits + pbit + stops
    bitv, ci = sc.sample_grid(d, spb, phase)
    if len(bitv) < flen:
        return []

    # Cell layout of one frame, forward vs. reversed-in-time:
    #   forward : [start=!idle][data LSB..MSB][parity?][stop*=idle]
    #   reverse : [stop*=idle][parity?][data MSB..LSB][start=!idle]
    lo = 1 - idle
    if not reverse:
        start_cell = 0
        data_cells = [1 + b for b in range(bits)]
        par_cell = 1 + bits if pbit else None
        stop_cells = [1 + bits + pbit + s for s in range(stops)]
    else:
        stop_cells = list(range(stops))
        par_cell = stops if pbit else None
        base = stops + pbit
        data_cells = [base + (bits - 1 - b) for b in range(bits)]
        start_cell = flen - 1

    # A window is a frame iff its start cell is !idle and its stop cell(s) idle; value comes from the
    # data cells (parity folded into `ok`). Two framings are built and the one with more valid frames
    # wins:
    #   (A) fixed-offset tiling at the phase that validates the most frames — the correct byte
    #       boundary for a gapless / continuous stream (a wrong phase piles up stop-bit violations on
    #       real, jittery data);
    #   (B) a greedy resync walk — needed when frames are separated by idle GAPS (framed mode), which
    #       fixed-offset tiling can't follow.
    # Both self-recover a corrupt head / mid-stream trigger: invalid leading frames are skipped and
    # decoding continues from the first clean frame to the end.
    L = len(bitv)

    def _match(i):
        fr = bitv[i:i + flen]
        if len(fr) < flen or fr[start_cell] != lo or any(fr[c] != idle for c in stop_cells):
            return None
        val = 0
        for b in range(bits):
            if fr[data_cells[b]]:
                val |= (1 << b)
        ok = True
        if par_cell is not None:
            ones = bin(val).count('1') + int(fr[par_cell])
            ok = (ones % 2 == 0) if parity == 'even' else (ones % 2 == 1)
        return val, ok

    matches = [_match(i) for i in range(max(0, L - flen + 1))]

    def _make(i, r):
        val, ok = r
        a, b_ = int(ci[i]), int(ci[i + flen - 1])
        if reverse:                                          # map back to original coordinates
            a, b_ = n - 1 - b_, n - 1 - a
        return {'start': min(a, b_), 'end': max(a, b_), 'value': val, 'ok': ok,
                'text': f"{val:02X}" + ('' if ok else '!'), 'kind': 'byte'}

    best_a = []
    for off in range(flen):                                  # (A) fixed-offset tiling
        fr = [_make(i, matches[i]) for i in range(off, len(matches), flen) if matches[i]]
        if len(fr) > len(best_a):
            best_a = fr
    walk_b, i = [], 0                                        # (B) greedy resync
    while i < len(matches):
        if matches[i] is None:
            i += 1
        else:
            walk_b.append(_make(i, matches[i])); i += flen
    frames = best_a if len(best_a) >= len(walk_b) else walk_b
    if reverse:
        frames.reverse()
    return frames


def decode_uart(digital, sample_interval_ns=None, baud=None,
                bits=8, parity='none', stops=1, idle=1, both_ways=False):
    """Decode an asynchronous UART line (idle high, LSB first, 1 start bit).

    The caller specifies the line rate: pass `baud` with `sample_interval_ns` to lock the bit period
    (auto-detect from the shortest pulse is only a fallback). Robust to gapless back-to-back streams
    (bit-grid framing, not start-edge hunting): each frame is validated and the walk re-syncs at every
    frame, so a capture that TRIGGERS MID-BYTE skips the leading garbage and decodes cleanly from the
    first good frame to the end. `both_ways=True` also tries the time-reversed trace and keeps whichever
    yields more frames (only helpful if the forward walk can't lock at all). `parity`:
    'none'|'even'|'odd'. Returns frames [{start, end, value, ok, text, kind}]; `ok` is the framing check."""
    digital = np.asarray(digital).astype(np.int8)
    kw = dict(sample_interval_ns=sample_interval_ns, baud=baud,
              bits=bits, parity=parity, stops=stops, idle=idle)
    if both_ways:
        return sc.both_ways(lambda dig, reverse=False: _uart_one(dig, reverse, **kw),
                            digital, score=lambda fr: len(fr))
    return _uart_one(digital, False, **kw)


# --- self-test -------------------------------------------------------------------
def _synth_uart(values, spb=20, parity='none', gap=8):
    """Idle-high UART trace for `values` (LSB-first, 1 start, 1 stop) at `spb` samples/bit, with `gap`
    idle bits between frames (gap=0 → a solid back-to-back stream)."""
    bitstream = []
    for v in values:
        frame = [0] + [(v >> b) & 1 for b in range(8)]
        if parity != 'none':
            ones = bin(v).count('1')
            frame.append((ones % 2) if parity == 'even' else (1 - ones % 2))
        frame.append(1)
        bitstream += frame + [1] * gap
    bitstream = [1] * gap + bitstream
    return np.repeat(np.array(bitstream, dtype=int), spb)


def selftest():
    from decoding.common import threshold, to_bytes_trace, ramp_ratio
    ramp = list(range(256))
    ok = True
    for parity in ('none', 'even', 'odd'):
        dig = _synth_uart(ramp, spb=20, parity=parity, gap=8)
        logic = threshold(to_bytes_trace(dig, pos=30), pos=30)
        got = [f['value'] for f in decode_uart(logic, parity=parity)]
        good = (got == ramp)
        print(f"UART parity={parity:4} framed : {len(got)} bytes, {'OK' if good else 'FAIL'}")
        ok &= good
    dig = _synth_uart(ramp, spb=20, gap=0)                  # solid back-to-back (continuous mode)
    dig = np.concatenate([dig, np.ones(40, dtype=int)])     # trailing idle so the last stop bit fits
    got = [f['value'] for f in decode_uart(dig)]
    print(f"UART gapless (continuous): {len(got)} bytes, {'OK' if got == ramp else 'FAIL'}")
    ok &= (got == ramp)
    dig = _synth_uart(ramp, spb=20, gap=4)                  # framed, cut mid-byte → recover the tail
    got = [f['value'] for f in decode_uart(dig[int(len(dig) * 0.37):])]
    good = ramp_ratio(got) >= 0.99 and len(got) > 100       # ≤1 artifact at the cut boundary
    print(f"UART mid-stream start    : {len(got)} bytes ramp={ramp_ratio(got):.3f}, {'OK' if good else 'FAIL'}")
    ok &= good
    return ok


if __name__ == '__main__':
    import sys
    sys.exit(0 if selftest() else 1)
