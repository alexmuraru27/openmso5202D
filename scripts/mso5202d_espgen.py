#!/usr/bin/env python3
"""Host-side control for the esp_combo_gen test-signal generator.

The ESP32 sketch in scripts/esp_combo_gen/ drives a serial protocol (SPI/UART/I2C)
on the scope's CH1/CH2 plus 16 LA channels, with the protocol AND frequency now
switchable at runtime over its USB serial console (115200 8N1, one JSON reply per
line). This tool is the host end of that command API:

    python3 mso5202d_espgen.py status                 # current protocol + frequency + mode (JSON)
    python3 mso5202d_espgen.py reset                  # reboot the ESP to its power-on defaults
    python3 mso5202d_espgen.py capabilities           # every protocol + freq table + modes (JSON)
    python3 mso5202d_espgen.py set spi 2000000 continuous   # protocol + frequency + transmit mode
    python3 mso5202d_espgen.py set uart 115200 single       # (mode: single=framed / continuous=stream)
    python3 mso5202d_espgen.py burst 256              # bytes per transaction (fine-tune)
    python3 mso5202d_espgen.py gap 0                  # idle us between transactions (0=continuous)
    python3 mso5202d_espgen.py pattern ramp           # byte sequence: ramp (0,1,2,..) or prbs (hash)
    python3 mso5202d_espgen.py trigger 5 1            # send exactly 5 bytes from index 1 -> 1,2,3,4,5

The BYTE SEQUENCE is set by `pattern`:
  - `ramp` — 0x00, 0x01, 0x02, … wrapping at 0xFF. Human-readable; the classic test ramp.
  - `prbs` — a deterministic hash of the byte index. Every byte depends on its position, so
    a decode that is shifted or has dropped a byte is immediately visible (a shifted ramp
    still looks like a ramp). Both are reproducible; `prbs` is the better grading pattern.

The TRANSMIT MODE decides WHEN bytes go out:
  - `continuous` — a solid back-to-back stream (long bursts, no gap): fills the scope.
  - `single`     — framed bytes with idle gaps between them (decoder-friendly).
  - `triggered`  — SILENT until you ask: `trigger <n> [start]` sends exactly n bytes ONCE
                   (from pattern index `start`, default 0), then falls silent again. This is
                   what pairs with a scope single-sequence: arm the scope, then `trigger`.
                   e.g. `trigger 5 1` -> 1,2,3,4,5;  `trigger 256` -> the full 0x00..0xFF ramp.

Port is auto-detected (/dev/ttyUSB*, /dev/ttyACM*); override with --port.

The connection is deliberately NON-DISTURBING: it opens the tty with HUPCL cleared
and DTR/RTS held low, so opening the port does NOT reset the ESP32. This matters —
the ESP keeps generating whatever you last set even after the tool exits, and a
plain `status` query never wipes it. Pass --reset to force a reboot back to the
power-on defaults (pulses the auto-reset lines, then waits ~1.8 s).

Pure termios/ioctl — no pyserial needed. Linux only (as is the rest of the repo).
"""
from __future__ import annotations

import argparse
import fcntl
import glob
import json
import os
import select
import struct
import sys
import termios
import time
import tty

PROTOS = ("spi", "uart", "i2c")
PROTO_DESC = {
    "spi": "SCLK=CH1/GPIO22, MOSI=CH2/GPIO23, mode 0 MSB (HW peripheral)",
    "uart": "TX=CH1/GPIO22, 8N1, CH2 unused (HW peripheral)",
    "i2c": "SCL=CH1/GPIO22, SDA=CH2/GPIO23, self-ACK (bit-banged)",
}
MODES = ("single", "continuous", "triggered")
PATTERNS = ("ramp", "prbs")
MODE_DESC = {
    "single": "framed bytes with idle gaps — decoder-friendly (burst 1, auto gap)",
    "continuous": "solid near-gapless stream — for viewing (burst 64, gap 0)",
}
BAUD_CONST = termios.B115200

# ioctl requests / modem-control bits (Linux).
TIOCMGET = 0x5415
TIOCMSET = 0x5418
TIOCM_DTR = 0x002
TIOCM_RTS = 0x004


def find_port() -> str:
    ports = sorted(glob.glob("/dev/ttyUSB*") + glob.glob("/dev/ttyACM*"))
    if not ports:
        sys.exit("no serial port found (looked for /dev/ttyUSB*, /dev/ttyACM*); "
                 "pass --port")
    return ports[0]


