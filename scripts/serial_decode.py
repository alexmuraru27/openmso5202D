#!/usr/bin/env python3
"""
Serial-protocol decoders (UART / SPI / I²C) for MSO5202D captures — pure logic,
no USB. Given digital (0/1) traces recovered from the scope's two analog
channels, reconstruct the bytes on the wire.

Why analog → digital: the scope's logic-analyzer channels are not readable over
USB (see MSO5202D-protocol.md §5), so serial decoding thresholds the two ANALOG
channels (CH1/CH2) into logic levels. That caps us at 2 signals per capture:
  - UART  : 1 line  (the data line)
  - SPI   : 2 lines (SCLK + one data line, MOSI or MISO)
  - I²C   : 2 lines (SCL + SDA)
and at the fixed 3840-sample record (no deep memory) → short messages only.

Everything here is deterministic and hardware-free: run `python3 serial_decode.py`
to synthesize the 0x00..0xFF ramp in each protocol and assert it round-trips.
`mso5202d_decode.py` calls threshold()+decode_*() on real frozen captures.

Each decode_*() returns a list of annotation dicts the viewer can draw directly:
    {'start': sample_idx, 'end': sample_idx, 'text': str, 'kind': str, ...}
plus decoder-specific keys ('value', 'ok', 'ack').
"""
import numpy as np

# to_divs (byte → divisions) is the scope's vertical model; reuse it so the
# threshold sees the same "up = higher voltage" trace the plotter draws.
from mso5202d_plot import to_divs


# --- analog → digital ------------------------------------------------------------
def _schmitt(sig, lo_th, hi_th, start_high=None):
    """Schmitt-trigger a continuous signal into a boolean logic trace. Two
    thresholds give hysteresis so ringing/noise near the midpoint doesn't chatter
    into phantom edges."""
    n = len(sig)
    out = np.empty(n, dtype=bool)
    state = (sig[0] > (lo_th + hi_th) / 2) if start_high is None else start_high
    for i in range(n):
        v = sig[i]
        if state and v < lo_th:
            state = False
        elif not state and v > hi_th:
            state = True
        out[i] = state
    return out


def threshold(y_bytes, pos, frac=0.5, hysteresis=0.3):
    """Raw waveform bytes + the channel's VERT-POS → boolean logic trace.

    Unwraps to divisions (to_divs), estimates the low/high rails robustly with
    5th/95th percentiles, then Schmitt-triggers at `frac` of the way between them
    with a `hysteresis` band (fraction of the swing). Returns a bool ndarray
    (True = logic high). `hysteresis=0` gives a plain comparator."""
    y_bytes = np.asarray(y_bytes, dtype=np.uint8)
    if not len(y_bytes):
        return np.zeros(0, dtype=bool)
    d = to_divs(y_bytes, pos)
    lo, hi = np.percentile(d, 5), np.percentile(d, 95)
    if hi - lo < 1e-9:                    # flat line — no signal
        return np.zeros(len(d), dtype=bool)
    mid = lo + (hi - lo) * frac
    band = (hi - lo) * hysteresis / 2
    return _schmitt(d, mid - band, mid + band)


def _edges(d):
    """Indices where the level changes (edge lands on the new-level sample)."""
    return np.flatnonzero(np.diff(d.astype(int)) != 0) + 1


def _min_pulse(d):
    """Shortest constant run in samples ≈ one bit period (for baud auto-detect)."""
    e = _edges(d)
    return int(np.min(np.diff(e))) if len(e) >= 2 else 0


