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
import sys, struct, time, threading
import usb.core, usb.util

VID, PID = 0x049F, 0x505A
EP_OUT, EP_IN = 0x02, 0x81

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
        bytes in _rx cascade into repeated failures."""
        self._rx.clear()
        for _ in range(8):
            try:
                self.dev.read(EP_IN, 512, timeout=120)
            except usb.core.USBError:
                break   # nothing more pending

    def _transact_once(self, payload: bytes, timeout) -> bytes:
        out = {}
        def reader():
            try: out['f'] = self._recv(timeout)
            except Exception as e: out['e'] = e
        t = threading.Thread(target=reader, daemon=True); t.start()
        time.sleep(0.03)                                  # let the IN read get posted
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
    def read_file(self, path: str) -> bytes:
        frame = self.transact(b'\x10\x00' + path.encode())
        data = frame[2:] if len(frame) > 1 and frame[1] == 0x01 else b''
        try: self._recv(1000)          # discard end-marker (subtype 0x02)
        except Exception: pass
        return data

    def read_settings(self) -> bytes:
        """poll selector 0x01 -> settings-state blob. Returns a byte string on
        which the documented RAW offsets (status@5, timebase@31, vdiv@159/160) apply."""
        p = self.transact(b'\x01')      # payload = 81 01 <data...>
        return b'\x53\x00\x00' + p      # re-prefix dummy SOF+len so raw offsets match

    def read_waveform(self, ch=0, retries=2) -> bytes:
        # The acquire is a multi-frame sequence; retry the whole thing (with a
        # resync) if it times out or returns no data frame, so transient USB
        # timeouts self-heal instead of surfacing to the caller.
        for _ in range(retries + 1):
            try:
                self.transact(bytes([0x12, 0x01, ch]))            # select (ack)
                frame = self.transact(bytes([0x02, 0x01, 0x00]))  # acquire -> size(00)
                data = b''
                for _ in range(5):
                    st = frame[1] if len(frame) > 1 else 0xFF
                    if st == 0x01: data = frame[3:]
                    elif st == 0x02: break
                    frame = self._recv(2000)
                if data:
                    return data
            except Exception:
                pass
            self._resync()
        return b''


def decode_settings(raw: bytes) -> dict:
    # NOTE: these are EMPIRICAL raw-frame offsets (frame begins 53 d7 00 81 01).
    # The exact field<->offset alignment vs /protocol.inf is NOT fully resolved
    # (there is an unmodeled prefix in the blob) — the names below are provisional.
    # See docs/MSO5202D-protocol.md §6 and §8.
    def u(o, n): return int.from_bytes(raw[o:o+n], 'little')
    g = lambda o: raw[o] if len(raw) > o else None
    return {
        'status':    g(5),
        'field24':   g(24),
        'trigpos':   struct.unpack_from('<h', raw, 29)[0] if len(raw) > 30 else None,
        'timebase':  u(31, 3) if len(raw) > 33 else None,
        'vdiv_ch1':  g(159),
        'vdiv_ch2':  g(160),
    }


if __name__ == '__main__':
    s = Scope()
    print("/protocol.inf head:\n", s.read_file('/protocol.inf').decode('latin1')[:200])
    print("settings:", decode_settings(s.read_settings()))
    w = s.read_waveform(0)
    print(f"waveform: {len(w)} samples, min={min(w)} max={max(w)}")
    s.close()
