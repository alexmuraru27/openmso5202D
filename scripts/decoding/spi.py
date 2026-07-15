#!/usr/bin/env python3
"""SPI decoder (SCLK + one data line; optional CS).

Data is sampled on the leading (cpha=0) or trailing (cpha=1) clock edge. Byte framing comes from a
captured CS line if present, else from idle-clock gaps (so a bit-banged stream re-aligns even when the
capture starts mid-byte); a short gap of a few bit periods is treated as MISSED clock edges (a
distorted / bandwidth-limited clock) and reconstructed so framing doesn't slip. With only SCLK+data
and no gaps or CS, byte boundaries are genuinely ambiguous. Run `python3 -m decoding.spi` to self-test."""
import numpy as np


def detect_sample_rising(clk, data):
    """Detect the SPI sampling edge from the signal itself (subsumes CPOL+CPHA). Data is shifted on
    one clock edge and held stable across the other — the SAMPLING edge — so data-line transitions
    cluster near the SHIFT edge; the correct sampling edge is the opposite one. Returns True to sample
    on rising clock edges, False on falling. Falls back to True (mode 0) if there's too little to tell."""
    c = np.asarray(clk).astype(int)
    dat = np.asarray(data).astype(int)
    crise = np.flatnonzero(np.diff(c) == 1) + 1
    cfall = np.flatnonzero(np.diff(c) == -1) + 1
    dedge = np.flatnonzero(np.diff(dat) != 0) + 1
    if len(dedge) < 3 or len(crise) < 2 or len(cfall) < 2:
        return True
    clock_edges = np.sort(np.concatenate([crise, cfall]))
    rising_set = np.zeros(len(c) + 1, dtype=bool)
    rising_set[crise] = True
    j = np.clip(np.searchsorted(clock_edges, dedge), 1, len(clock_edges) - 1)
    left, right = clock_edges[j - 1], clock_edges[j]
    nearest = np.where(dedge - left <= right - dedge, left, right)
    near_rise = int(rising_set[nearest].sum())
    shift_on_rising = near_rise >= (len(nearest) - near_rise)
    return not shift_on_rising                                # sample on the edge opposite the shift