# --- UART ------------------------------------------------------------------------
def decode_uart(digital, sample_interval_ns=None, baud=None,
                bits=8, parity='none', stops=1, idle=1):
    """Decode an asynchronous UART line (idle high, LSB first, 1 start bit).

    Provide `baud` (with `sample_interval_ns`) to lock the bit period, or leave
    both to auto-detect it from the shortest pulse. `parity`: 'none'|'even'|'odd'.
    Returns frames [{start, end, value, ok, text, kind}]; `ok` is the framing
    check (parity + stop bits valid)."""
    d = np.asarray(digital).astype(int)
    n = len(d)
    if baud and sample_interval_ns:
        spb = (1e9 / baud) / sample_interval_ns
    else:
        spb = _min_pulse(d)
    if not spb or spb < 1:
        return []
    frames = []
    i = 0
    while i < n - 1:
        # A start bit is idle→!idle (falling edge for the usual idle-high line).
        if d[i] == idle and d[i + 1] == (1 - idle):
            start = i + 1

            def sample(k):               # center of bit k (start bit = 0)
                idx = int(round(start + (k + 0.5) * spb))
                return d[idx] if idx < n else idle

            if sample(0) != (1 - idle):  # start bit not actually low — noise
                i += 1
                continue
            val = 0
            for b in range(bits):
                if sample(1 + b):
                    val |= (1 << b)      # LSB first
            k = 1 + bits
            ok = True
            if parity != 'none':
                p = sample(k); k += 1
                ones = bin(val).count('1') + p
                ok = (ones % 2 == 0) if parity == 'even' else (ones % 2 == 1)
            if not all(sample(k + j) == idle for j in range(stops)):
                ok = False               # stop bit(s) must return to idle
            end = int(round(start + (k + stops) * spb))
            frames.append({'start': start, 'end': min(end, n - 1),
                           'value': val, 'ok': ok,
                           'text': f"{val:02X}" + ('' if ok else '!'),
                           'kind': 'byte'})
            i = end
        else:
            i += 1
    return frames


# --- SPI -------------------------------------------------------------------------
def decode_spi(clk, data, cpol=0, cpha=0, msb_first=True, cs=None, bits=8,
               word_gap=1.8):
    """Decode an SPI data line clocked by `clk`. Data is sampled on the leading
    (cpha=0) or trailing (cpha=1) clock edge; with clock idle level `cpol` that
    resolves to the rising edge iff cpol==cpha, else the falling edge.

    Word framing (in priority order):
      - `cs` (active-low chip-select trace), if given, frames words and gates
        edges — the robust way when a CS line is captured;
      - else if `word_gap` is set, a clock gap longer than `word_gap`× the
        typical bit spacing starts a new word (so a bit-banged stream with an
        idle gap between bytes re-aligns even if the capture starts mid-byte);
      - else bytes are simply grouped every `bits` sampled edges.
    Returns [{start, end, value, text, kind}]."""
    c = np.asarray(clk).astype(int)
    dat = np.asarray(data).astype(int)
    sample_rising = (cpol == cpha)
    want = 1 if sample_rising else -1
    edges = list(np.flatnonzero(np.diff(c) == want) + 1)
    cs_arr = None if cs is None else np.asarray(cs).astype(int)
    # Typical intra-byte edge spacing, for gap-based re-framing (CS takes over).
    med = float(np.median(np.diff(edges))) if (cs_arr is None and len(edges) > 2) else None

    out = []
    buf, pos = [], []
    prev_active = True
    prev_edge = None
    for e in edges:
        if cs_arr is not None:
            active = cs_arr[e] == 0          # active-low CS asserted
            if not active:
                buf, pos = [], []            # deselected — drop partial word
                prev_active = False
                continue
            if not prev_active:              # just (re)selected — fresh word
                buf, pos = [], []
            prev_active = True
        elif med and word_gap and prev_edge is not None and (e - prev_edge) > word_gap * med:
            buf, pos = [], []                # idle gap → drop partial, start a new word
        prev_edge = e
        buf.append(dat[e]); pos.append(e)
        if len(buf) == bits:
            val = 0
            for j, b in enumerate(buf):
                val = (val << 1) | b if msb_first else val | (b << j)
            out.append({'start': pos[0], 'end': pos[-1], 'value': val,
                        'text': f"{val:02X}", 'kind': 'byte'})
            buf, pos = [], []
    return out


