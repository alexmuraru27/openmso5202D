#!/usr/bin/env python3
"""Capture ONLY the MSO5202D's USB traffic to a pcapng.

    python3 mso5202d_capture.py <seconds> <output.pcapng>

Why this exists: `usbmon` sees the whole USB bus, so a raw capture is full of
mouse/keyboard/other-peripheral packets — and the scope's device address changes
whenever it re-enumerates, so you can't hard-code a filter. This script finds the
scope (049f:505a) via pyusb to learn its current bus + device address, captures
that bus for <seconds> with tshark, then filters the result down to just the
scope's device. The output contains only Hantek MSO5202D frames.

Requirements (Linux):
  - Wireshark CLI (`tshark`, `dumpcap`) and pyusb.
  - Read access to the scope's usbmon device. One-time setup:
        sudo modprobe usbmon
        sudo usermod -aG wireshark "$USER"      # then log out/in (dumpcap perms)
        sudo setfacl -m u:"$USER":r /dev/usbmon<bus>   # <bus> = scope's bus number
    Or just run the whole thing as root (e.g. `sudo python3 ...`).

The scope's bus number is printed at startup; re-run setfacl for that bus if the
capture step reports a permission error.
"""
import argparse
import os
import subprocess
import sys
import tempfile
from shutil import which

VID, PID = 0x049F, 0x505A


def find_scope():
    """Return (bus, address) of the connected MSO5202D, or exit."""
    import usb.core
    dev = usb.core.find(idVendor=VID, idProduct=PID)
    if dev is None:
        sys.exit(f"MSO5202D ({VID:04x}:{PID:04x}) not found — is it plugged in?")
    return dev.bus, dev.address


def _run(cmd):
    return subprocess.run(cmd, stderr=subprocess.PIPE, stdout=subprocess.PIPE,
                          text=True)


def packet_count(pcap):
    """Number of frames in a pcapng (via tshark)."""
    r = _run(["tshark", "-r", pcap, "-T", "fields", "-e", "frame.number"])
    return sum(1 for line in r.stdout.splitlines() if line.strip())


def main():
    ap = argparse.ArgumentParser(
        description="Capture only MSO5202D (049f:505a) USB traffic to a pcapng.")
    ap.add_argument("seconds", type=int, help="capture duration in seconds")
    ap.add_argument("output", help="output .pcapng path")
    args = ap.parse_args()

    if not which("tshark"):
        sys.exit("tshark not found — install Wireshark CLI.")

    bus, addr = find_scope()
    iface = f"usbmon{bus}"
    print(f"[+] MSO5202D on bus {bus}, device {addr}  →  capturing {iface} "
          f"for {args.seconds}s")

    tmp = tempfile.NamedTemporaryFile(suffix=".pcapng", delete=False).name
    try:
        # 1) capture the whole bus (live capture needs dumpcap perms)
        cap = _run(["tshark", "-i", iface, "-a", f"duration:{args.seconds}",
                    "-w", tmp])
        if cap.returncode != 0:
            sys.exit("[!] capture failed:\n  " + cap.stderr.strip() +
                     f"\n  (need usbmon read access for bus {bus} + dumpcap "
                     "perms — see this script's header.)")

        # 2) keep only the scope's device (strips mouse/keyboard/etc.)
        filt = _run(["tshark", "-r", tmp, "-Y", f"usb.device_address == {addr}",
                     "-w", args.output])
        if filt.returncode != 0:
            sys.exit("[!] filter step failed:\n  " + filt.stderr.strip())
    finally:
        try:
            os.unlink(tmp)
        except OSError:
            pass

    n = packet_count(args.output)
    if n == 0:
        print(f"[!] warning: {args.output} has 0 packets — did the scope go idle,"
              " or re-enumerate mid-capture?")
    else:
        print(f"[+] saved {args.output}  ({n} scope packets)")


if __name__ == "__main__":
    main()
