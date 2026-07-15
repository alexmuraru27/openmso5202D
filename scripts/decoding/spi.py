#!/usr/bin/env python3
"""SPI decoder (SCLK + one data line; optional CS).

Data is sampled on the leading (cpha=0) or trailing (cpha=1) clock edge. Byte framing comes from a
captured CS line if present, else from idle-clock gaps (so a bit-banged stream re-aligns even when the
capture starts mid-byte); a short gap of a few bit periods is treated as MISSED clock edges (a
distorted / bandwidth-limited clock) and reconstructed so framing doesn't slip. With only SCLK+data
and no gaps or CS, byte boundaries are genuinely ambiguous. Run `python3 -m decoding.spi` to self-test."""
import numpy as np


def decode_spi(clk, data, cpol=0, cpha=0, msb_first=True, cs=None, bits=8,
               word_gap=10.0, max_missed=8):
    """Decode an SPI data line clocked by `clk`. With clock idle level `cpol`, the sampling edge is
    rising iff cpol==cpha, else falling.

    Word framing (priority): a captured `cs` (active-low) frames words and gates edges; else an
    idle-clock gap longer than `word_gap`× the typical bit spacing starts a new word; else bytes are
    grouped every `bits` sampled edges. A gap of 2..`max_missed` bit periods is treated as missed clock
    edges and reconstructed. Returns [{start, end, value, text, kind}]."""
    c = np.asarray(clk).astype(np.int8)
    dat = np.asarray(data).astype(np.int8)
    sample_rising = (cpol == cpha)
    want = 1 if sample_rising else -1
    edges = list(np.flatnonzero(np.diff(c.astype(int)) == want) + 1)
    cs_arr = None if cs is None else np.asarray(cs).astype(int)
    med = float(np.median(np.diff(edges))) if (cs_arr is None and len(edges) > 2) else None

    out = []
    buf, pos = [], []

    def _sample(idx):
        buf.append(dat[min(idx, len(dat) - 1)]); pos.append(idx)
        if len(buf) == bits:
            val = 0
            for j, b in enumerate(buf):
                val = (val << 1) | b if msb_first else val | (b << j)
            out.append({'start': pos[0], 'end': pos[-1], 'value': val,
                        'text': f"{val:02X}", 'kind': 'byte'})
            buf.clear(); pos.clear()

    prev_active = True
    prev_edge = None
    for e in edges:
        if cs_arr is not None:
            active = cs_arr[e] == 0
            if not active:
                buf.clear(); pos.clear(); prev_active = False
                continue
            if not prev_active:
                buf.clear(); pos.clear()
            prev_active = True
        elif med and prev_edge is not None:
            gap = e - prev_edge
            n = int(round(gap / med)) if med else 1
            if 2 <= n <= max_missed and abs(gap - n * med) <= 0.5 * med:
                for k in range(1, n):
                    _sample(prev_edge + int(round(k * med)))
            elif word_gap and gap > word_gap * med:
                buf.clear(); pos.clear()
        prev_edge = e
        _sample(e)
    return out


# --- self-test -------------------------------------------------------------------
def _synth_spi(values, spb=10, cpol=0, cpha=0, msb_first=True, byte_gap=0):
    """SCLK+MOSI clocking `values`, MSB-first, mode (cpol,cpha). `byte_gap` idle-clock samples between
    bytes exercise the gap-based word re-framing."""
    clk, dat = [], []
    idle = cpol
    lead = (1 - cpol)

    def hold(c, d, k):
        clk.extend([c] * k); dat.extend([d] * k)

    cur = 0
    hold(idle, 0, spb)
    for v in values:
        order = range(7, -1, -1) if msb_first else range(8)
        for b in order:
            bit = (v >> b) & 1
            if cpha == 0:
                cur = bit
                hold(idle, cur, spb // 2); hold(lead, cur, spb // 2)
            else:
                hold(idle, cur, spb // 2); cur = bit; hold(lead, cur, spb // 2)
            clk[-1] = lead
        if byte_gap:
            hold(idle, cur, byte_gap)
    hold(idle, cur, spb)
    return np.array(clk, dtype=int), np.array(dat, dtype=int)


def selftest():
    ramp = list(range(256))
    ok = True
    for cpol in (0, 1):
        for cpha in (0, 1):
            for msb in (True, False):
                clk, dat = _synth_spi(ramp, cpol=cpol, cpha=cpha, msb_first=msb)
                got = [o['value'] for o in decode_spi(clk, dat, cpol=cpol, cpha=cpha, msb_first=msb)]
                good = (got == ramp)
                print(f"SPI  mode{cpol}{cpha} {'MSB' if msb else 'LSB'}: {len(got)} bytes, "
                      f"{'OK' if good else 'FAIL'}")
                ok &= good
    clk, dat = _synth_spi(ramp, spb=10, byte_gap=150)
    got = [o['value'] for o in decode_spi(clk, dat)]
    print(f"SPI  gap-framed  : {len(got)} bytes, {'OK' if got == ramp else 'FAIL'}")
    ok &= (got == ramp)
    cut = 45
    got = [o['value'] for o in decode_spi(clk[cut:], dat[cut:])]
    print(f"SPI  mid-byte cut: {len(got)} bytes, {'OK' if got == ramp[1:] else 'FAIL'}")
    ok &= (got == ramp[1:])
    return ok


if __name__ == '__main__':
    import sys
    sys.exit(0 if selftest() else 1)