# --- I²C -------------------------------------------------------------------------
def decode_i2c(scl, sda):
    """Decode an I²C bus (SCL clock + SDA data). START = SDA falling while SCL
    high; STOP = SDA rising while SCL high; bits sampled on SCL rising edges,
    MSB first, 8 data bits + 1 ACK per byte (ACK = SDA low on the 9th clock).
    The first byte after a START is the 7-bit address + R/W.

    Returns events: START/STOP markers plus byte dicts
    [{start, end, value, ack, text, kind}] (kind 'addr' for the first byte)."""
    scl = np.asarray(scl).astype(int)
    sda = np.asarray(sda).astype(int)
    n = len(scl)
    events = []
    bits, byte_start = [], None
    in_frame = False
    expect_addr = False
    for i in range(1, n):
        # START / STOP: SDA transition while SCL is held high.
        if scl[i] == 1 and scl[i - 1] == 1:
            if sda[i - 1] == 1 and sda[i] == 0:          # SDA ↓, SCL high → START
                events.append({'start': i, 'end': i, 'text': 'S', 'kind': 'start'})
                bits, byte_start = [], None
                in_frame, expect_addr = True, True
                continue
            if sda[i - 1] == 0 and sda[i] == 1:          # SDA ↑, SCL high → STOP
                events.append({'start': i, 'end': i, 'text': 'P', 'kind': 'stop'})
                bits, byte_start, in_frame = [], None, False
                continue
        # Sample a bit on each SCL rising edge inside a frame.
        if in_frame and scl[i] == 1 and scl[i - 1] == 0:
            if byte_start is None:
                byte_start = i
            bits.append(sda[i])
            if len(bits) == 9:
                val = 0
                for b in bits[:8]:
                    val = (val << 1) | b
                ack = (bits[8] == 0)
                if expect_addr:
                    rw = 'R' if (val & 1) else 'W'
                    text = f"{val >> 1:02X}{rw}{'A' if ack else 'N'}"
                    kind = 'addr'
                    expect_addr = False
                else:
                    text = f"{val:02X}{'A' if ack else 'N'}"
                    kind = 'byte'
                events.append({'start': byte_start, 'end': i, 'value': val,
                               'ack': ack, 'text': text, 'kind': kind})
                bits, byte_start = [], None
    return events


# --- self-test (no hardware) -----------------------------------------------------
def _synth_uart(values, spb=20, parity='none', gap=8):
    """Build an idle-high UART digital trace for `values` (LSB-first, 1 start,
    1 stop) at `spb` samples/bit, with `gap` idle bits between frames."""
    bitstream = []
    for v in values:
        frame = [0] + [(v >> b) & 1 for b in range(8)]     # start + 8 data LSB-first
        if parity != 'none':
            ones = bin(v).count('1')
            frame.append((ones % 2) if parity == 'even' else (1 - ones % 2))
        frame.append(1)                                     # stop
        bitstream += frame + [1] * gap
    bitstream = [1] * gap + bitstream
    return np.repeat(np.array(bitstream, dtype=int), spb)