class EspGen:
    """Non-disturbing line/JSON command channel to the esp_combo_gen firmware.

    Opens the tty without resetting the ESP32: HUPCL cleared + DTR/RTS held low so
    the open() causes no auto-reset pulse and the board's runtime state survives.
    """

    def __init__(self, port: str, reset: bool = False, timeout: float = 4.0):
        self.timeout = timeout
        self._rx = b""
        self.fd = os.open(port, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
        try:
            tty.setraw(self.fd)
            attrs = termios.tcgetattr(self.fd)
            # cflag (index 2): clear HUPCL so close/open doesn't drop the lines;
            # keep the receiver on and ignore modem status lines.
            attrs[2] = (attrs[2] & ~termios.HUPCL) | termios.CLOCAL | termios.CREAD
            attrs[4] = BAUD_CONST                     # ispeed
            attrs[5] = BAUD_CONST                     # ospeed
            termios.tcsetattr(self.fd, termios.TCSANOW, attrs)
            if reset:
                self._reset_pulse()
            self._set_modem(dtr=False, rts=False)     # rest state = no reset
            time.sleep(1.8 if reset else 0.15)
            termios.tcflush(self.fd, termios.TCIFLUSH)
        except Exception:
            os.close(self.fd)
            raise

    def _set_modem(self, dtr: bool, rts: bool):
        mb = struct.unpack("I", fcntl.ioctl(self.fd, TIOCMGET, struct.pack("I", 0)))[0]
        for bit, on in ((TIOCM_DTR, dtr), (TIOCM_RTS, rts)):
            mb = (mb | bit) if on else (mb & ~bit)
        fcntl.ioctl(self.fd, TIOCMSET, struct.pack("I", mb))

    def _reset_pulse(self):
        # Classic ESP32 auto-reset (run, not bootloader): EN low then high.
        self._set_modem(dtr=False, rts=True)          # EN asserted
        time.sleep(0.1)
        self._set_modem(dtr=False, rts=False)         # EN released -> boots app

    def close(self):
        try:
            os.close(self.fd)
        except Exception:
            pass

    def _readline(self, deadline: float):
        while b"\n" not in self._rx:
            remaining = deadline - time.time()
            if remaining <= 0:
                return None
            r, _, _ = select.select([self.fd], [], [], remaining)
            if not r:
                return None
            try:
                d = os.read(self.fd, 512)
            except (BlockingIOError, OSError):
                time.sleep(0.01)
                continue
            if d:
                self._rx += d
        line, _, self._rx = self._rx.partition(b"\n")
        return line

    def query(self, cmd: str, timeout: float | None = None) -> dict:
        """Send one command; return the first JSON object the board replies with.

        `timeout` overrides the default, for commands the board answers only after
        doing real work — `trigger` replies once the whole burst has been sent,
        which at a slow line rate takes seconds.
        """
        timeout = self.timeout if timeout is None else timeout
        deadline = time.time() + timeout
        self._rx = b""
        termios.tcflush(self.fd, termios.TCIFLUSH)
        os.write(self.fd, (cmd + "\n").encode())
        while time.time() < deadline:
            line = self._readline(deadline)
            if line is None:
                break
            s = line.decode(errors="replace").strip()
            if not s.startswith("{"):
                continue                      # skip boot-banner / non-JSON lines
            try:
                obj = json.loads(s)
            except json.JSONDecodeError:
                continue
            if isinstance(obj, dict):
                return obj
        raise TimeoutError(f"no JSON reply to {cmd!r} within {timeout:.1f}s")


def fmt_hz(hz) -> str:
    hz = float(hz)
    if hz >= 1e6:
        return f"{hz / 1e6:g} MHz"
    if hz >= 1e3:
        return f"{hz / 1e3:g} kHz"
    return f"{hz:g} Hz"


UNITS = {"spi": "SCLK", "uart": "baud", "i2c": "SCL"}


def build_capabilities(st: dict) -> dict:
    """Fold a status reply into a capabilities object: every protocol + its table."""
    tables = st.get("tables", {})
    freqs = st.get("freqs", {})
    protocols = {}
    for p in st.get("protos", PROTOS):
        t = tables.get(p, [])
        protocols[p] = {
            "desc": PROTO_DESC.get(p, ""),
            "unit": UNITS.get(p, "freq"),
            "min": t[0] if t else None,
            "max": t[-1] if t else None,
            "table": t,
            "last": freqs.get(p),
        }
    # Firmware reports the active transmit mode as "framed"; the user-facing name
    # for that preset is "single".
    active_mode = st.get("mode")
    if active_mode == "framed":
        active_mode = "single"
    modes = [{"name": m, "desc": MODE_DESC.get(m, "")} for m in MODES]
    return {"ok": True, "active": st.get("proto"), "active_mode": active_mode,
            "protocols": protocols, "modes": modes}


def running_settings(st: dict) -> dict:
    """The current running settings only (what the generator is doing right now).

    The frequency ladders / all-protocol info live in `capabilities`; this is just
    the live config. Firmware reports the active mode as "framed"; normalise to the
    user-facing "single".
    """
    mode = st.get("mode")
    if mode == "framed":
        mode = "single"
    return {
        "ok": st.get("ok", True),
        "proto": st.get("proto"),
        "freq": st.get("freq"),
        "freq_achieved": st.get("freq_achieved"),
        "mode": mode,
        "burst": st.get("burst"),
        "gap_us": st.get("gap_us"),
        "pattern": st.get("pattern"),
        "triggered": st.get("triggered"),
    }


def print_status(st: dict):
    """Human-readable view of the current running settings."""
    s = running_settings(st)
    proto = s.get("proto", "?")
    unit = UNITS.get(proto, "freq")
    cur = s.get("freq")
    print(f"protocol : {proto}")
    print(f"frequency: {fmt_hz(cur or 0)} ({cur} Hz {unit})")
    if s.get("freq_achieved") not in (None, cur):
        print(f"  applied: {fmt_hz(s['freq_achieved'])} "
              f"({s['freq_achieved']} Hz — bit-bang limited)")
    if s.get("burst") is not None:
        gap = s.get("gap_us", 0)
        gaptxt = "0 (continuous)" if gap == 0 else f"{gap} us"
        print(f"mode     : {s.get('mode', '?')}  (burst {s['burst']} B/txn, gap {gaptxt})")


def main():
    ap = argparse.ArgumentParser(
        description="Control the esp_combo_gen runtime protocol/frequency generator.")
    ap.add_argument("--port", help="serial port (default: auto-detect ttyUSB*/ttyACM*)")
    ap.add_argument("--reset", action="store_true",
                    help="reboot the ESP to power-on defaults before the command")
    ap.add_argument("--json", action="store_true",
                    help="print the raw JSON reply instead of a formatted view")
    sub = ap.add_subparsers(dest="cmd", required=True)

    sub.add_parser("status", help="print current protocol, frequency and mode as JSON")
    sub.add_parser("reset", help="reboot the ESP to its power-on default state")
    sub.add_parser("capabilities", help="print every protocol and its frequency "
                                        "table as JSON (switch with 'set')")
    p_set = sub.add_parser("set", help="set protocol, frequency and transmit mode")
    p_set.add_argument("proto", choices=PROTOS)
    p_set.add_argument("hz", type=int, help="frequency in Hz (snapped to the table)")
    p_set.add_argument("mode", choices=MODES,
                       help="single (framed bytes, decoder-friendly) or "
                            "continuous (solid stream)")
    p_burst = sub.add_parser("burst", help="bytes sent per transaction (1..256)")
    p_burst.add_argument("n", type=int)
    p_gap = sub.add_parser("gap", help="idle microseconds between transactions (0=continuous, or 'auto')")
    p_gap.add_argument("us", help="microseconds, or 'auto'")
    p_trig = sub.add_parser("trigger",
                            help="send exactly n bytes ONCE from the current pattern, then "
                                 "fall silent (also switches the generator to triggered mode)")
    p_trig.add_argument("n", type=int, help="bytes to send (1..8192)")
    p_trig.add_argument("start", type=int, nargs="?", default=0,
                        help="start index into the pattern (default 0). With pattern=ramp, "
                             "`trigger 5 1` sends 1,2,3,4,5; `trigger 5` sends 0,1,2,3,4. "
                             "Vary it per capture so runs cover different bytes.")
    p_pat = sub.add_parser("pattern", help="the byte sequence the generator sends")
    p_pat.add_argument("name", choices=PATTERNS,
                       help="ramp = 0x00,0x01,0x02,.. (wraps at 0xFF); "
                            "prbs = deterministic hash of the byte index (any shift/drop is "
                            "visible). Applies to every mode (continuous/single/triggered).")

    args = ap.parse_args()
    port = args.port or find_port()

    # A `reset` subcommand is just the --reset reboot followed by a status read.
    force_reset = args.reset or args.cmd == "reset"

    try:
        gen = EspGen(port, reset=force_reset)
    except OSError as e:
        sys.exit(f"cannot open {port}: {e}")

    try:
        if args.cmd == "capabilities":
            # Every protocol + its frequency table + the transmit modes, always JSON.
            print(json.dumps(build_capabilities(gen.query("status")), indent=2))
            return
        elif args.cmd == "status":
            # Current running settings only, always JSON.
            print(json.dumps(running_settings(gen.query("status")), indent=2))
            return
        elif args.cmd == "reset":
            reply = gen.query("status")
        elif args.cmd == "set":
            # One interface: protocol + frequency + mode. The firmware sets these
            # with two commands (set proto/freq, then mode); the second reply is
            # the final state.
            gen.query(f"set {args.proto} {args.hz}")
            reply = gen.query(f"mode {args.mode}")
        elif args.cmd == "burst":
            reply = gen.query(f"burst {args.n}")
        elif args.cmd == "gap":
            reply = gen.query(f"gap {args.us}")
        elif args.cmd == "trigger":
            # The firmware replies only once the whole burst has gone out, so this
            # returns when the transmission is complete — no guessed delay needed.
            reply = gen.query(f"trigger {args.n} {args.start}", timeout=30.0)
        elif args.cmd == "pattern":
            reply = gen.query(f"pattern {args.name}")
        else:
            reply = gen.query("status")
    except TimeoutError as e:
        sys.exit(f"error: {e} (is esp_combo_gen flashed and not held by another program?)")
    finally:
        gen.close()

    if args.json:
        print(json.dumps(reply, indent=2))
        return
    if not reply.get("ok", False):
        sys.exit(f"error: {reply.get('error', reply)}")
    print_status(reply)


if __name__ == "__main__":
    main()