def _gap_has_pulse(clk_analog, a, b, med):
    """When two detected clock edges are ~2..N periods apart, decide whether the clock actually
    PULSED in the gap (a bandwidth-limited/rounded pulse that didn't cross the threshold → a MISSED
    edge to reconstruct) or stayed FLAT at idle (a real inter-word transaction gap → reframe). This
    is the one discriminator a passive scope capture has that edge-triggered hardware lacks: the raw
    clock waveform. We inspect the MIDDLE of the gap (away from the transition tails at a/b): a real
    idle stays near one level (small range); a missed pulse shows a swing. No idle-level/polarity
    knowledge needed. Returns True if a pulse is present."""
    m = max(1, int(round(med)))
    seg = clk_analog[min(a + m // 2, b - 1):max(b - m // 2, a + 2)]
    if len(seg) < 3:
        seg = clk_analog[a + 1:b]
    if len(seg) < 2:
        return True
    lo, hi = np.percentile(clk_analog, 1), np.percentile(clk_analog, 99)
    swing = hi - lo
    if swing < 1e-9:
        return True
    return (float(seg.max()) - float(seg.min())) > 0.4 * swing


def decode_spi(clk, data, cpol=0, cpha=0, msb_first=True, cs=None, bits=8,
               word_gap=10.0, max_missed=8, anchor='auto', auto_mode=False, clk_analog=None):
    """Decode an SPI data line clocked by `clk`. With clock idle level `cpol`, the sampling edge is
    rising iff cpol==cpha, else falling.

    Word framing (priority): a captured `cs` (active-low) frames words and gates edges; else an
    idle-clock gap longer than `word_gap`× the typical bit spacing splits bursts; within a burst
    bytes are grouped every `bits` sampled edges. A gap of 2..`max_missed` bit periods is treated as
    missed clock edges and reconstructed.

    Byte-boundary **anchoring** (no CS): a capture triggered mid-byte shifts every byte if grouped
    forward from the cut start. `anchor`:
      - 'start' — group forward from each burst's start (drop a trailing partial);
      - 'end'   — anchor the first burst to its END (drop the LEADING partial) — correct when the
                  transaction ended cleanly but the capture began mid-byte;
      - 'auto'  — pick 'end' when the clock stopped well before the record end (clean trailing idle)
                  yet was already running at sample 0 (no clean leading idle) ⇒ triggered mid-byte,
                  clean end. Whole-byte bursts decode identically either way.
    `auto_mode=True` ignores cpol/cpha and detects the sampling edge from the signal
    (`detect_sample_rising`) — so a mode-1/2/3 device decodes without specifying the mode.
    Returns [{start, end, value, text, kind}]."""
    c = np.asarray(clk).astype(np.int8)
    dat = np.asarray(data).astype(np.int8)
    N = len(c)
    sample_rising = detect_sample_rising(c, dat) if auto_mode else (cpol == cpha)
    want = 1 if sample_rising else -1
    edges = list(np.flatnonzero(np.diff(c.astype(int)) == want) + 1)
    cs_arr = None if cs is None else np.asarray(cs).astype(int)
    med = float(np.median(np.diff(edges))) if (cs_arr is None and len(edges) > 2) else None

    # Collect sampled bits into bursts (split on CS deselect or an idle-clock gap; reconstruct
    # missed clock edges). Each burst: list of (bit, sample_idx).
    bursts = [[]]

    def add(idx):
        bursts[-1].append((int(dat[min(idx, len(dat) - 1)]), idx))

    def new_burst():
        if bursts[-1]:
            bursts.append([])

    prev_active = True
    prev_edge = None
    for e in edges:
        if cs_arr is not None:
            active = cs_arr[e] == 0
            if not active:
                new_burst(); prev_active = False
                continue
            if not prev_active:
                new_burst()
            prev_active = True
        elif med and prev_edge is not None:
            gap = e - prev_edge
            n = int(round(gap / med)) if med else 1
            if 2 <= n <= max_missed and abs(gap - n * med) <= 0.5 * med:
                # A 2..N-period gap is EITHER missed clock edges (bandwidth-limited clock) OR a real
                # short inter-word idle. Timing can't tell them apart — but the analog clock can: if
                # it stayed flat, it's a real gap → reframe; if it pulsed, reconstruct the missed
                # edge(s). Without the analog, fall back to always-reconstruct (the old behaviour).
                if clk_analog is not None and not _gap_has_pulse(clk_analog, prev_edge, e, med):
                    new_burst()
                else:
                    for k in range(1, n):
                        add(prev_edge + int(round(k * med)))
            elif word_gap and gap > word_gap * med:
                new_burst()
        prev_edge = e
        add(e)
    bursts = [b for b in bursts if b]

    anchor_end = (anchor == 'end')
    if anchor == 'auto' and cs_arr is None and med and len(edges) >= 2:
        clean_start = edges[0] > word_gap * med
        clean_end = (N - edges[-1]) > word_gap * med
        anchor_end = clean_end and not clean_start

    out = []
    for bi, burst in enumerate(bursts):
        if bi == 0 and anchor_end:
            burst = burst[len(burst) % bits:]            # drop the leading partial → end-aligned
        for k in range(0, len(burst) - bits + 1, bits):
            grp = burst[k:k + bits]
            val = 0
            for j, (b, _) in enumerate(grp):
                val = (val << 1) | b if msb_first else val | (b << j)
            out.append({'start': grp[0][1], 'end': grp[-1][1], 'value': val,
                        'text': f"{val:02X}", 'kind': 'byte'})
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
    both = True                                             # auto sampling-edge detection, all 4 modes
    for cpol in (0, 1):
        for cpha in (0, 1):
            clk, dat = _synth_spi(ramp, cpol=cpol, cpha=cpha)
            both &= ([o['value'] for o in decode_spi(clk, dat, auto_mode=True)] == ramp)
    print(f"SPI  auto-mode (4 modes): {'OK' if both else 'FAIL'}")
    ok &= both
    clk, dat = _synth_spi(ramp, spb=10, byte_gap=150)
    got = [o['value'] for o in decode_spi(clk, dat)]
    print(f"SPI  gap-framed  : {len(got)} bytes, {'OK' if got == ramp else 'FAIL'}")
    ok &= (got == ramp)
    cut = 45
    got = [o['value'] for o in decode_spi(clk[cut:], dat[cut:])]
    print(f"SPI  mid-byte cut: {len(got)} bytes, {'OK' if got == ramp[1:] else 'FAIL'}")
    ok &= (got == ramp[1:])
    # End-anchor: a GAPLESS burst triggered mid-byte (no leading idle) but with the clock stopping
    # cleanly at the end. Forward grouping would shift every byte; auto-anchoring to the clean end
    # recovers the ramp tail (the reverse-decode case).
    from decoding.common import ramp_ratio
    clk, dat = _synth_spi(ramp, spb=10, byte_gap=0)
    clk = np.concatenate([clk, np.zeros(400, dtype=int)])   # long trailing idle (clock stopped)
    dat = np.concatenate([dat, np.full(400, dat[-1], dtype=int)])
    cut = 34                                                # begin mid-byte-0, no leading idle
    got = [o['value'] for o in decode_spi(clk[cut:], dat[cut:])]
    good = ramp_ratio(got) == 1.0 and got and got[-1] == 255 and len(got) > 200
    print(f"SPI  end-anchor  : {len(got)} bytes ramp={ramp_ratio(got):.3f} last={got[-1] if got else '-':>3}"
          f", {'OK' if good else 'FAIL'}")
    ok &= good
    # Analog gap disambiguation: from the raw clock waveform, a FLAT gap is a real inter-word idle
    # (reframe) while a gap containing a sub-threshold PULSE is a missed edge (reconstruct).
    flat = np.zeros(40); flat[:2] = 3.3; flat[-2:] = 3.3            # sharp edges at ends, idle between
    pulsed = np.zeros(40); pulsed[:2] = 3.3; pulsed[18:22] = 3.3; pulsed[-2:] = 3.3   # a bump in the middle
    fp_flat = _gap_has_pulse(flat, 0, 40, 10)
    fp_pulse = _gap_has_pulse(pulsed, 0, 40, 10)
    good = (not fp_flat) and fp_pulse
    print(f"SPI  gap-disambig: flat->pulse={fp_flat}(want F) bump->pulse={fp_pulse}(want T), "
          f"{'OK' if good else 'FAIL'}")
    ok &= good
    return ok


if __name__ == '__main__':
    import sys
    sys.exit(0 if selftest() else 1)