def _synth_spi(values, spb=10, cpol=0, cpha=0, msb_first=True, byte_gap=0):
    """Build SCLK+MOSI traces clocking out `values`, MSB-first, mode (cpol,cpha).
    `byte_gap` inserts that many idle-clock samples between bytes (to test the
    gap-based word re-framing)."""
    clk, dat = [], []
    idle = cpol
    lead = (1 - cpol)                                       # clock active level

    def hold(line_c, line_d, k):
        clk.extend([line_c] * k); dat.extend([line_d] * k)

    cur = 0
    hold(idle, 0, spb)                                      # quiet lead-in
    for v in values:
        order = range(7, -1, -1) if msb_first else range(8)
        for b in order:
            bit = (v >> b) & 1
            if cpha == 0:
                cur = bit                                   # data valid before leading edge
                hold(idle, cur, spb // 2)                   # leading edge coming
                hold(lead, cur, spb // 2)                   # sampled here
            else:
                hold(idle, cur, spb // 2)                   # leading edge (no sample)
                cur = bit
                hold(lead, cur, spb // 2)                   # data set; trailing edge samples
            clk[-1] = lead
        if byte_gap:
            hold(idle, cur, byte_gap)                       # idle clock between bytes
    hold(idle, cur, spb)
    return np.array(clk, dtype=int), np.array(dat, dtype=int)


def _synth_i2c(values, spb=10, addr=0x50, rw=0):
    """Build SCL+SDA traces: START, addr+RW (+ACK), each data byte (+ACK), STOP."""
    scl, sda = [], []

    def seg(c, d, k):
        scl.extend([c] * k); sda.extend([d] * k)

    seg(1, 1, spb)                                          # idle
    seg(1, 0, spb)                                          # START: SDA↓ while SCL high
    frames = [(addr << 1) | rw] + list(values)
    for fi, byte in enumerate(frames):
        for b in range(7, -1, -1):                          # 8 data bits MSB-first
            bit = (byte >> b) & 1
            seg(0, bit, spb // 2)                           # SDA set while SCL low
            seg(1, bit, spb)                                # SCL high (sample)
            seg(0, bit, spb // 2)                           # SCL low
        seg(0, 0, spb // 2)                                 # ACK: master pulls SDA low
        seg(1, 0, spb)                                      # SCL high (ACK sampled)
        seg(0, 0, spb // 2)
    seg(0, 0, spb // 2)                                     # prep STOP: SDA low, SCL low→high
    seg(1, 0, spb // 2)
    seg(1, 1, spb)                                          # STOP: SDA↑ while SCL high
    return np.array(scl, dtype=int), np.array(sda, dtype=int)


def _to_bytes_trace(digital, pos=0, amp_div=1.0):
    """Wrap a 0/1 digital trace back into scope-style waveform bytes so the test
    also exercises threshold(): byte = (pos + 16 + signal) mod 256, high/low at
    ±amp_div divisions (25 counts/div)."""
    d = np.asarray(digital).astype(int)
    sig = (d * 2 - 1) * (amp_div * 25)                      # ±amp_div div in counts
    return ((pos + 16 + sig) % 256).astype(np.uint8)


def _selftest():
    ramp = list(range(256))
    ok = True

    # UART — through threshold() (analog round-trip) at a couple of configs.
    for parity in ('none', 'even', 'odd'):
        dig = _synth_uart(ramp, spb=20, parity=parity)
        bytes_trace = _to_bytes_trace(dig, pos=30)
        logic = threshold(bytes_trace, pos=30)
        frames = decode_uart(logic, parity=parity)
        got = [f['value'] for f in frames]
        bad = [f for f in frames if not f['ok']]
        good = (got == ramp and not bad)
        print(f"UART parity={parity:4}: {len(got)} bytes, "
              f"{'OK' if good else 'FAIL'}")
        ok &= good

    # SPI — all four modes, MSB and LSB first.
    for cpol in (0, 1):
        for cpha in (0, 1):
            for msb in (True, False):
                clk, dat = _synth_spi(ramp, cpol=cpol, cpha=cpha, msb_first=msb)
                out = decode_spi(clk, dat, cpol=cpol, cpha=cpha, msb_first=msb)
                got = [o['value'] for o in out]
                good = (got == ramp)
                print(f"SPI  mode{cpol}{cpha} {'MSB' if msb else 'LSB'}: "
                      f"{len(got)} bytes, {'OK' if good else 'FAIL'}")
                ok &= good

    # SPI — gap-framed stream, and the same truncated at a mid-byte start (the
    # real-capture hazard): the leading partial byte must be dropped, rest intact.
    clk, dat = _synth_spi(ramp, spb=10, byte_gap=20)         # ≥2 bit-period idle gap
    got = [o['value'] for o in decode_spi(clk, dat)]
    good = (got == ramp)
    print(f"SPI  gap-framed  : {len(got)} bytes, {'OK' if good else 'FAIL'}")
    ok &= good
    cut = 45                                                 # start partway into byte 0
    got = [o['value'] for o in decode_spi(clk[cut:], dat[cut:])]
    good = (got == ramp[1:])                                 # byte 0 fragment dropped
    print(f"SPI  mid-byte cut: {len(got)} bytes, {'OK' if good else 'FAIL'}")
    ok &= good

    # I²C — address + data ramp, ACKed.
    scl, sda = _synth_i2c(ramp, addr=0x50, rw=0)
    ev = decode_i2c(scl, sda)
    starts = [e for e in ev if e['kind'] == 'start']
    stops = [e for e in ev if e['kind'] == 'stop']
    addrs = [e for e in ev if e['kind'] == 'addr']
    data = [e['value'] for e in ev if e['kind'] == 'byte']
    good = (len(starts) == 1 and len(stops) == 1 and len(addrs) == 1
            and addrs[0]['value'] == (0x50 << 1) and data == ramp
            and all(e['ack'] for e in ev if e['kind'] in ('addr', 'byte')))
    print(f"I2C  addr+ramp: {len(data)} data bytes, "
          f"{'OK' if good else 'FAIL'}")
    ok &= good

    print("\n" + ("ALL PASS" if ok else "FAILURES ABOVE"))
    return 0 if ok else 1


if __name__ == '__main__':
    import sys
    sys.exit(_selftest())
