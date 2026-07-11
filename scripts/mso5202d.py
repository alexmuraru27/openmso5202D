#!/usr/bin/env python3
"""
MSO5202D driver library — reusable transport + protocol for the Hantek MSO5202D
(USB 049f:505a). See MSO5202D-protocol.md for the reverse-engineered spec.

Confirmed working recipe (Linux): detach cdc_subset -> dev.reset() -> re-detach
-> claim -> clear_halt -> transact with the bulk IN read posted BEFORE the OUT
write. File reads return a content frame (subtype 0x01) + an end-marker frame
(subtype 0x02) — both must be consumed. A persistent RX buffer keeps frames aligned.

Needs a udev rule granting access to 049f:505a to run without root (see
70-mso5202d.rules); otherwise run as root.
"""
import struct, time, threading
import usb.core, usb.util

VID, PID = 0x049F, 0x505A
EP_OUT, EP_IN = 0x02, 0x81
# Transport: the device only replies if a bulk-IN read is already posted when the
# OUT is written. We start a reader thread, wait until it signals it is about to
# read (Event), then leave TRANSACT_POST_S for the IN URB to actually reach the
# kernel before writing. Below ~12 ms the write races the read and the device
# stops replying (transactions time out / retry). 15 ms is measured reliable with
# headroom, and latency is USB-round-trip-limited from there down, so lower is no
# faster. Re-tune with scripts/tune_transact.py if you change hosts.
TRANSACT_POST_S = 0.015

def build(payload: bytes) -> bytes:
    hdr = b'\x53' + struct.pack('<H', len(payload) + 1) + payload
    return hdr + bytes([sum(hdr) & 0xFF])

def verify(frame: bytes) -> bytes:
    """Validate a full frame; return payload = selectorEcho | subtype | data..."""
    if len(frame) < 5 or frame[0] != 0x53:
        raise ValueError(f"bad SOF: {frame[:8].hex()}")
    length = struct.unpack_from('<H', frame, 1)[0]
    if length != len(frame) - 3:
        raise ValueError(f"length field={length} actual={len(frame)-3}")
    if (sum(frame[:-1]) & 0xFF) != frame[-1]:
        raise ValueError("checksum mismatch")
    return frame[3:-1]


