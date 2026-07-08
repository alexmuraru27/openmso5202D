#!/usr/bin/env python3
"""
MSO5202D driver / demo — implements the reverse-engineered USB protocol
(see MSO5202D-protocol.md) and drives the real device on Linux.

Confirmed working recipe (2026-07): detach cdc_subset -> dev.reset() -> re-detach
-> claim -> clear_halt -> transact with the bulk IN read posted BEFORE the OUT
write (the device only replies when an IN transfer is already pending).

    pip install pyusb ; sudo python3 mso5202d_probe.py

Endpoints: OUT 0x02, IN 0x81 (bulk).  Frame: 0x53 | len_LE16(=framelen-3) |
payload | checksum(sum&0xFF).  Response echoes selector | 0x80.
"""
import sys, struct, time, threading
import usb.core, usb.util

VID, PID = 0x049F, 0x505A
EP_OUT, EP_IN = 0x02, 0x81

# --- framing ---------------------------------------------------------------
def build(payload: bytes) -> bytes:
    hdr = b'\x53' + struct.pack('<H', len(payload) + 1) + payload
    return hdr + bytes([sum(hdr) & 0xFF])

def verify(frame: bytes) -> bytes:
    """Validate a full frame; return payload (selectorEcho | subtype | data...)."""
    if len(frame) < 5 or frame[0] != 0x53:
        raise ValueError(f"bad SOF: {frame[:8].hex()}")
    length = struct.unpack_from('<H', frame, 1)[0]
    if length != len(frame) - 3:
        raise ValueError(f"length field={length} actual={len(frame)-3}")
    if (sum(frame[:-1]) & 0xFF) != frame[-1]:
        raise ValueError("checksum mismatch")
    return frame[3:-1]

# --- transport -------------------------------------------------------------
class Scope:
    def __init__(self):
        self._rx = bytearray()          # persistent leftover bytes across frames
        self.dev = usb.core.find(idVendor=VID, idProduct=PID)
        if self.dev is None:
            sys.exit("MSO5202D (049f:505a) not found — is it plugged in?")
        self._detach()
        try:
            self.dev.reset()
            print("[+] usb reset")
            time.sleep(1.0)
            self._detach()
        except usb.core.USBError as e:
            print(f"[!] reset: {e}")
        usb.util.claim_interface(self.dev, 0)
        for ep in (EP_OUT, EP_IN):
            try: self.dev.clear_halt(ep)
            except usb.core.USBError: pass

    def _detach(self):
        try:
            if self.dev.is_kernel_driver_active(0):
                self.dev.detach_kernel_driver(0)
                print("[+] detached cdc_subset")
        except (NotImplementedError, usb.core.USBError) as e:
            print(f"[!] detach: {e}")

    def _recv(self, timeout) -> bytes:
        # Pull exactly one frame from the persistent buffer, reading more as
        # needed and keeping any overflow bytes for the next frame.
        while len(self._rx) < 3:
            self._rx += bytes(self.dev.read(EP_IN, 512, timeout=timeout))
        total = struct.unpack_from('<H', self._rx, 1)[0] + 3
        while len(self._rx) < total:
            self._rx += bytes(self.dev.read(EP_IN, 512, timeout=timeout))
        frame = bytes(self._rx[:total])
        del self._rx[:total]
        return verify(frame)

    def transact(self, payload: bytes, timeout=3000) -> bytes:
        """Post IN read first (thread), then write OUT; return the response payload."""
        out = {}
        def reader():
            try: out['f'] = self._recv(timeout)
            except Exception as e: out['e'] = e
        t = threading.Thread(target=reader, daemon=True)
        t.start()
        time.sleep(0.03)
        self.dev.write(EP_OUT, build(payload), timeout=2000)
        t.join(timeout / 1000 + 1.5)
        if 'f' in out: return out['f']
        raise out.get('e', TimeoutError("no response"))

    # --- high-level ops ----------------------------------------------------
    def read_file(self, path: str) -> bytes:
        # A file read returns a content frame (subtype 0x01) then an end-marker
        # frame (subtype 0x02). Consume both so the stream stays aligned.
        frame = self.transact(b'\x10\x00' + path.encode())
        data = frame[2:] if frame[1] == 0x01 else b''
        try:
            self._recv(1000)                # discard the 0x02 end-marker
        except Exception:
            pass
        return data

    def read_settings(self) -> bytes:
        """poll selector 0x01 -> settings-state blob. Returns the RAW frame
        (0x53 d7 00 81 01 ...) so documented raw offsets apply."""
        p = self.transact(b'\x01')          # payload = 81 01 <data...>
        return b'\x53\x00\x00' + p           # re-prefix a dummy SOF+len -> raw offsets

    def read_waveform(self, ch=0) -> bytes:
        self.transact(bytes([0x12, 0x01, 0x00]))         # param 0x12=0 (run/hold?; ack)
        frame = self.transact(bytes([0x02, 0x01, ch]))   # acquire CHANNEL ch -> size frame
        data = b''
        for _ in range(5):                               # size(00) -> data(01) -> end(02)
            st = frame[1] if len(frame) > 1 else 0xff
            if st == 0x01:
                data = frame[3:]
            elif st == 0x02:
                break                                    # end-marker consumed; stream aligned
            try:
                frame = self._recv(2000)
            except Exception:
                break
        return data

