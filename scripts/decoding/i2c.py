#!/usr/bin/env python3
"""I²C decoder (SCL clock + SDA data).

START = SDA↓ while SCL high; STOP = SDA↑ while SCL high; bits sampled on SCL rising edges, MSB first,
8 data + 1 ACK per byte. The first byte after a START is the 7-bit address + R/W. Self-framing, so a
gapless (continuous) stream needs no bit-grid tricks — but a capture window that catches no START has
no boundary to lock onto. Run `python3 -m decoding.i2c` to self-test."""
import numpy as np


def _i2c_forward(scl, sda):
    """Forward I²C decode: bits counted from each START. Returns START/STOP markers + byte dicts.
    Handles repeated START (labelled 'Sr'), 7-bit and 10-bit (11110XX) addressing, ACK/NACK."""
    n = len(scl)
    events = []
    bits, byte_start = [], None
    in_frame = False
    expect_addr = False      # first byte after START = address
    expect_addr2 = False     # second byte of a 10-bit address
    for i in range(1, n):
        if scl[i] == 1 and scl[i - 1] == 1:
            if sda[i - 1] == 1 and sda[i] == 0:              # START (repeated if already in a transfer)
                events.append({'start': i, 'end': i, 'text': 'Sr' if in_frame else 'S', 'kind': 'start'})
                bits, byte_start = [], None
                in_frame, expect_addr, expect_addr2 = True, True, False
                continue
            if sda[i - 1] == 0 and sda[i] == 1:              # STOP
                events.append({'start': i, 'end': i, 'text': 'P', 'kind': 'stop'})
                bits, byte_start, in_frame, expect_addr2 = [], None, False, False
                continue
        if in_frame and scl[i] == 1 and scl[i - 1] == 0:     # sample on SCL rising
            if byte_start is None:
                byte_start = i
            bits.append(sda[i])
            if len(bits) == 9:
                val = 0
                for b in bits[:8]:
                    val = (val << 1) | b
                ack = (bits[8] == 0)
                nak = '' if ack else 'N'
                if expect_addr and (val & 0xF8) == 0xF0:     # 10-bit address, 1st byte (11110XX + R/W)
                    rw = 'R' if (val & 1) else 'W'
                    text = f"10b{((val >> 1) & 3):01d}xx{rw}{'A' if ack else 'N'}"; kind = 'addr'
                    expect_addr, expect_addr2 = False, True
                elif expect_addr:                            # 7-bit address + R/W
                    rw = 'R' if (val & 1) else 'W'
                    text = f"{val >> 1:02X}{rw}{'A' if ack else 'N'}"; kind = 'addr'
                    expect_addr = False
                elif expect_addr2:                           # 10-bit address, low 8 bits
                    text = f"A{val:02X}{'A' if ack else 'N'}"; kind = 'addr'
                    expect_addr2 = False
                else:                                        # data byte
                    text = f"{val:02X}{'A' if ack else 'N'}"; kind = 'byte'
                events.append({'start': byte_start, 'end': i, 'value': val,
                               'ack': ack, 'text': text, 'kind': kind})
                bits, byte_start = [], None
    return events


def _i2c_end_anchored(scl, sda, events):
    """Fallback for a capture with **no START** (triggered mid-transaction): byte boundaries can't be
    counted from a START, so anchor to the transaction END instead. Group the SCL rising edges into
    9-clock bytes (8 data MSB-first + ACK) counting BACKWARD from the last STOP (or the last clock if
    no STOP), dropping the leading partial. Bytes are emitted as plain data (no addr, since the
    START/address is off-screen). The direct analog of SPI end-anchoring."""
    rises = np.flatnonzero((scl[1:] == 1) & (scl[:-1] == 0)) + 1
    falls = np.flatnonzero((scl[1:] == 0) & (scl[:-1] == 1)) + 1
    stops = [e['start'] for e in events if e['kind'] == 'stop']
    if stops:
        # Anchor to the last COMPLETE clock pulse before the STOP: the STOP releases SDA with SCL held
        # high, adding an extra rising edge that is NOT a data/ACK bit. The last SCL *falling* edge
        # before the STOP ends the real last (ACK) clock; keep only rises before it.
        fb = falls[falls < stops[-1]]
        cutoff = int(fb[-1]) if len(fb) else int(stops[-1])
        usable = rises[rises < cutoff]
    else:
        usable = rises                                          # no STOP → anchor to the last clock
    if len(usable) < 9:
        return events                                            # not even one byte to recover
    usable = usable[len(usable) % 9:]                            # drop leading partial → end-aligned
    bitvals = sda[usable]
    out = [e for e in events if e['kind'] == 'stop']             # keep STOP markers
    for k in range(0, len(usable) - 8, 9):
        b = bitvals[k:k + 9]
        val = 0
        for x in b[:8]:
            val = (val << 1) | int(x)
        ack = (int(b[8]) == 0)
        out.append({'start': int(usable[k]), 'end': int(usable[k + 8]), 'value': val,
                    'ack': ack, 'text': f"{val:02X}{'A' if ack else 'N'}", 'kind': 'byte'})
    out.sort(key=lambda e: e['start'])
    return out