class Scope:
    def __init__(self, reset=True):
        self._rx = bytearray()
        self.dev = usb.core.find(idVendor=VID, idProduct=PID)
        if self.dev is None:
            raise RuntimeError("MSO5202D (049f:505a) not found — plugged in?")
        self._detach()
        if reset:
            try:
                self.dev.reset()
                time.sleep(1.0)
                self._detach()
            except usb.core.USBError:
                pass
        usb.util.claim_interface(self.dev, 0)
        for ep in (EP_OUT, EP_IN):
            try: self.dev.clear_halt(ep)
            except usb.core.USBError: pass

    def close(self):
        try: usb.util.release_interface(self.dev, 0)
        except Exception: pass

    def _detach(self):
        try:
            if self.dev.is_kernel_driver_active(0):
                self.dev.detach_kernel_driver(0)
        except (NotImplementedError, usb.core.USBError):
            pass

    def _recv(self, timeout) -> bytes:
        while len(self._rx) < 3:
            self._rx += bytes(self.dev.read(EP_IN, 512, timeout=timeout))
        total = struct.unpack_from('<H', self._rx, 1)[0] + 3
        while len(self._rx) < total:
            self._rx += bytes(self.dev.read(EP_IN, 512, timeout=timeout))
        frame = bytes(self._rx[:total]); del self._rx[:total]
        return verify(frame)

    def _resync(self):
        """Discard any partial/stale bytes so the next frame starts on a clean
        boundary. Called after a timeout or a bad frame — otherwise leftover
        bytes in _rx cascade into repeated failures. Drains in large chunks with a
        generous cap because an interrupted big read (a file-read or framebuffer
        grab) can leave hundreds of KB queued; keep reading until the endpoint runs
        dry (a read timeout = empty)."""
        self._rx.clear()
        # Drain in big chunks with a short timeout so a desync costs ~1-2 s, not tens of
        # seconds: an interrupted big read (file/framebuffer) can queue hundreds of KB, but
        # 64 KB reads clear ~1 MB in ~16 iterations. Keep reading until the endpoint times out
        # (empty) — don't stop on a partial read, since the scope dribbles frames and a
        # premature stop would leave a trailing frame that re-desyncs the next transact.
        for _ in range(64):                      # bounded (~4 s worst case) but exits when dry
            try:
                if not self.dev.read(EP_IN, 65536, timeout=60):
                    break
            except usb.core.USBError:
                break   # timeout = nothing more pending

    def _transact_once(self, payload: bytes, timeout) -> bytes:
        out = {}
        posted = threading.Event()
        def reader():
            posted.set()                                  # about to post the bulk-IN read
            try: out['f'] = self._recv(timeout)
            except Exception as e: out['e'] = e
        t = threading.Thread(target=reader, daemon=True); t.start()
        posted.wait(0.5)                                  # reader is up and about to read
        time.sleep(TRANSACT_POST_S)                       # margin for the IN URB to post
        try:
            self.dev.write(EP_OUT, build(payload), timeout=2000)
        except usb.core.USBError as e:
            out.setdefault('e', e)
        t.join(timeout / 1000 + 1.5)                      # reader has finished by now
        if 'f' in out:
            return out['f']
        raise out.get('e', TimeoutError("no response"))

    def transact(self, payload: bytes, timeout=3000, retries=2) -> bytes:
        last = None
        for _ in range(retries + 1):
            try:
                return self._transact_once(payload, timeout)
            except Exception as e:
                last = e
                self._resync()          # clear desync before retrying
        raise last

    # --- high-level ops ----------------------------------------------------
    def read_file(self, path: str, timeout=4000) -> bytes:
        """Read a file from the scope's embedded Linux over USB (selector 0x10).

        The reply is multi-frame: any number of **content** frames (echo 0x90,
        subtype 0x01) terminated by an **end-marker** frame (subtype 0x02). A
        single 'S' frame caps at 64 KB (16-bit length), so large files span many
        content frames — loop until the end-marker. Verified byte-perfect and fast
        on hardware (911 KB `/help.db` in ~1.1 s ≈ 800 KB/s), so this is a viable
        way to pull large files — e.g. a deep Save-CSV `WaveData*.csv` off an
        inserted card without removing it (see MSO5202D-protocol.md §5)."""
        frame = self.transact(b'\x10\x00' + path.encode(), timeout=timeout)
        data = bytearray()
        for _ in range(1_000_000):          # bounded loop over content frames
            st = frame[1] if len(frame) > 1 else 0xFF
            if st == 0x01:
                data += frame[2:]
            elif st == 0x02:                # end-marker — done
                break
            try:
                frame = self._recv(timeout)
            except Exception:
                break
        return bytes(data)

    def read_settings(self, timeout=3000, retries=2) -> bytes:
        """Poll selector 0x01 -> settings payload: selectorEcho 0x81 followed
        directly by the 213 /protocol.inf parameter bytes (no subtype byte —
        resolved 2026-07-08, see MSO5202D-protocol.md §6). Feed to
        decode_settings(). A live viewer can pass a short timeout + retries=0 to
        fail fast when the scope is busy."""
        return self.transact(b'\x01', timeout=timeout, retries=retries)

    def read_waveform(self, ch=0, retries=2, timeout=2000) -> bytes:
        """Acquire one 3840-sample block. ch: 0 = CH1, 1 = CH2 — the channel is
        selected by the ACQUIRE value byte (02 01 <ch>), NOT by param 0x12
        (which the vendor app toggles 1->0 around each refresh; run/hold?).
        Verified on hardware 2026-07-08: with CH2's probe disconnected,
        02 01 00 returns CH1's square wave and 02 01 01 returns CH2's flat line.

        `timeout` (ms) and `retries` bound how long a read can block. The inner
        transactions use retries=0 so retrying is governed here — a live viewer
        can pass a short timeout + retries=0 to fail fast and skip the frame when
        the scope is busy (e.g. a knob being turned), instead of hanging on
        seconds of nested retries."""
        # The acquire (0x02) reply can legitimately take a while, but once it has
        # answered, the data frames arrive within ~20 ms or not at all (the scope
        # dropped them — typically because a knob is being turned and it is
        # reconfiguring). So wait generously for the acquire, but fail the data
        # frames fast so a disrupted read costs ~150 ms, not the full timeout.
        data_to = min(timeout, 150)
        for _ in range(retries + 1):
            try:
                self.transact(bytes([0x12, 0x01, 0x00]), timeout=timeout, retries=0)
                frame = self.transact(bytes([0x02, 0x01, ch]), timeout=timeout, retries=0)
                data = b''
                for _ in range(5):
                    st = frame[1] if len(frame) > 1 else 0xFF
                    if st == 0x01: data = frame[3:]
                    elif st == 0x02: break
                    frame = self._recv(data_to)
                if data:
                    return data
            except Exception:
                pass
            self._resync()
        return b''

    # Acquire channel-code map (the value byte of `02 01 <code>`), decoded from
    # the vendor app: 0=CH1, 1=CH2, 2=Math (1 byte/sample each), 5=LA. Codes 3/4
    # are not usable channels (3 replies empty, 4 is a dual-analog block); 6+ are
    # invalid and desync the link. See MSO5202D-protocol.md §5.
    LA_CHANNEL_CODE = 0x05

    def read_la(self, retries=2, timeout=2000) -> bytes:
        """Acquire one Logic-Analyzer block: `02 01 05` -> 3840 samples, **2 bytes
        per sample** (little-endian 16-bit word; bit N = channel D(N)), i.e. 7680
        bytes. Unlike the analog channels (1 byte/sample) LA packs all 16 digital
        channels into each word. Returns the raw 7680-byte payload (decode with
        decode_la); b'' if the read fails or LA is off.

        ⚠ UNSAFE / UNRELIABLE — DO NOT USE in normal operation. Channel-code 5 is
        a half-wired firmware path: it returns coherent data only at slow
        timebases, mostly a 2-state pattern otherwise, AND it **corrupts the
        scope's own on-screen LA display** while reading. The frame *format* above
        is correct, but the firmware doesn't serve real LA samples over USB. For a
        live LA view use the 0x20 framebuffer (the scope's rendered screen)
        instead. Kept only for RE experiments."""
        data_to = min(timeout, 200)
        for _ in range(retries + 1):
            try:
                self.transact(bytes([0x12, 0x01, 0x00]), timeout=timeout, retries=0)
                frame = self.transact(bytes([0x02, 0x01, self.LA_CHANNEL_CODE]),
                                      timeout=timeout, retries=0)
                data = b''
                for _ in range(6):
                    st = frame[1] if len(frame) > 1 else 0xFF
                    if st == 0x01: data = frame[3:]
                    elif st == 0x02: break
                    frame = self._recv(data_to)
                if data:
                    return data
            except Exception:
                pass
            self._resync()
        return b''