# --- decode ----------------------------------------------------------------
def decode_settings(raw: bytes):
    # Key fields only, at their resolved raw-frame offsets: the blob is the
    # /protocol.inf parameter list starting at raw offset 4 (right after the
    # 0x81 echo — no subtype byte). Full decode lives in mso5202d.py; spec in
    # MSO5202D-protocol.md §6.
    def u(o, n, signed=False): return int.from_bytes(raw[o:o+n], 'little', signed=signed)
    return {
        'VERT-CH1-VB@5':    raw[5]  if len(raw) > 5   else None,   # 0=2mV..10=5V (10V wraps to 0)
        'TRIG-STATE@24':    raw[24] if len(raw) > 24  else None,
        'TRIG-VPOS@29':     u(29, 2, True) if len(raw) > 30 else None,
        'TRIG-FREQ@31':     u(31, 8) if len(raw) > 38 else None,   # mHz (1 kHz cal -> 1000000)
        'HORIZ-TB@159':     raw[159] if len(raw) > 159 else None,  # acq timebase idx (clamps at 6 = 200ns)
        'HORIZ-WIN-TB@160': raw[160] if len(raw) > 160 else None,  # knob idx 0..31 = 2ns..40s (1-2-4); table in mso5202d.py
    }

def preview(samples, width=120, rows=8):
    if not samples: return "(no data)"
    lo, hi = min(samples), max(samples)
    span = max(1, hi - lo)
    step = max(1, len(samples) // width)
    return ''.join('#' if (samples[i] - lo) * 2 >= span else '.'
                   for i in range(0, min(len(samples), width * step), step))

# --- main ------------------------------------------------------------------
def main():
    s = Scope()
    print("\n=== /protocol.inf (head) ===")
    print(s.read_file('/protocol.inf').decode('latin1')[:400])
    print("\n=== /keyprotocol.inf (head) ===")
    print(s.read_file('/keyprotocol.inf').decode('latin1')[:160])

    print("\n=== live settings ===")
    raw = s.read_settings()
    for k, v in decode_settings(raw).items():
        print(f"   {k:16s} = {v}")

    print("\n=== waveforms ===")
    for ch in (0, 1):
        w = s.read_waveform(ch)
        if w:
            lo, hi = min(w), max(w)
            mid = (lo + hi) / 2
            lows = [v for v in w if v < mid]; highs = [v for v in w if v >= mid]
            edges = sum(1 for i in range(1, len(w)) if (w[i-1] < mid) != (w[i] < mid))
            print(f"CH{ch+1}: {len(w)} samples  min={lo} max={hi}  "
                  f"low~{sum(lows)//max(1,len(lows))} high~{sum(highs)//max(1,len(highs))}  "
                  f"edges={edges} (~{edges//2} cycles)")
            print("   ", preview(w))
        else:
            print(f"CH{ch+1}: no data")
    usb.util.release_interface(s.dev, 0)
    print("\n[done]")

if __name__ == '__main__':
    main()
