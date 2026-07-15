#!/usr/bin/env python3
"""I²C decoder (SCL clock + SDA data).

START = SDA↓ while SCL high; STOP = SDA↑ while SCL high; bits sampled on SCL rising edges, MSB first,
8 data + 1 ACK per byte. The first byte after a START is the 7-bit address + R/W. Self-framing, so a
gapless (continuous) stream needs no bit-grid tricks — but a capture window that catches no START has
no boundary to lock onto. Run `python3 -m decoding.i2c` to self-test."""
import numpy as np


def decode_i2c(scl, sda):
    """Decode an I²C bus. Returns START/STOP markers plus byte dicts
    [{start, end, value, ack, text, kind}] (kind 'addr' for the first byte after START)."""
    scl = np.asarray(scl).astype(np.int8)
    sda = np.asarray(sda).astype(np.int8)
    n = len(scl)
    events = []
    bits, byte_start = [], None
    in_frame = False
    expect_addr = False
    for i in range(1, n):
        if scl[i] == 1 and scl[i - 1] == 1:
            if sda[i - 1] == 1 and sda[i] == 0:              # START
                events.append({'start': i, 'end': i, 'text': 'S', 'kind': 'start'})
                bits, byte_start = [], None
                in_frame, expect_addr = True, True
                continue
            if sda[i - 1] == 0 and sda[i] == 1:              # STOP
                events.append({'start': i, 'end': i, 'text': 'P', 'kind': 'stop'})
                bits, byte_start, in_frame = [], None, False
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
                if expect_addr:
                    rw = 'R' if (val & 1) else 'W'
                    text = f"{val >> 1:02X}{rw}{'A' if ack else 'N'}"; kind = 'addr'
                    expect_addr = False
                else:
                    text = f"{val:02X}{'A' if ack else 'N'}"; kind = 'byte'
                events.append({'start': byte_start, 'end': i, 'value': val,
                               'ack': ack, 'text': text, 'kind': kind})
                bits, byte_start = [], None
    return events


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
    ramp = list(range(256))
    scl, sda = _synth_i2c(ramp, addr=0x50, rw=0)
    ev = decode_i2c(scl, sda)
    data = [e['value'] for e in ev if e['kind'] == 'byte']
    good = (sum(e['kind'] == 'start' for e in ev) == 1 and sum(e['kind'] == 'stop' for e in ev) == 1
            and data == ramp and all(e['ack'] for e in ev if e['kind'] in ('addr', 'byte')))
    print(f"I2C  addr+ramp: {len(data)} data bytes, {'OK' if good else 'FAIL'}")
    return good


if __name__ == '__main__':
    import sys
    sys.exit(0 if selftest() else 1)