# /protocol.inf parameter list (name, byte width) in wire order. The settings
# payload is exactly: selectorEcho 0x81 | these 213 bytes — no subtype, no
# prefix. Alignment resolved 2026-07-08 by diffing a CH1-V/div knob sweep
# (scope_dump/captures_wireshark/mso5202d-ch1-vdiv.pcapng); see MSO5202D-protocol.md §6.
SETTINGS_PARAMS = [
    ('VERT-CH1-DISP',1),('VERT-CH1-VB',1),('VERT-CH1-COUP',1),('VERT-CH1-20MHZ',1),
    ('VERT-CH1-FINE',1),('VERT-CH1-PROBE',1),('VERT-CH1-RPHASE',1),('VERT-CH1-CNT-FINE',1),
    ('VERT-CH1-POS',2),
    ('VERT-CH2-DISP',1),('VERT-CH2-VB',1),('VERT-CH2-COUP',1),('VERT-CH2-20MHZ',1),
    ('VERT-CH2-FINE',1),('VERT-CH2-PROBE',1),('VERT-CH2-RPHASE',1),('VERT-CH2-CNT-FINE',1),
    ('VERT-CH2-POS',2),
    ('TRIG-STATE',1),('TRIG-TYPE',1),('TRIG-SRC',1),('TRIG-MODE',1),('TRIG-COUP',1),
    ('TRIG-VPOS',2),('TRIG-FREQUENCY',8),
    ('TRIG-HOLDTIME-MIN',8),('TRIG-HOLDTIME-MAX',8),('TRIG-HOLDTIME',8),
    ('TRIG-EDGE-SLOPE',1),
    ('TRIG-VIDEO-NEG',1),('TRIG-VIDEO-PAL',1),('TRIG-VIDEO-SYN',1),('TRIG-VIDEO-LINE',2),
    ('TRIG-PULSE-NEG',1),('TRIG-PULSE-WHEN',1),('TRIG-PULSE-TIME',8),
    ('TRIG-SLOPE-SET',1),('TRIG-SLOPE-WIN',1),('TRIG-SLOPE-WHEN',1),
    ('TRIG-SLOPE-V1',2),('TRIG-SLOPE-V2',2),('TRIG-SLOPE-TIME',8),
    ('TRIG-SWAP-CH1-TYPE',1),('TRIG-SWAP-CH1-MODE',1),('TRIG-SWAP-CH1-COUP',1),
    ('TRIG-SWAP-CH1-EDGE-SLOPE',1),('TRIG-SWAP-CH1-VIDEO-NEG',1),('TRIG-SWAP-CH1-VIDEO-PAL',1),
    ('TRIG-SWAP-CH1-VIDEO-SYN',1),('TRIG-SWAP-CH1-VIDEO-LINE',2),
    ('TRIG-SWAP-CH1-PULSE-NEG',1),('TRIG-SWAP-CH1-PULSE-WHEN',1),('TRIG-SWAP-CH1-PULSE-TIME',8),
    ('TRIG-SWAP-CH1-OVERTIME-NEG',1),('TRIG-SWAP-CH1-OVERTIME-TIME',8),
    ('TRIG-SWAP-CH2-TYPE',1),('TRIG-SWAP-CH2-MODE',1),('TRIG-SWAP-CH2-COUP',1),
    ('TRIG-SWAP-CH2-EDGE-SLOPE',1),('TRIG-SWAP-CH2-VIDEO-NEG',1),('TRIG-SWAP-CH2-VIDEO-PAL',1),
    ('TRIG-SWAP-CH2-VIDEO-SYN',1),('TRIG-SWAP-CH2-VIDEO-LINE',2),
    ('TRIG-SWAP-CH2-PULSE-NEG',1),('TRIG-SWAP-CH2-PULSE-WHEN',1),('TRIG-SWAP-CH2-PULSE-TIME',8),
    ('TRIG-SWAP-CH2-OVERTIME-NEG',1),('TRIG-SWAP-CH2-OVERTIME-TIME',8),
    ('TRIG-OVERTIME-NEG',1),('TRIG-OVERTIME-TIME',8),
    ('HORIZ-TB',1),('HORIZ-WIN-TB',1),('HORIZ-WIN-STATE',1),('HORIZ-TRIGTIME',8),
    ('MATH-DISP',1),('MATH-MODE',1),('MATH-FFT-SRC',1),('MATH-FFT-WIN',1),
    ('MATH-FFT-FACTOR',1),('MATH-FFT-DB',1),
    ('DISPLAY-MODE',1),('DISPLAY-PERSIST',1),('DISPLAY-FORMAT',1),('DISPLAY-CONTRAST',1),
    ('DISPLAY-MAXCONTRAST',1),('DISPLAY-GRID-KIND',1),('DISPLAY-GRID-BRIGHT',1),
    ('DISPLAY-MAXGRID-BRIGHT',1),
    ('ACQURIE-MODE',1),('ACQURIE-AVG-CNT',1),('ACQURIE-TYPE',1),('ACQURIE-STORE-DEPTH',1),
    ('MEASURE-ITEM1-SRC',1),('MEASURE-ITEM1',1),('MEASURE-ITEM2-SRC',1),('MEASURE-ITEM2',1),
    ('MEASURE-ITEM3-SRC',1),('MEASURE-ITEM3',1),('MEASURE-ITEM4-SRC',1),('MEASURE-ITEM4',1),
    ('MEASURE-ITEM5-SRC',1),('MEASURE-ITEM5',1),('MEASURE-ITEM6-SRC',1),('MEASURE-ITEM6',1),
    ('MEASURE-ITEM7-SRC',1),('MEASURE-ITEM7',1),('MEASURE-ITEM8-SRC',1),('MEASURE-ITEM8',1),
    ('CONTROL-TYPE',1),('CONTROL-MENUID',1),('CONTROL-DISP-MENU',1),
    ('LA-SWI',1),('LA-CHANNEL-STATE',2),('LA-CURRENT-CHANNEL',1),
    ('LA-D7-D0-THRESHOLD-TYPE',1),('LA-D15-D8-THRESHOLD-TYPE',1),
    ('LA-D7-D0-USER-THRESHOLD-VOLT',2),('LA-D15-D8-USER-THRESHOLD-VOLT',2),
]
assert sum(w for _, w in SETTINGS_PARAMS) == 213    # == /protocol.inf [TOTAL]