def decode_i2c(scl, sda, anchor='auto'):
    """Decode an I²C bus. Returns START/STOP markers plus byte dicts
    [{start, end, value, ack, text, kind}] (kind 'addr' for the first byte after START).

    `anchor`: 'start' — forward-only (bytes counted from each START). 'auto' (default) — forward when a
    START is captured, else fall back to END-anchored decoding (count bytes backward from the STOP /
    transaction end) so a capture that MISSED the START still recovers its bytes. 'end' — force the
    end-anchored fallback."""
    scl = np.asarray(scl).astype(np.int8)
    sda = np.asarray(sda).astype(np.int8)
    events = _i2c_forward(scl, sda)
    has_start = any(e['kind'] == 'start' for e in events)
    if anchor == 'start' or (anchor == 'auto' and has_start):
        return events
    return _i2c_end_anchored(scl, sda, events)                   # start missing → anchor to the end


# --- self-test -------------------------------------------------------------------
def _synth_i2c(values, spb=10, addr=0x50, rw=0):
    """SCL+SDA: START, addr+RW (+ACK), each data byte (+ACK), STOP."""
    scl, sda = [], []

    def seg(c, d, k):
        scl.extend([c] * k); sda.extend([d] * k)

    seg(1, 1, spb); seg(1, 0, spb)                          # idle, then START
    for byte in [(addr << 1) | rw] + list(values):
        for b in range(7, -1, -1):
            bit = (byte >> b) & 1
            seg(0, bit, spb // 2); seg(1, bit, spb); seg(0, bit, spb // 2)
        seg(0, 0, spb // 2); seg(1, 0, spb); seg(0, 0, spb // 2)    # ACK
    seg(0, 0, spb // 2); seg(1, 0, spb // 2); seg(1, 1, spb)        # STOP
    return np.array(scl, dtype=int), np.array(sda, dtype=int)


def selftest():
    from decoding.common import ramp_ratio
    ok = True
    ramp = list(range(256))
    scl, sda = _synth_i2c(ramp, addr=0x50, rw=0)
    ev = decode_i2c(scl, sda)
    data = [e['value'] for e in ev if e['kind'] == 'byte']
    good = (sum(e['kind'] == 'start' for e in ev) == 1 and sum(e['kind'] == 'stop' for e in ev) == 1
            and data == ramp and all(e['ack'] for e in ev if e['kind'] in ('addr', 'byte')))
    print(f"I2C  addr+ramp: {len(data)} data bytes, {'OK' if good else 'FAIL'}")
    ok &= good
    # Start missing: a capture triggered mid-transaction (no START/address) but with the STOP in view.
    # End-anchoring counts bytes backward from the STOP and recovers the data-byte tail.
    scl, sda = _synth_i2c(list(range(64)), addr=0x50, rw=0)
    cut = len(scl) // 3                                      # begin mid-data → no START captured
    ev = decode_i2c(scl[cut:], sda[cut:])
    data = [e['value'] for e in ev if e['kind'] == 'byte']
    good = (not any(e['kind'] == 'start' for e in ev) and ramp_ratio(data) == 1.0
            and data and data[-1] == 63 and len(data) > 20)
    print(f"I2C  start-missing: {len(data)} bytes ramp={ramp_ratio(data):.3f} "
          f"last={data[-1] if data else '-':>3}, {'OK' if good else 'FAIL'}")
    ok &= good
    return ok


if __name__ == '__main__':
    import sys
    sys.exit(0 if selftest() else 1)