# Units (verified on hardware, see MSO5202D-protocol.md §6):
#  - 8-byte time fields (HOLDTIME*, *-TIME, HORIZ-TRIGTIME) are PICOSECONDS
#    (HOLDTIME-MIN 100000 = 100 ns, MAX 1e13 = 10 s = the holdoff limits).
#  - VERT-CHx-POS and TRIG-VPOS are signed 1/25-DIVISION units (so ±200 =
#    ±8 div = the manual's trigger-level range). Trigger level in volts =
#    (TRIG-VPOS - POS_src) * vdiv / 25 — verified against the scope readout
#    (±200 -> +13.4 V / -18.5 V at 2 V/div, POS +32).
#  - TRIG-FREQUENCY is mHz (frequency counter).
DIV_UNIT = 25          # settings position/level fields are in 1/25-division units
# Multi-byte SIGNED fields: positions/levels (2-byte, 1/25 div) and the
# horizontal delay HORIZ-TRIGTIME (8-byte ps) — the delay goes negative
# (post-trigger), which otherwise decodes as a huge ~2^64 value.
_SIGNED = {'VERT-CH1-POS', 'VERT-CH2-POS', 'TRIG-VPOS', 'TRIG-SLOPE-V1',
           'TRIG-SLOPE-V2', 'HORIZ-TRIGTIME', 'LA-D7-D0-USER-THRESHOLD-VOLT',
           'LA-D15-D8-USER-THRESHOLD-VOLT'}

# VERT-CHx-VB index -> mV/div, verified on hardware over a full 2mV..10V sweep.
# Quirk: 10 V/div also stores VB=0 (wraps mod 11) — at that setting TRIG-VPOS
# can disambiguate if the trigger level is nonzero.
VB_TO_MV = {0: 2, 1: 5, 2: 10, 3: 20, 4: 50, 5: 100,
            6: 200, 7: 500, 8: 1000, 9: 2000, 10: 5000}

# HORIZ-TB / HORIZ-WIN-TB index -> time/div in ns. 2-4-8 sequence over the
# scope's 2 ns..40 s range: 32 steps, verified end stop to end stop on hardware
# (scope_dump/captures_wireshark/mso5202d-timediv.pcapng) and anchored against the on-screen
# readings (8 ns, 80 ns, 800 ns, ... — NOT a 1-2-4/1-2-5 sequence).
# HORIZ-WIN-TB follows the time/div knob over the full 0..31; HORIZ-TB (the
# real acquisition timebase) clamps at 6 (200 ns/div) — the 2..80 ns settings
# are zoom/interpolation.
TB_TO_NS = {
    0: 2, 1: 4, 2: 8, 3: 20, 4: 40, 5: 80, 6: 200, 7: 400,
    8: 800, 9: 2_000, 10: 4_000, 11: 8_000, 12: 20_000, 13: 40_000,
    14: 80_000, 15: 200_000, 16: 400_000, 17: 800_000, 18: 2_000_000,
    19: 4_000_000, 20: 8_000_000, 21: 20_000_000, 22: 40_000_000,
    23: 80_000_000, 24: 200_000_000, 25: 400_000_000, 26: 800_000_000,
    27: 2_000_000_000, 28: 4_000_000_000, 29: 8_000_000_000,
    30: 20_000_000_000, 31: 40_000_000_000,
}

# Trigger enums -> labels (mapped by stepping each menu on hardware, see
# MSO5202D-protocol.md §6). All verified twice.
TRIG_STATE_NAMES = {   # 1 = WAIT resolved via Normal-mode-no-trigger
    # official on-screen labels: STOP/Ready/AUTO/Trig'd/Scan/Astop/Armed (0..6)
    0: 'STOP', 1: 'WAIT', 2: 'AUTO', 3: "TRIG'D", 4: 'SCAN', 5: 'SINGLE', 6: 'ARMING',
}
TRIG_TYPE_NAMES = {
    0: 'Edge', 1: 'Video', 2: 'Pulse', 3: 'Slope', 4: 'Overtime', 5: 'Alter',
}
# Source list is restricted per trigger type: Edge = all 5; Video/Pulse/Slope =
# CH1/CH2/EXT/EXT-5 (no AC line); Overtime = CH1/CH2 only.
TRIG_SRC_NAMES = {0: 'CH1', 1: 'CH2', 2: 'EXT', 3: 'EXT/5', 4: 'AC line'}
TRIG_MODE_NAMES = {0: 'Auto', 1: 'Normal'}
TRIG_SLOPE_NAMES = {0: 'Rising', 1: 'Falling'}
TRIG_COUP_NAMES = {0: 'DC', 1: 'AC', 2: 'Noise Rej', 3: 'HF Rej', 4: 'LF Rej'}
# Video-type sub-params (TRIG-VIDEO-*).
TRIG_VIDEO_NEG_NAMES = {0: 'Normal', 1: 'Inverted'}
TRIG_VIDEO_STD_NAMES = {0: 'NTSC', 1: 'PAL/SECAM'}      # TRIG-VIDEO-PAL
TRIG_VIDEO_SYN_NAMES = {
    0: 'All Lines', 1: 'Line Num', 2: 'Odd Field', 3: 'Even Field', 4: 'All Fields',
}   # when SYN=1 (Line Num), TRIG-VIDEO-LINE = selected line number
    # (1..525 for NTSC; PAL/SECAM would be 1..625)

# Slope-type sub-params (TRIG-SLOPE-*).
TRIG_SLOPE_SET_NAMES = {0: 'Positive', 1: 'Negative'}   # slope direction
TRIG_SLOPE_WIN_NAMES = {0: 'V1', 1: 'V2', 2: 'Both'}    # which threshold the knob adjusts
# TRIG-SLOPE-V1/V2: two thresholds, signed 1/25-div; TRIG-SLOPE-TIME: ps (20ns..10s)

# Pulse-type sub-params (TRIG-PULSE-*). TRIG-PULSE-TIME: ps (20ns..10s width).
TRIG_PULSE_NEG_NAMES = {0: 'Positive', 1: 'Negative'}   # pulse polarity

# Overtime-type sub-params (TRIG-OVERTIME-*). TRIG-OVERTIME-TIME: ps (20ns..10s).
TRIG_OVERTIME_NEG_NAMES = {0: 'Positive', 1: 'Negative'}

# Alter/Swap: each channel has its OWN trigger config in the TRIG-SWAP-CHx-*
# block. TRIG-SWAP-CHx-TYPE is a 4-value enum (no Slope/Alter, unlike the
# main 6-value TRIG-TYPE). Its sub-params reuse the main-trigger enums
# (SWAP-*-EDGE-SLOPE=TRIG_SLOPE_NAMES, -COUP=TRIG_COUP_NAMES, -VIDEO-*, -PULSE-*,
# -OVERTIME-*, -MODE=TRIG_MODE_NAMES).
TRIG_SWAP_TYPE_NAMES = {0: 'Edge', 1: 'Video', 2: 'Pulse', 3: 'Overtime'}

# Shared "when" condition enum for Slope (TRIG-SLOPE-WHEN) and Pulse
# (TRIG-PULSE-WHEN) — verified identical on both.
TRIG_WHEN_NAMES = {0: '=', 1: '≠', 2: '>', 3: '<'}

# Vertical (CHx) menu enums.
VERT_COUP_NAMES = {0: 'DC', 1: 'AC', 2: 'GND'}           # VERT-CHx-COUP (NOT trigger coup)
VERT_BW_NAMES = {0: 'Full', 1: '20MHz'}                  # VERT-CHx-20MHZ (BW limit)
VERT_FINE_NAMES = {0: 'Coarse', 1: 'Fine'}               # VERT-CHx-FINE (V/div resolution)
VERT_PROBE_NAMES = {0: '1x', 1: '10x', 2: '100x', 3: '1000x'}  # VERT-CHx-PROBE
VERT_INVERT_NAMES = {0: 'Off', 1: 'On'}                  # VERT-CHx-RPHASE (invert)

# Acquire menu enums (ACQURIE-* fields).
ACQ_TYPE_NAMES = {0: 'Realtime', 1: 'Equ-time'}          # ACQURIE-TYPE
ACQ_MODE_NAMES = {0: 'Normal', 1: 'Peak', 2: 'Average'}  # ACQURIE-MODE
# ACQURIE-AVG-CNT: index -> number of averages (count = 4 << index = 2^(idx+2)).
ACQ_AVG_COUNTS = {0: 4, 1: 8, 2: 16, 3: 32, 4: 64, 5: 128}
# ACQURIE-STORE-DEPTH: record length. Codes are gapped because unavailable
# depths (greyed out in the current mode) still occupy enum slots. 0=4K, 4=40K,
# 6=512K (all channels), 7=1M (single-channel only; captured with one channel
# on). The gaps (1/2/3/5) are the greyed-out depths (e.g. 20K).
ACQ_DEPTH_NAMES = {0: '4K', 4: '40K', 6: '512K', 7: '1M'}

# Math menu enums (MATH-* fields).
MATH_MODE_NAMES = {
    0: 'CH1+CH2', 1: 'CH1-CH2', 2: 'CH2-CH1', 3: 'CH1*CH2',
    4: 'CH1/CH2', 5: 'CH2/CH1', 6: 'FFT',
}
MATH_FFT_SRC_NAMES = {0: 'CH1', 1: 'CH2'}
# FFT window: 0/1/2 verified; 3/4 inferred (only Hanning/Flattop/Rect swept).
MATH_FFT_WIN_NAMES = {
    0: 'Hanning', 1: 'Flattop', 2: 'Rectangular', 3: 'Bartlett', 4: 'Blackman',
}
MATH_FFT_FACTOR_NAMES = {0: 'x1', 1: 'x2', 2: 'x5', 3: 'x10'}  # FFT (horizontal) zoom
MATH_FFT_DB_NAMES = {0: '1dB', 1: '2dB', 2: '5dB', 3: '10dB', 4: '20dB'}  # FFT vertical dB/div
# Selecting FFT (MATH-MODE=6) sets DISPLAY-FORMAT=2. In FFT the frequency axis
# tracks the timebase/sample rate (slowest 5 S/s -> 250 mHz resolution).

# Display menu enums (DISPLAY-* fields).
DISPLAY_MODE_NAMES = {0: 'Vectors', 1: 'Dots'}           # DISPLAY-MODE (draw type)
DISPLAY_FORMAT_NAMES = {0: 'XT', 1: 'XY', 2: 'FFT'}      # DISPLAY-FORMAT (2=FFT set by MATH FFT)
DISPLAY_GRID_NAMES = {0: 'Off', 1: 'Dotted', 2: 'RealLine'}  # DISPLAY-GRID-KIND (order inferred)
# DISPLAY-PERSIST: gapped codes -> persistence time (label). 0=Auto..19=Infinity.
DISPLAY_PERSIST_NAMES = {
    0: 'Auto', 2: '0.2s', 4: '0.4s', 8: '0.8s', 10: '1.0s', 11: '2.0s',
    13: '4.0s', 17: '8.0s', 19: 'Infinity',
}
# DISPLAY-CONTRAST and DISPLAY-GRID-BRIGHT are 0..15 intensity (max = the
# DISPLAY-MAXCONTRAST / DISPLAY-MAXGRID-BRIGHT fields, both 15).

# MEASURE-ITEM1..8 = the 8 measurement slots; each has a -SRC (source) and a
# type id. Mapped 2026-07-09 (scope_dump/captures_wireshark/mso5202d-measure.pcapng) by sweeping
# MEASURE-ITEM8 through the on-screen list; scope-labelled. 0 = Off = empty slot.
MEASURE_SRC_NAMES = {0: 'CH1', 1: 'CH2', 3: 'LA'}  # only CH1/CH2/LA; id 2 is skipped/unused (no Math source)
MEASURE_TYPE_NAMES = {
    0: 'Off', 1: 'Frequency', 2: 'Period', 3: 'Mean', 4: 'Pk-Pk',
    5: 'Cyc RMS', 6: 'Minimum', 7: 'Maximum', 8: 'Rise Time', 9: 'Fall Time',
    10: 'Pos Width', 11: 'Neg Width', 12: 'Delay1-2 Rise', 13: 'Delay1-2 Fall',
    14: '+Duty', 15: '-Duty', 16: 'Vbase', 17: 'Vtop', 18: 'Vmid', 19: 'Vamp',
    20: 'Overshoot', 21: 'Preshoot', 22: 'Period Mean', 23: 'Period RMS',
    24: 'FOvershoot', 25: 'RPreshoot', 26: 'Burst Width', 27: 'FRF', 28: 'FFR',
    29: 'LRR', 30: 'LRF', 31: 'LFR',
}

# Logic analyzer. LA-CHANNEL-STATE = D0..D15 enable bitmask, bit N = D(N)
# (D0 = LSB, all-on = 0xFFFF; low byte = D0-D7 group, high byte = D8-D15).
# LA-CURRENT-CHANNEL = selected channel 0..15. Threshold is per 8-ch group.
# Mapped 2026-07-09 (scope_dump/captures_wireshark/mso5202d-la-{d7-d0,d15-d8,threshold}.pcapng).
LA_THRESHOLD_TYPE_NAMES = {0: 'TTL', 1: 'CMOS', 2: 'ECL', 3: 'User'}  # LA-Dxx-THRESHOLD-TYPE
LA_THRESHOLD_DAC = 4096.0   # LA user threshold: volts = raw / 4096 (±8 V, 12-bit DAC = code<<4)

# CONTROL-MENUID -> which on-screen menu is shown (mapped by context across
# captures; see MSO5202D-protocol.md §6). Partial — more menus to identify.
# Trigger sub-menus that span two pages have consecutive ids (page1, page2).
MENU_NAMES = {
    1: 'CH1 (vertical)', 2: 'CH2 (vertical)', 3: 'Horizontal p1',
    5: 'Trig:Edge', 6: 'Trig:Pulse p1', 7: 'Trig:Pulse p2',
    8: 'Trig:Video', 10: 'default/none', 11: 'Trigger', 17: 'Acquire',
    22: 'Trig:Slope p1',
    23: 'Trig:Slope p2', 24: 'Trig:Alter', 38: 'Trig:Overtime p1',
    39: 'Trig:Overtime p2',
    # Alter/Swap per-type submenus: CH1 block 26-29, CH2 block 30-33.
    26: 'Alter-CH1:Edge', 27: 'Alter-CH1:Pulse', 28: 'Alter-CH1:Video',
    29: 'Alter-CH1:Overtime', 30: 'Alter-CH2:Edge', 31: 'Alter-CH2:Pulse',
    32: 'Alter-CH2:Video', 33: 'Alter-CH2:Overtime',
    40: 'Horizontal p2',
    61: 'Logic Analyzer', 62: 'LA config (D7-D0 group)', 63: 'LA config (D15-D8 group)',
    4: 'Display (Type/Persist/Contrast)', 36: 'Display (Grid/Format)',
    15: 'Cursor', 41: 'Math', 16: 'Math:FFT p1', 56: 'Math:FFT p2',
    # Save/Recall (Storage) — action/UI menu, NO settings-blob params (like
    # Cursor). 47 = base type-selector; 48 is shared by CSV and its FileList
    # file-browser. Mapped 2026-07-09 by ordered-open poll of CONTROL-MENUID.
    47: 'Save/Recall', 19: 'Save/Recall:REF', 18: 'Save/Recall:SETUP',
    48: 'Save/Recall:CSV/FileList',
    # Utility — view-only, NO settings-blob params. 3 pages cycle
    # 42(p1: sys-status/update-fw/save-wave/self-cal) -> 43(p2) -> 10(p3);
    # page 3 reuses the generic id 10 (='default/none'). Mapped 2026-07-09.
    42: 'Utility p1', 43: 'Utility p2',   # Utility p3 shares id 10
    # Measure — DOES populate the blob (MEASURE-ITEM1..8 + -SRC). Mapped
    # 2026-07-09. 20 = base; 21 = item add/config submenu (toggles 20<->21).
    20: 'Measure', 21: 'Measure:config',
    25: 'Default Setup',   # factory reset — resets all params (verified 2026-07-09)
}

# Horizontal sample density. The vendor spec gives "sample interval = s/div /
# 200" (i.e. 200 samples per division); confirmed to the digit against our own
# cal signal (500 samples/period at 400 us/div, 1 kHz). A waveform block is
# 3840 samples = 19.2 divisions.
SAMPLES_PER_DIV = 200


def decode_la(raw: bytes) -> list:
    """Decode a raw LA block from read_la() into a list of 16-bit sample words
    (little-endian). Each word packs all 16 digital channels: **bit N = D(N)**
    (D0 = LSB … D15 = MSB), matching LA-CHANNEL-STATE. Returns [] if empty.
    To get one channel's 0/1 trace: `[(w >> n) & 1 for w in decode_la(raw)]`."""
    n = len(raw) // 2
    return list(struct.unpack(f'<{n}H', raw[:2 * n])) if n else []


def parse_wavedata_csv(text) -> dict:
    """Parse a front-panel Save→CSV deep-capture export (MSO5202D-protocol.md §7.5).

    These are the files the instrument writes to /mnt/udisk/WaveData<n>.csv when you
    Save/Recall → CSV; reading one back over USB (read_file, selector 0x10) is the ONLY
    route to the deep 40K/512K record — the acquire command only ever serves the
    ≤3840-sample screen block. One Source per file (CH1/CH2/**LA**). Two layouts:

    ANALOG (Source=CH1/CH2):                LOGIC ANALYZER (Source=LA):
        #timebase=<n>(ns)                       #timebase=<n>(ns)
        ,#voltbase=<n>(mv/100)  µV/div          ,#threshold=<n>(mv)   LA threshold, mV
        #size=<N>                               #size=<N>
        <t_s>,<volts>   value = VOLTS           <t_s>,<word>   value = 16-bit LA word,
                                                               bit N = channel D(N)

    Returns {'is_la', 'time_s', 'dt_s', 'size', 'timebase_ns'} plus, for analog,
    {'volts', 'vdiv_mV'} and for LA {'words' (uint16 array), 'threshold_mV'} (the analog
    keys are None for LA and vice-versa). Accepts str or bytes. The LA value column is
    the digital pod word — verified as real 16-ch data (per-bit toggle rates match the
    ESP frequency ladder D0→D15, 2026-07-11). Analog verified vs WaveData141{0,1,2}.csv."""
    import re
    import numpy as np
    if isinstance(text, (bytes, bytearray)):
        text = text.decode('latin1', 'replace')
    hdr, last = {}, -1
    lines = text.splitlines()
    for i, ln in enumerate(lines):
        m = re.search(r'#\s*(timebase|voltbase|threshold|size)\s*=\s*(-?\d+)', ln)
        if m:
            hdr[m.group(1)] = int(m.group(2))
            last = i
    body = '\n'.join(lines[last + 1:]).strip() if last >= 0 else text.strip()
    if body:
        flat = np.fromstring(body.replace('\n', ','), sep=',')
        n = flat.size - (flat.size % 2)
        arr = flat[:n].reshape(-1, 2)
    else:
        arr = np.empty((0, 2))
    time_s, value = arr[:, 0], arr[:, 1]
    dt = float(np.median(np.diff(time_s))) if len(time_s) > 1 else None
    is_la = 'threshold' in hdr
    out = {'is_la': is_la, 'time_s': time_s, 'dt_s': dt,
           'size': hdr.get('size', len(arr)), 'timebase_ns': hdr.get('timebase'),
           'volts': None, 'vdiv_mV': None, 'words': None, 'threshold_mV': None}
    if is_la:
        out['words'] = np.clip(np.round(value), 0, 0xFFFF).astype(np.uint16)  # 16-bit LA word
        out['threshold_mV'] = hdr['threshold']
    else:
        out['volts'] = value
        out['vdiv_mV'] = (hdr['voltbase'] / 1000.0) if 'voltbase' in hdr else None
    return out


def decode_settings(payload: bytes) -> dict:
    """Decode a settings payload from read_settings() (0x81 echo + 213 param
    bytes) into named /protocol.inf fields, plus derived 'CH1-VDIV-mV' /
    'CH2-VDIV-mV' (None if the VB index is unknown)."""
    if len(payload) != 214 or payload[0] != 0x81:
        raise ValueError(f"not a settings payload: len={len(payload)} "
                         f"first={payload[:1].hex()}")
    d, off = {}, 1                       # params start right after the echo
    for name, width in SETTINGS_PARAMS:
        d[name] = int.from_bytes(payload[off:off+width], 'little',
                                 signed=name in _SIGNED)
        off += width
    d['CH1-VDIV-mV'] = VB_TO_MV.get(d['VERT-CH1-VB'])
    d['CH2-VDIV-mV'] = VB_TO_MV.get(d['VERT-CH2-VB'])
    d['TDIV-ns'] = TB_TO_NS.get(d['HORIZ-WIN-TB'])       # knob / displayed
    d['TDIV-ACQ-ns'] = TB_TO_NS.get(d['HORIZ-TB'])       # real acquisition TB
    # Horizontal calibration: 200 samples/div (spec, hw-confirmed).
    tdiv = d['TDIV-ns']
    d['SAMPLE-INTERVAL-ns'] = tdiv / SAMPLES_PER_DIV if tdiv else None
    d['SAMPLERATE-HZ'] = SAMPLES_PER_DIV / (tdiv * 1e-9) if tdiv else None
    # Trigger level & slope thresholds (volts) = (field - source POS) * vdiv / 25,
    # since the position/level fields are in 1/25-division units (verified vs
    # scope for the level, and for the slope V1/V2 at CH1 5V/div: +12V/-36V).
    src = d['TRIG-SRC']
    if src in (0, 1):
        vdiv = d['CH1-VDIV-mV'] if src == 0 else d['CH2-VDIV-mV']
        pos = d['VERT-CH1-POS'] if src == 0 else d['VERT-CH2-POS']
        volt = lambda f: (f - pos) * vdiv / DIV_UNIT if vdiv else None
        d['TRIG-LEVEL-mV'] = volt(d['TRIG-VPOS'])
        d['TRIG-SLOPE-V1-mV'] = volt(d['TRIG-SLOPE-V1'])
        d['TRIG-SLOPE-V2-mV'] = volt(d['TRIG-SLOPE-V2'])
    else:
        d['TRIG-LEVEL-mV'] = d['TRIG-SLOPE-V1-mV'] = d['TRIG-SLOPE-V2-mV'] = None
    # LA user threshold volts: signed int16, 12-bit DAC stored as code<<4 (all
    # values %16==0); volts = raw / 4096, ±8 V full-scale (step ≈ 3.9 mV).
    # Verified 2026-07-09 (scope_dump/captures_wireshark/mso5202d-la-threshold.pcapng). Only
    # meaningful when the group's THRESHOLD-TYPE = 3 (User).
    d['LA-D7-D0-THRESHOLD-V'] = d['LA-D7-D0-USER-THRESHOLD-VOLT'] / LA_THRESHOLD_DAC
    d['LA-D15-D8-THRESHOLD-V'] = d['LA-D15-D8-USER-THRESHOLD-VOLT'] / LA_THRESHOLD_DAC
    return d


if __name__ == '__main__':
    s = Scope()
    print("/protocol.inf head:\n", s.read_file('/protocol.inf').decode('latin1')[:200])
    d = decode_settings(s.read_settings())
    print("settings:", {k: d[k] for k in (
        'VERT-CH1-DISP', 'VERT-CH1-VB', 'CH1-VDIV-mV', 'VERT-CH2-DISP',
        'CH2-VDIV-mV', 'TRIG-STATE', 'TRIG-VPOS', 'TRIG-LEVEL-mV',
        'TRIG-FREQUENCY', 'HORIZ-TB', 'TDIV-ns', 'SAMPLERATE-HZ')})
    w = s.read_waveform(0)
    print(f"waveform: {len(w)} samples, min={min(w)} max={max(w)}")
    s.close()
